//! Embedded, loopback-only touch workbench. HTTP callers submit geometry, never scripts.
use super::*;
use crate::observation::Cancellation;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

const ADDRESS: &str = "127.0.0.1:43497";
const ORIGIN: &str = "http://127.0.0.1:43497";
const INDEX: &str = include_str!("../../web/index.html");
const EDITOR: &str = include_str!("../../web/zuse-touch.js");
const STYLE: &str = include_str!("../../web/zuse-touch.css");
const HOST: &str = include_str!("../../web/host.js");

#[derive(Default)]
struct Active {
    running: Option<(String, String, Cancellation)>,
    // The cancel POST can race ahead of the gesture POST on a second connection.
    early: std::collections::VecDeque<(String, String, Instant)>,
}
struct State {
    token: String,
    active: Mutex<Active>,
}
fn identifier(root: Value<'_>, field: &str) -> Result<String> {
    let value: String = root
        .get(field)
        .and_then(Value::string)
        .ok_or("missing instance or request ID")?
        .chars()
        .collect();
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid instance or request ID".into());
    }
    Ok(value)
}

struct Request {
    method: String,
    path: String,
    headers: std::collections::BTreeMap<String, String>,
    body: String,
}
fn http(socket: &mut TcpStream) -> Result<Request> {
    let mut reader = BufReader::new(socket);
    let mut header = Vec::new();
    loop {
        let count = BufRead::read_until(
            &mut Read::by_ref(&mut reader).take((8193 - header.len()) as u64),
            b'\n',
            &mut header,
        )
        .map_err(|e| e.to_string())?;
        if count == 0 || header.len() > 8192 {
            return Err("invalid or oversized HTTP header".into());
        }
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let mut lines = std::str::from_utf8(&header)
        .map_err(|_| "invalid HTTP header")?
        .split("\r\n");
    let mut first = lines.next().ok_or("missing HTTP method")?.split(' ');
    let method = first.next().unwrap_or("").to_owned();
    let path = first.next().unwrap_or("").to_owned();
    if first.next() != Some("HTTP/1.1") || first.next().is_some() {
        return Err("invalid HTTP request".into());
    }
    let mut headers = std::collections::BTreeMap::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let (k, v) = line.split_once(':').ok_or("invalid HTTP field")?;
        if headers
            .insert(k.to_ascii_lowercase(), v.trim().to_owned())
            .is_some()
        {
            return Err("duplicate HTTP field".into());
        }
    }
    if headers.get("host").map(String::as_str) != Some(ADDRESS)
        || headers.contains_key("transfer-encoding")
    {
        return Err("unexpected host or transfer framing".into());
    }
    let size = headers
        .get("content-length")
        .map(|s| s.parse::<usize>())
        .transpose()
        .map_err(|_| "invalid body size")?
        .unwrap_or(0);
    if size > 8192 {
        return Err("request body exceeds 8 KiB".into());
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(Request {
        method,
        path,
        headers,
        body: String::from_utf8(body).map_err(|_| "invalid body")?,
    })
}
fn response(s: &mut TcpStream, status: &str, mime: &str, body: &str) -> std::io::Result<()> {
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'self' http://127.0.0.1:42387\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn handle(s: &mut TcpStream, state: &State) -> Result<()> {
    let r = http(s)?;
    let h = |name: &str| r.headers.get(name).map(String::as_str);
    if r.method == "GET" {
        let (mime, body) = match r.path.as_str() {
            "/" | "/index.html" => ("text/html; charset=utf-8", INDEX),
            "/zuse-touch.js" => ("text/javascript; charset=utf-8", EDITOR),
            "/zuse-touch.css" => ("text/css; charset=utf-8", STYLE),
            "/host.js" => ("text/javascript; charset=utf-8", HOST),
            "/api/session" if h("x-zuse-workbench") == Some("1") => {
                return response(
                    s,
                    "200 OK",
                    "application/json",
                    &format!(
                        r#"{{"token":{},"android":{}}}"#,
                        quote(&state.token),
                        cfg!(target_os = "android")
                    ),
                )
                .map_err(|e| e.to_string());
            }
            _ => {
                return response(s, "404 Not Found", "text/plain", "Not found")
                    .map_err(|e| e.to_string());
            }
        };
        return response(s, "200 OK", mime, body).map_err(|e| e.to_string());
    }
    if r.method != "POST"
        || h("origin") != Some(ORIGIN)
        || h("x-zuse-token") != Some(state.token.as_str())
        || h("content-type") != Some("application/json")
    {
        return response(
            s,
            "403 Forbidden",
            "application/json",
            r#"{"ok":false,"error":"Same-origin workbench authorization required"}"#,
        )
        .map_err(|e| e.to_string());
    }
    let (key, instance) = with_json(&r.body, |v| {
        Ok((identifier(v, "key")?, identifier(v, "instance")?))
    })?;
    if r.path == "/api/cancel" {
        let mut active = state.active.lock().unwrap();
        if let Some((running, board, cancel)) = &active.running
            && running == &key
            && board == &instance
        {
            cancel.cancel();
        } else {
            if active.early.len() >= 32 {
                active.early.pop_front();
            }
            active.early.push_back((key, instance, Instant::now()));
        }
        return response(s, "200 OK", "application/json", r#"{"ok":true}"#)
            .map_err(|e| e.to_string());
    }
    if r.path != "/api/gesture" {
        return response(s, "404 Not Found", "application/json", "{}").map_err(|e| e.to_string());
    }
    let cancel = Cancellation::default();
    {
        let mut active = state.active.lock().unwrap();
        active
            .early
            .retain(|(_, _, time)| time.elapsed() < Duration::from_secs(10));
        if let Some(index) = active
            .early
            .iter()
            .position(|(k, b, _)| k == &key && b == &instance)
        {
            active.early.remove(index);
            return Err("The gesture was cancelled.".into());
        }
        if active.running.is_some() {
            return Err("A native gesture is already running.".into());
        }
        active.running = Some((key, instance.clone(), cancel.clone()));
    }
    let result = run_gesture(&r.body, &instance, &cancel);
    state.active.lock().unwrap().running = None;
    response(s, "200 OK", "application/json", &result?).map_err(|e| e.to_string())
}

/// Serve the embedded component until this foreground process is stopped.
pub fn serve() -> Result<()> {
    let listener = TcpListener::bind(ADDRESS).map_err(|e| format!("open {ADDRESS}: {e}"))?;
    let mut random = [0; 24];
    entropy(&mut random)?;
    let state = Arc::new(State {
        token: encode_base64(&random),
        active: Mutex::default(),
    });
    let count = Arc::new(AtomicUsize::new(0));
    println!("Zuse touch workbench: {ORIGIN}/");
    for stream in listener.incoming() {
        let mut s = stream.map_err(|e| e.to_string())?;
        if count.fetch_add(1, Ordering::Relaxed) >= 12 {
            count.fetch_sub(1, Ordering::Relaxed);
            continue;
        }
        let state = state.clone();
        let count = count.clone();
        thread::spawn(move || {
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            if let Err(e) = handle(&mut s, &state) {
                let _ = response(
                    &mut s,
                    "400 Bad Request",
                    "application/json",
                    &format!(r#"{{"ok":false,"error":{}}}"#, quote(&e)),
                );
            }
            count.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn run_gesture(_: &str, _: &str, _: &Cancellation) -> Result<String> {
    Err("Native workbench input requires Android Chrome.".into())
}

#[cfg(target_os = "android")]
fn select_board(instance: &str) -> Result<Browser> {
    use std::os::android::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr as UnixAddr, UnixStream};
    let addr = UnixAddr::from_abstract_name("chrome_devtools_remote").map_err(|e| e.to_string())?;
    let socket = || {
        UnixStream::connect_addr(&addr)
            .map_err(|e| format!("Cannot open the Android Chrome debugging socket: {e}"))
    };
    let list = discovery_json(&mut socket()?, "localhost", "/json/list")?;
    let ids = with_json(&list, |v| {
        let mut ids = Vec::new();
        for page in v.elements() {
            let matching = page.get("url").and_then(Value::string).is_some_and(|u| {
                u.matches("http://127.0.0.1:43497/")
                    || u.matches("http://127.0.0.1:43497/index.html")
            });
            if matching
                && page
                    .get("type")
                    .and_then(Value::string)
                    .is_some_and(|s| s.matches("page"))
            {
                let id: String = page
                    .get("id")
                    .and_then(Value::string)
                    .ok_or("Chrome returned no page ID")?
                    .chars()
                    .collect();
                if id.len() > 128
                    || id.is_empty()
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                {
                    return Err("Invalid page ID".into());
                }
                ids.push(id);
            }
        }
        if ids.len() > 8 {
            return Err("Close unused workbench tabs before trying again.".into());
        }
        Ok(ids)
    })?;
    let check = format!(
        "location.origin==={}&&document.querySelector('zuse-touch')?.dataset.zuseInstance==={}",
        quote(ORIGIN),
        quote(instance)
    );
    for id in ids {
        let ws = WebSocket::handshake(socket()?, "localhost", &format!("/devtools/page/{id}"))?;
        let mut browser = Browser::from_socket(ws, true, true)?;
        if browser.evaluate(&check)? == "true" {
            return Ok(browser);
        }
    }
    Err("Open this workbench directly in Android Chrome.".into())
}

#[cfg(target_os = "android")]
fn run_gesture(body: &str, instance: &str, cancel: &Cancellation) -> Result<String> {
    use crate::motion::Curve;
    let mut browser = select_board(instance)?;
    let rect=browser.evaluate("(()=>{const r=document.querySelector('zuse-touch').surfaceBounds();return {x:r.x,y:r.y,w:r.width,h:r.height}})()")?;
    let (x, y, w, h) = with_json(&rect, |v| {
        Ok((
            number(v.get("x"))?,
            number(v.get("y"))?,
            number(v.get("w"))?,
            number(v.get("h"))?,
        ))
    })?;
    if w < 8.0 || h < 8.0 {
        return Err("The touch surface is too small.".into());
    }
    let gesture = with_json(body, |root| {
        let v = root.get("request").ok_or("missing gesture")?;
        let integer = |k: &str, min: i64, max: i64| {
            v.get(k)
                .and_then(|n| n.integer().ok())
                .filter(|&n| (min..=max).contains(&n))
                .ok_or_else(|| format!("invalid {k}"))
        };
        integer("version", 1, 1)?;
        let duration = integer("duration_ms", 0, 10000)? as u64;
        let steps = integer("steps", 1, 120)? as u32;
        let seed = integer("seed", 0, i64::from(u32::MAX))? as u64;
        let curve = v
            .get("curve")
            .and_then(Value::string)
            .map(|s| s.chars().collect::<String>())
            .and_then(|s| Curve::parse(&s))
            .ok_or("invalid curve")?;
        let jitter = number(v.get("jitter"))?;
        let speed = v
            .get("speed_variation")
            .map(|n| number(Some(n)))
            .transpose()?
            .unwrap_or(0.0);
        let mut paths = Vec::new();
        for contact in v.get("contacts").ok_or("missing contacts")?.elements() {
            if paths.len() >= 10 {
                return Err("Use up to ten contacts.".into());
            }
            if contact.get("id").and_then(|n| n.integer().ok()) != Some(paths.len() as i64) {
                return Err("Contact IDs must be contiguous from zero.".into());
            }
            let axis = |k: &str, start: f64, extent: f64| -> Result<i64> {
                let n = number(contact.get(k))?;
                if !(0.0..=1.0).contains(&n) {
                    return Err("Contact coordinates must be in [0,1].".into());
                }
                Ok((start + n * (extent - 1.0)).round() as i64)
            };
            paths.push(format!(
                "{},{}:{},{}",
                axis("x0", x, w)?,
                axis("y0", y, h)?,
                axis("x1", x, w)?,
                axis("y1", y, h)?
            ));
        }
        touch::TouchGesture::parse(
            &paths.join(";"),
            duration,
            steps,
            curve,
            jitter,
            seed,
            speed,
        )
    })?;
    if !gesture.fits(x, y, w, h) {
        return Err("The path would leave the touch surface.".into());
    }
    let guard = format!(
        r#"(pts)=>{{const b=document.querySelector('zuse-touch'),c=b?.shadowRoot?.querySelector('#surface');if(location.origin!=={}||b?.dataset.zuseInstance!=={}||!c)return false;const r=c.getBoundingClientRect();return Math.abs(r.x-({x}))<0.1&&Math.abs(r.y-({y}))<0.1&&Math.abs(r.width-{w})<0.1&&Math.abs(r.height-{h})<0.1&&pts.every(p=>document.elementFromPoint(p.x,p.y)===b&&b.shadowRoot.elementFromPoint(p.x,p.y)===c)}}"#,
        quote(ORIGIN),
        quote(instance)
    );
    let started = Instant::now();
    browser.touch_checked(gesture, cancel, Some(&guard))?;
    Ok(format!(
        r#"{{"ok":true,"elapsed_ms":{},"backend":"zuse-android-chrome"}}"#,
        started.elapsed().as_millis()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(server: &Arc<State>, text: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = server.clone();
        let reader = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            if let Err(error) = handle(&mut socket, &server) {
                response(&mut socket, "400 Bad Request", "text/plain", &error).unwrap();
            }
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(text.as_bytes()).unwrap();
        let mut received = String::new();
        client.read_to_string(&mut received).unwrap();
        reader.join().unwrap();
        received
    }

    #[test]
    fn workbench_protects_the_board_and_remembers_early_cancellation() {
        let state = Arc::new(State {
            token: "test-token".into(),
            active: Mutex::default(),
        });
        let public = request(
            &state,
            &format!("GET / HTTP/1.1\r\nHost: {ADDRESS}\r\n\r\n"),
        );
        assert!(public.starts_with("HTTP/1.1 200 OK"));
        assert!(public.contains("frame-ancestors 'self' http://127.0.0.1:42387"));
        let no_session = request(
            &state,
            &format!("GET /api/session HTTP/1.1\r\nHost: {ADDRESS}\r\n\r\n"),
        );
        assert!(no_session.starts_with("HTTP/1.1 404"));
        let body = r#"{"key":"00000000000000000000000000000001","instance":"00000000000000000000000000000002"}"#;
        let post = |path: &str, origin: &str| {
            format!(
                "POST {path} HTTP/1.1\r\nHost: {ADDRESS}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nX-Zuse-Token: test-token\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        };
        assert!(
            request(&state, &post("/api/gesture", "http://127.0.0.1:42387"))
                .starts_with("HTTP/1.1 403")
        );
        assert!(request(&state, &post("/api/cancel", ORIGIN)).starts_with("HTTP/1.1 200"));
        assert!(request(&state, &post("/api/gesture", ORIGIN)).contains("gesture was cancelled"));
    }
}
