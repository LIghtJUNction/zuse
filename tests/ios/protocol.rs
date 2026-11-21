use super::*;
use std::{
    fs,
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
struct Request {
    method: String,
    path: String,
    body: String,
}

enum Reply {
    Bytes(Vec<u8>),
    Trickle(String),
}

struct Exchange {
    method: &'static str,
    path: &'static str,
    reply: Reply,
}

impl Exchange {
    fn json(method: &'static str, path: &'static str, body: &str) -> Self {
        Self {
            method,
            path,
            reply: Reply::Bytes(response(200, body)),
        }
    }
    fn raw(method: &'static str, path: &'static str, bytes: Vec<u8>) -> Self {
        Self {
            method,
            path,
            reply: Reply::Bytes(bytes),
        }
    }
}

struct Mock {
    endpoint: String,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Request>>>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl Mock {
    fn new(exchanges: Vec<Exchange>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop_thread = stop.clone();
        let seen_thread = seen.clone();
        let worker = thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            for expected in exchanges {
                let until = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    if stop_thread.load(Ordering::Relaxed) {
                        return Err("mock stopped before expected request".into());
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= until { return Err("mock request was not received".into()); }
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                stream.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
                let request = read_request(&mut stream)?;
                if request.method != expected.method || request.path != expected.path {
                    return Err(format!("expected {} {}, received {} {}", expected.method, expected.path, request.method, request.path));
                }
                seen_thread.lock().unwrap().push(request);
                match expected.reply {
                    Reply::Bytes(bytes) => { let _ = stream.write_all(&bytes); }
                    Reply::Trickle(body) => {
                        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                        stream.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
                        for byte in body.bytes() {
                            if stop_thread.load(Ordering::Relaxed) || stream.write_all(&[byte]).is_err() { break; }
                            thread::sleep(Duration::from_millis(25));
                        }
                    }
                }
            }
            Ok(())
        }).unwrap();
        Self {
            endpoint,
            stop,
            seen,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> Vec<Request> {
        self.worker.take().unwrap().join().unwrap().unwrap();
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() > 65536 {
            return Err("mock request headers too large".into());
        }
        let mut block = [0; 1024];
        let count = stream.read(&mut block).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("mock incomplete request".into());
        }
        bytes.extend_from_slice(&block[..count]);
    };
    let head = std::str::from_utf8(&bytes[..header_end]).map_err(|e| e.to_string())?;
    let mut line = head.lines().next().unwrap().split_whitespace();
    let method = line.next().unwrap().to_owned();
    let path = line.next().unwrap().to_owned();
    let length = head
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(key, value)| {
                key.eq_ignore_ascii_case("Content-Length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
        })
        .unwrap_or(0);
    if length > 65536 {
        return Err("mock request body too large".into());
    }
    while bytes.len() < header_end + length {
        let mut block = [0; 1024];
        let count = stream.read(&mut block).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("mock incomplete body".into());
        }
        bytes.extend_from_slice(&block[..count]);
    }
    let body = String::from_utf8(bytes[header_end..header_end + length].to_vec())
        .map_err(|e| e.to_string())?;
    Ok(Request { method, path, body })
}

fn response(status: u16, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
}

fn ready(session: Option<&str>) -> String {
    envelope(
        session,
        r#"{"ready":true,"os":{"name":"iOS","version":"fixture"}}"#,
    )
}

fn envelope(session: Option<&str>, value: &str) -> String {
    format!(
        r#"{{"sessionId":{},"value":{value}}}"#,
        session.map(quote).unwrap_or_else(|| "null".into())
    )
}

fn session(id: &str) -> String {
    envelope(
        Some(id),
        &format!(r#"{{"sessionId":{},"capabilities":{{}}}}"#, quote(id)),
    )
}

fn attached_steps() -> Vec<Exchange> {
    vec![
        Exchange::json("GET", "/status", &ready(Some("external"))),
        Exchange::json("GET", "/session/external", &session("external")),
    ]
}

#[test]
fn pure_validation_rejects_ambiguous_targets_and_preserves_unicode() {
    for value in [
        "https://127.0.0.1:8100",
        "http://192.0.2.1:8100",
        "http://localhost",
        "http://localhost:0",
        "http://user@localhost:8100",
        "http://localhost:8100/?x=y",
        "http://localhost:8100/a/../b",
        "http://localhost:8100/\r\nx",
        "http://[::1]:8100/%2e%2e",
        "http://::1:8100",
    ] {
        assert!(validate_endpoint(value).is_err(), "{value}");
    }
    for value in [
        "http://localhost:8100",
        "http://127.0.0.1:8100/wda/",
        "http://[::1]:8100",
    ] {
        validate_endpoint(value).unwrap();
    }
    validate_text("中文 😀 e\u{301}\n").unwrap();
    assert!(validate_text("password\0").is_err());
    assert!(validate_text(&"x".repeat(MAX_TEXT + 1)).is_err());
    assert!(validate_session("../other").is_err());
    assert!(validate_bundle("com.example/Other").is_err());
    assert!(validate_point(f64::NAN, 0.0).is_err());
    assert!(validate_swipe(0.0, 0.0, 10.0, 20.0, 0).is_err());
    assert!(validate_key("Ctrl+A").is_err());
    assert!(validate_format("html").is_err());
    assert!(validate_url("file:///private/data").is_err());
    assert!(validate_url("https://user:password@example.test/").is_err());
}

#[test]
fn read_only_connection_does_not_adopt_or_create_an_existing_session() {
    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", &ready(Some("external"))),
        Exchange::json(
            "GET",
            "/source?format=json",
            &envelope(
                Some("external"),
                r#"{"type":"Application","children":[{"type":"Button","label":"你好"}]}"#,
            ),
        ),
        Exchange::json(
            "GET",
            "/wda/screen",
            &envelope(
                Some("external"),
                r#"{"screenSize":{"width":390,"height":844},"scale":3}"#,
            ),
        ),
    ]);
    let (device, status) = Ios::connect_with_status(&mock.endpoint, None).unwrap();
    assert!(status.contains(r#""attachedSessionId":null"#));
    assert!(status.contains(r#""activeSessionId":"external""#));
    assert!(
        device
            .tap(1.0, 2.0)
            .unwrap_err()
            .contains("requires an explicitly")
    );
    let snapshot = device.snapshot().unwrap();
    with_json(&snapshot, |value| {
        assert_eq!(text(value.get("coordinateSpace").unwrap())?, "points");
        assert!(value.get("tree").is_some());
        assert!(value.get("screen").is_some());
        Ok(())
    })
    .unwrap();
    drop(device);
    assert_eq!(mock.finish().len(), 3);
}

#[test]
fn attached_external_session_is_never_deleted_by_end_or_drop() {
    let mock = Mock::new(attached_steps());
    let mut device = Ios::connect(&mock.endpoint, Some("external")).unwrap();
    assert!(device.session_end().unwrap_err().contains("external"));
    drop(device);
    assert_eq!(mock.finish().len(), 2);
}

#[test]
fn explicit_replacement_and_owned_cleanup_follow_real_wda_routes() {
    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", &ready(Some("foreign"))),
        Exchange::json("GET", "/status", &ready(Some("foreign"))),
        Exchange::json("GET", "/status", &ready(Some("foreign"))),
        Exchange::json("POST", "/session", &session("owned")),
        Exchange::json("GET", "/status", &ready(Some("owned"))),
        Exchange::json("DELETE", "/session/owned", &envelope(None, "null")),
    ]);
    let mut device = Ios::connect(&mock.endpoint, None).unwrap();
    assert!(
        device
            .session_start(Some("com.example.fixture"), false)
            .unwrap_err()
            .contains("replace=true")
    );
    device
        .session_start(Some("com.example.fixture"), true)
        .unwrap();
    assert!(device.session_end().unwrap().contains(r#""ended":true"#));
    let requests = mock.finish();
    with_json(&requests[3].body, |value| {
        let capabilities = value
            .get("capabilities")
            .unwrap()
            .get("alwaysMatch")
            .unwrap();
        assert_eq!(
            text(capabilities.get("bundleId").unwrap())?,
            "com.example.fixture"
        );
        assert_eq!(
            capabilities.get("forceAppLaunch").unwrap().boolean(),
            Some(false)
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn unknown_active_session_requires_explicit_replacement() {
    let unknown = r#"{"value":{"ready":true}}"#;
    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", unknown),
        Exchange::json("GET", "/status", unknown),
        Exchange::json("GET", "/status", unknown),
        Exchange::json("POST", "/session", &session("owned")),
    ]);
    let mut device = Ios::connect(&mock.endpoint, None).unwrap();
    assert!(device.session_start(None, false).is_err());
    device.session_start(None, true).unwrap();
    drop(device);
    assert_eq!(mock.finish().len(), 4);
}

#[test]
fn later_external_session_is_not_deleted_by_owned_cleanup() {
    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", &ready(None)),
        Exchange::json("GET", "/status", &ready(None)),
        Exchange::json("POST", "/session", &session("owned")),
        Exchange::json("GET", "/status", &ready(Some("replacement"))),
    ]);
    let mut device = Ios::connect(&mock.endpoint, None).unwrap();
    device.session_start(None, false).unwrap();
    assert!(
        device
            .session_end()
            .unwrap_err()
            .contains("no other session was deleted")
    );
    assert!(device.tap(0.0, 0.0).is_err());
    assert_eq!(mock.finish().len(), 4);
}

#[test]
fn failed_session_creation_invalidates_the_previous_input_target() {
    let mut exchanges = attached_steps();
    exchanges.extend([
        Exchange::json("GET", "/status", &ready(Some("external"))),
        Exchange::raw(
            "POST",
            "/session",
            response(
                500,
                &envelope(
                    None,
                    r#"{"error":"session not created","message":"fixture failure"}"#,
                ),
            ),
        ),
    ]);
    let mock = Mock::new(exchanges);
    let mut device = Ios::connect(&mock.endpoint, Some("external")).unwrap();
    assert!(
        device
            .session_start(None, true)
            .unwrap_err()
            .contains("session not created")
    );
    assert!(
        device
            .tap(1.0, 1.0)
            .unwrap_err()
            .contains("requires an explicitly")
    );
    assert_eq!(mock.finish().len(), 4);
}

#[test]
fn unicode_touch_buttons_apps_and_urls_use_the_verified_protocol() {
    let mut exchanges = attached_steps();
    let ok = envelope(Some("external"), "null");
    for path in [
        "/session/external/wda/keys",
        "/session/external/wda/tap",
        "/session/external/actions",
        "/session/external/wda/pressButton",
        "/session/external/wda/apps/launch",
        "/session/external/url",
    ] {
        exchanges.push(Exchange::json("POST", path, &ok));
    }
    let mock = Mock::new(exchanges);
    let device = Ios::connect(&mock.endpoint, Some("external")).unwrap();
    device.type_text("中文 😀\n").unwrap();
    device.tap(10.5, 20.25).unwrap();
    device.swipe(1.0, 2.0, 100.0, 200.0, 350).unwrap();
    device.key("VolumeUp").unwrap();
    device.launch("com.example.fixture").unwrap();
    device.open_url("https://example.test/path").unwrap();
    let requests = mock.finish();
    with_json(&requests[2].body, |value| {
        let values: Vec<_> = value.get("value").unwrap().elements().collect();
        assert_eq!(values.len(), 1);
        assert_eq!(text(values[0])?, "中文 😀\n");
        Ok(())
    })
    .unwrap();
    with_json(&requests[4].body, |value| {
        let pointer = value.get("actions").unwrap().elements().next().unwrap();
        assert_eq!(
            text(
                pointer
                    .get("parameters")
                    .unwrap()
                    .get("pointerType")
                    .unwrap()
            )?,
            "touch"
        );
        let actions: Vec<_> = pointer.get("actions").unwrap().elements().collect();
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[2].get("duration").unwrap().integer().unwrap(), 350);
        assert_eq!(text(actions[3].get("type").unwrap())?, "pointerUp");
        Ok(())
    })
    .unwrap();
    assert!(requests[5].body.contains("volumeUp"));
    assert!(requests[6].body.contains(r#""asAppUnderTest":true"#));
    assert!(requests[7].body.contains("https://example.test/path"));
}

#[test]
fn wda_error_payload_is_failure_even_when_http_status_is_200() {
    let mut exchanges = attached_steps();
    let error = envelope(
        Some("external"),
        r#"{"error":"invalid element state","message":"keyboard is unavailable","traceback":"fixture"}"#,
    );
    exchanges.extend([
        Exchange::json("POST", "/session/external/wda/keys", &error),
        Exchange::raw("POST", "/session/external/wda/keys", response(500, &error)),
    ]);
    let mock = Mock::new(exchanges);
    let device = Ios::connect(&mock.endpoint, Some("external")).unwrap();
    for _ in 0..2 {
        let error = device.type_text("hello").unwrap_err();
        assert!(error.contains("invalid element state"));
        assert!(error.contains("keyboard is unavailable"));
    }
    assert_eq!(mock.finish().len(), 4);
}

#[test]
fn chunked_http_and_prefix_endpoints_are_decoded_without_waiting_for_eof() {
    let body = ready(None);
    let mut encoded = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for chunk in body.as_bytes().chunks(7) {
        encoded.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        encoded.extend_from_slice(chunk);
        encoded.extend_from_slice(b"\r\n");
    }
    encoded.extend_from_slice(b"0\r\nX-Fixture: yes\r\n\r\n");
    let mock = Mock::new(vec![Exchange::raw("GET", "/bridge/status", encoded)]);
    Ios::connect(&format!("{}/bridge/", mock.endpoint), None).unwrap();
    assert_eq!(mock.finish().len(), 1);
}

#[test]
fn ambiguous_overlimit_and_truncated_http_responses_are_rejected() {
    let cases = vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_JSON + 1
        )
        .into_bytes(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nq\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{}".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nContent-Length: 2\r\n\r\n"
            .to_vec(),
    ];
    for bytes in cases {
        let mock = Mock::new(vec![Exchange::raw("GET", "/status", bytes)]);
        assert!(Ios::connect(&mock.endpoint, None).is_err());
        assert_eq!(mock.finish().len(), 1);
    }
    let duplicate = r#"{"sessionId":null,"sessionId":"foreign","value":{"ready":true}}"#;
    let mock = Mock::new(vec![Exchange::json("GET", "/status", duplicate)]);
    let error = Ios::connect(&mock.endpoint, None).err().unwrap();
    assert!(
        error.contains("duplicate")
            || error.contains("Duplicate")
            || error.contains("repeats field sessionId"),
        "{error}"
    );
    mock.finish();
}

#[test]
fn absolute_deadline_stops_trickled_action_responses_without_replay() {
    let mock = Mock::new(vec![Exchange {
        method: "POST",
        path: "/session/owned/wda/tap",
        reply: Reply::Trickle(envelope(Some("owned"), "null")),
    }]);
    let device = Ios {
        endpoint: parse_endpoint(&mock.endpoint).unwrap(),
        session: Some("owned".into()),
        owned_session: Some("owned".into()),
        timeout: Duration::from_millis(120),
    };
    let start = Instant::now();
    let error = device.tap(1.0, 1.0).unwrap_err();
    assert!(error.contains("deadline exceeded"), "{error}");
    assert!(error.contains("outcome is unknown"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(mock.finish().len(), 1);
}

struct TemporaryDirectory(std::path::PathBuf);
impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zuse-ios-protocol-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn screenshot_requires_png_structure_and_failed_capture_preserves_the_destination() {
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJZkAAAAASUVORK5CYII=";
    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", &ready(None)),
        Exchange::json("GET", "/screenshot", &envelope(None, r#""AAAA""#)),
        Exchange::json("GET", "/screenshot", &envelope(None, &quote(PNG))),
    ]);
    let directory = TemporaryDirectory::new();
    let path = directory.0.join("screen.png");
    fs::write(&path, b"previous screenshot").unwrap();
    let device = Ios::connect(&mock.endpoint, None).unwrap();
    assert!(device.screenshot(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous screenshot");
    let result = device.screenshot(&path).unwrap();
    with_json(&result, |value| {
        assert_eq!(value.get("width").unwrap().integer().unwrap(), 1);
        assert_eq!(value.get("height").unwrap().integer().unwrap(), 1);
        assert_eq!(text(value.get("coordinateSpace").unwrap())?, "pixels");
        Ok(())
    })
    .unwrap();
    assert!(fs::read(&path).unwrap().starts_with(b"\x89PNG"));
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(mock.finish().len(), 3);
}

#[test]
fn source_text_is_valid_json_and_invalid_parameters_do_not_send_requests() {
    let xml = "<Application name=\"你好 & fixture\"/>";
    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", &ready(None)),
        Exchange::json("GET", "/source?format=xml", &envelope(None, &quote(xml))),
    ]);
    let device = Ios::connect(&mock.endpoint, None).unwrap();
    assert!(device.source("html").is_err());
    assert!(device.tap(f64::INFINITY, 0.0).is_err());
    assert!(device.key("Ctrl+Q").is_err());
    assert!(device.source("xml").unwrap_err().contains("raw=true"));
    assert!(
        device
            .source("description")
            .unwrap_err()
            .contains("raw=true")
    );
    let output = device.source_with_options("xml", true).unwrap();
    with_json(&output, |value| {
        assert_eq!(text(value.get("source").unwrap())?, xml);
        Ok(())
    })
    .unwrap();
    assert_eq!(mock.finish().len(), 2);
}

#[test]
fn structured_observations_redact_native_secure_fields_and_descendants() {
    let tree = r#"{"type":"Application","name":"Fixture","children":[{"type":"SecureTextField","name":"secret-name","value":123456789,"label":"secret-label","text":"secret-text","rawIdentifier":"secret-id","placeholderValue":"secret-placeholder","futureField":{"value":"secret-future"},"rect":{"x":10,"y":20,"width":80,"height":30,"extra":"secret-rectangle"},"isEnabled":true,"children":[{"type":"StaticText","name":"secret-child-name","value":"secret-child-value"}]},{"type":"TextField","value":"public fixture value"}]}"#;
    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", &ready(None)),
        Exchange::json("GET", "/source?format=json", &envelope(None, tree)),
        Exchange::json(
            "GET",
            "/wda/screen",
            &envelope(
                None,
                r#"{"screenSize":{"width":390,"height":844},"scale":3}"#,
            ),
        ),
        Exchange::json("GET", "/source?format=json", &envelope(None, tree)),
        Exchange::json("GET", "/source?format=json", &envelope(None, tree)),
    ]);
    let device = Ios::connect(&mock.endpoint, None).unwrap();
    let snapshot = device.snapshot().unwrap();
    let source = device.source("json").unwrap();
    for output in [&snapshot, &source] {
        assert!(!output.contains("secret-"), "{output}");
        assert!(!output.contains("123456789"), "{output}");
        assert!(output.contains("public fixture value"));
        assert!(output.contains(r#""redacted":true"#));
        assert!(output.contains(r#""x":10"#));
        assert!(output.contains(r#""isEnabled":true"#));
        with_json(output, |_| Ok(())).unwrap();
    }
    let raw = device.source_with_options("json", true).unwrap();
    assert!(raw.contains("secret-child-value"));
    assert!(raw.contains(r#""raw":true"#));
    assert_eq!(mock.finish().len(), 5);
}

#[test]
fn source_redaction_rejects_ambiguous_roles_and_malformed_node_structure() {
    for tree in [
        r#"{"type":"TextField","\u0074ype":"SecureTextField","value":"private"}"#,
        r#"{"type":"Application","children":[],"children":[{"type":"SecureTextField"}]}"#,
        r#"{"type":"Application","children":"private"}"#,
        r#"{"type":"Application","children":[{"value":"private"}]}"#,
        r#"{"type":"SecureTextField","rect":{"x":"private"}}"#,
        r#"{"value":"private"}"#,
    ] {
        assert!(redact_tree(tree).is_err(), "{tree}");
    }
    let escaped = r#"{"type":"Secure\u0054extField","value":"private","children":[{"type":"StaticText","value":"private-child"}]}"#;
    let output = redact_tree(escaped).unwrap();
    assert!(!output.contains("private"));
    assert_eq!(output.matches(r#""redacted":true"#).count(), 2);
    let flagged = redact_tree(r#"{"type":"TextField","isSecure":true,"value":"private"}"#).unwrap();
    assert!(!flagged.contains("private"));
    validate_source("json", false).unwrap();
    assert!(validate_source("xml", false).is_err());
    assert!(validate_source("description", false).is_err());
    validate_source("xml", true).unwrap();
    validate_source("description", true).unwrap();
}

#[test]
fn malformed_action_response_is_not_success_or_an_automatic_retry() {
    let mock = Mock::new(vec![Exchange::json(
        "POST",
        "/session/owned/wda/tap",
        "{broken response",
    )]);
    let device = Ios {
        endpoint: parse_endpoint(&mock.endpoint).unwrap(),
        session: Some("owned".into()),
        owned_session: Some("owned".into()),
        timeout: Duration::from_secs(1),
    };
    let error = device.tap(1.0, 1.0).unwrap_err();
    assert!(error.contains("outcome could not be confirmed"), "{error}");
    assert!(error.contains("observe before retrying"), "{error}");
    assert_eq!(mock.finish().len(), 1);
}

#[test]
fn mcp_ios_screenshot_returns_real_png_and_removes_its_temporary_output() {
    use crate::{mcp::Engine, runtime::Options};
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJZkAAAAASUVORK5CYII=";
    const META: &str = r#"{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}"#;
    fn request(engine: &mut Engine, id: u32, method: &str, parameters: &str) -> String {
        let frame = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":{},"params":{{"_meta":{META}{parameters}}}}}"#,
            quote(method),
        );
        String::from_utf8(engine.handle(frame.as_bytes()).unwrap().unwrap()).unwrap()
    }
    fn call(engine: &mut Engine, id: u32, name: &str, arguments: &str) -> String {
        request(
            engine,
            id,
            "tools/call",
            &format!(r#","name":{},"arguments":{arguments}"#, quote(name)),
        )
    }

    let mock = Mock::new(vec![
        Exchange::json("GET", "/status", &ready(None)),
        Exchange::json("GET", "/screenshot", &envelope(None, &quote(PNG))),
    ]);
    let mut engine = Engine::new(Options {
        read_only: true,
        ..Options::default()
    });
    let listed = request(&mut engine, 1, "tools/list", "");
    with_json(&listed, |root| {
        assert!(root.get("error").is_none(), "{listed}");
        let definition = root
            .get("result")
            .unwrap()
            .get("tools")
            .unwrap()
            .elements()
            .find(|tool| {
                tool.get("name")
                    .and_then(Value::string)
                    .is_some_and(|name| name.matches("zuse.screenshot"))
            })
            .unwrap();
        let sources = definition
            .get("inputSchema")
            .unwrap()
            .get("properties")
            .unwrap()
            .get("source")
            .unwrap()
            .get("enum")
            .unwrap();
        let sources: Vec<_> = sources.elements().map(text).collect::<Result<_>>()?;
        assert!(sources.iter().any(|source| source == "ios"));
        assert!(sources.iter().any(|source| source == "android"));
        Ok(())
    })
    .unwrap();

    let code = format!("ios.connect(endpoint={})", quote(&mock.endpoint));
    let connected = call(
        &mut engine,
        2,
        "zuse.execute",
        &format!(r#"{{"code":{}}}"#, quote(&code)),
    );
    with_json(&connected, |root| {
        let result = root.get("result").unwrap();
        assert_eq!(
            result.get("isError").and_then(Value::boolean),
            Some(false),
            "{connected}"
        );
        assert_eq!(
            text(
                result
                    .get("structuredContent")
                    .unwrap()
                    .get("status")
                    .unwrap()
            )?,
            "completed"
        );
        Ok(())
    })
    .unwrap();

    let blocked = call(
        &mut engine,
        3,
        "zuse.execute",
        r#"{"code":"ios.tap(x=1,y=1)","read_only":false}"#,
    );
    with_json(&blocked, |root| {
        assert_eq!(
            root.get("result")
                .unwrap()
                .get("isError")
                .and_then(Value::boolean),
            Some(true),
            "{blocked}"
        );
        assert!(blocked.contains("blocked in read-only mode"), "{blocked}");
        Ok(())
    })
    .unwrap();

    // No path argument: Engine owns the screenshot location and its cleanup.
    let captured = call(&mut engine, 4, "zuse.screenshot", r#"{"source":"ios"}"#);
    let path = with_json(&captured, |root| {
        assert!(root.get("error").is_none(), "{captured}");
        let result = root.get("result").unwrap();
        assert_eq!(
            result.get("isError").and_then(Value::boolean),
            Some(false),
            "{captured}"
        );
        let image = result
            .get("content")
            .unwrap()
            .elements()
            .find(|content| {
                content
                    .get("type")
                    .and_then(Value::string)
                    .is_some_and(|kind| kind.matches("image"))
            })
            .unwrap();
        assert_eq!(text(image.get("mimeType").unwrap())?, "image/png");
        assert_eq!(text(image.get("data").unwrap())?, PNG);
        let structured = result.get("structuredContent").unwrap();
        assert_eq!(text(structured.get("status").unwrap())?, "completed");
        let capture = structured
            .get("results")
            .unwrap()
            .elements()
            .next()
            .unwrap()
            .get("value")
            .unwrap();
        assert_eq!(capture.get("width").unwrap().integer().unwrap(), 1);
        assert_eq!(capture.get("height").unwrap().integer().unwrap(), 1);
        text(capture.get("path").unwrap())
    })
    .unwrap();
    let path = Path::new(&path);
    assert_eq!(path.parent().unwrap(), std::env::temp_dir());
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("zuse-mcp-")
    );
    assert!(
        !path.exists(),
        "MCP temporary screenshot was not removed: {}",
        path.display()
    );
    assert_eq!(mock.finish().len(), 2);
}

#[test]
fn browser_url_authority_validation_rejects_invalid_targets_and_preserves_valid_urls() {
    use crate::runtime::{self, Options};
    for url in [
        "http://",
        "https:///path",
        "http://:",
        "http://:80/path",
        "https://?query",
        "https://#fragment",
        "http://example.test:-1",
        "http://example.test:+80",
        "http://example.test:80x",
        "http://example.test:65536",
        "http://example.test:9999999999999999999999999999",
        "http://example.test:1:2",
        "http://[]",
        "http://[::1",
        "http://[::1]]",
        "http://[not-an-ip]",
        "http://[127.0.0.1]",
        "http://::1",
        "http://[::1]suffix",
        "http://[::1]:abc",
        "http://[::1]:65536",
        "http://example.test]",
        "http://user:password@[::1]/",
        "http://example.test\\other",
        "http://%3a",
        "http://example%2F.test",
        "http://example%40other.test",
        "http://%00",
        "http://%FF",
        "http://%GG",
        "http://%",
        "file:///private/data",
        "javascript:alert(1)",
    ] {
        assert!(validate_url(url).is_err(), "accepted invalid URL: {url}");
        let code = format!("ios.open_url(url={})", quote(url));
        assert!(runtime::check(&code, Options::default()).is_err(), "{url}");
    }
    for url in [
        "https://example.test",
        "https://example.test/path?q=hello#fragment",
        "http://localhost:8080/path",
        "http://127.0.0.1:80/",
        "https://example.test:65535/",
        "https://example.test:000443/",
        "https://example.test:/default-port",
        "http://example.test:0/",
        "https://example.test./",
        "https://例子.测试/路径?q=你好#片段",
        "https://%65xample.test/",
        "https://%E4%BE%8B%E5%AD%90.test/",
        "http://[::1]/",
        "http://[::1]:8080/path",
        "https://[2001:db8::1]:443/",
        "http://[::ffff:192.0.2.1]:/",
        "https://example.test/path@name?next=http://other.test:99999",
    ] {
        validate_url(url).unwrap_or_else(|error| panic!("{url}: {error}"));
        let code = format!("ios.open_url(url={})", quote(url));
        runtime::check(&code, Options::default()).unwrap_or_else(|error| panic!("{url}: {error}"));
    }
}

#[test]
fn invalid_browser_url_is_rejected_before_an_earlier_connect_sends_a_request() {
    use crate::runtime::{Options, Runtime};
    for invalid in ["http://:", "https://example.test:65536", "http://[::1"] {
        let mock = Mock::new(vec![Exchange::json("GET", "/status", &ready(None))]);
        let code = format!(
            "ios.connect(endpoint={}); ios.open_url(url={})",
            quote(&mock.endpoint),
            quote(invalid),
        );
        let result = Runtime::new().execute(&code, Options::default());
        assert!(
            result.is_err(),
            "invalid URL escaped batch preflight: {invalid}: {result:?}"
        );
        assert!(
            mock.seen.lock().unwrap().is_empty(),
            "preflight sent an earlier request"
        );
        // No request is expected: Drop stops the fixture's bounded accept loop.
        drop(mock);
    }
}
