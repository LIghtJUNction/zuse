//! Stateful loopback CDP fixture: page identity, attachment ownership and failure recovery.
use super::{Browser, encode_base64, field_string, optional_string, quote, sha1, with_json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

const OWNED: &str = "owned-tab";
const FIRST: &str = "existing-one";
const SECOND: &str = "existing-two";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    Attach,
    Enable,
    CloseFalse,
    Detach,
}

#[derive(Clone)]
struct State {
    targets: BTreeSet<String>,
    sessions: BTreeMap<String, String>,
    closed: Vec<String>,
    fault: Fault,
    sequence: usize,
}

struct Fixture {
    endpoint: String,
    state: Arc<Mutex<State>>,
    worker: JoinHandle<()>,
}

impl Fixture {
    fn start(fault: Fault) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "ws://{}/devtools/browser/fixture",
            listener.local_addr().unwrap()
        );
        let state = Arc::new(Mutex::new(State {
            targets: [FIRST.to_owned(), SECOND.to_owned()].into(),
            sessions: BTreeMap::new(),
            closed: Vec::new(),
            fault,
            sequence: 0,
        }));
        let worker_state = state.clone();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            handshake(&mut stream);
            while let Some(request) = read_frame(&mut stream) {
                let id = with_json(&request, |root| {
                    root.get("id")
                        .ok_or_else(|| "missing ID".to_owned())?
                        .integer()
                        .map_err(|e| format!("{e:?}"))
                })
                .unwrap();
                let method = field_string(&request, &["method"]).unwrap();
                let session = optional_string(&request, &["sessionId"]).unwrap();
                let target = optional_string(&request, &["params", "targetId"]).unwrap();
                let detach = optional_string(&request, &["params", "sessionId"]).unwrap();
                let result = dispatch(
                    &mut worker_state.lock().unwrap(),
                    &method,
                    session.as_deref(),
                    target.as_deref(),
                    detach.as_deref(),
                );
                let response = match result {
                    Ok(value) => format!(r#"{{"id":{id},"result":{value}}}"#),
                    Err(message) => format!(
                        r#"{{"id":{id},"error":{{"code":-32000,"message":{}}}}}"#,
                        quote(message)
                    ),
                };
                send_frame(&mut stream, response.as_bytes());
            }
        });
        Self {
            endpoint,
            state,
            worker,
        }
    }

    fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    fn finish(self) -> State {
        self.worker.join().unwrap();
        self.state.lock().unwrap().clone()
    }
}

fn dispatch(
    state: &mut State,
    method: &str,
    session: Option<&str>,
    target: Option<&str>,
    detach: Option<&str>,
) -> Result<String, &'static str> {
    match method {
        "Target.createTarget" => {
            state.targets.insert(OWNED.to_owned());
            Ok(format!(r#"{{"targetId":{}}}"#, quote(OWNED)))
        }
        "Target.getTargetInfo" => {
            let target = target.unwrap();
            if !state.targets.contains(target) {
                return Err("target no longer exists");
            }
            Ok(format!(
                r#"{{"targetInfo":{{"targetId":{},"type":"page"}}}}"#,
                quote(target)
            ))
        }
        "Target.attachToTarget" => {
            let target = target.unwrap();
            if !state.targets.contains(target) {
                return Err("cannot attach to closed target");
            }
            if target != OWNED && state.fault == Fault::Attach {
                state.fault = Fault::None;
                return Err("candidate attachment rejected");
            }
            state.sequence += 1;
            let session = format!("session-{}", state.sequence);
            state.sessions.insert(session.clone(), target.to_owned());
            Ok(format!(r#"{{"sessionId":{}}}"#, quote(&session)))
        }
        "Page.enable" | "Page.setLifecycleEventsEnabled" | "Runtime.evaluate" => {
            let target = state
                .sessions
                .get(session.unwrap_or(""))
                .ok_or("session is no longer attached")?;
            if !state.targets.contains(target) {
                return Err("page has been closed");
            }
            if method == "Page.setLifecycleEventsEnabled"
                && target != OWNED
                && state.fault == Fault::Enable
            {
                state.fault = Fault::None;
                return Err("candidate page initialization rejected");
            }
            if method == "Runtime.evaluate" {
                Ok(format!(
                    r#"{{"result":{{"type":"string","value":{}}}}}"#,
                    quote(target)
                ))
            } else {
                Ok("{}".into())
            }
        }
        "Target.detachFromTarget" => {
            let session = detach.unwrap();
            if state
                .sessions
                .get(session)
                .is_some_and(|target| target == FIRST)
                && state.fault == Fault::Detach
            {
                state.fault = Fault::None;
                return Err("current session detachment rejected");
            }
            state
                .sessions
                .remove(session)
                .ok_or("session already detached")?;
            Ok("{}".into())
        }
        "Target.closeTarget" => {
            let target = target.unwrap();
            if target == OWNED && state.fault == Fault::CloseFalse {
                state.fault = Fault::None;
                return Ok(r#"{"success":false}"#.into());
            }
            let closed = state.targets.remove(target);
            if closed {
                state.closed.push(target.to_owned());
                state.sessions.retain(|_, value| value != target);
            }
            Ok(format!(r#"{{"success":{closed}}}"#))
        }
        _ => panic!("unexpected CDP method: {method}"),
    }
}

fn handshake(stream: &mut TcpStream) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 16384);
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).unwrap();
    let key = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("Sec-WebSocket-Key")
                .then(|| value.trim())
        })
        .unwrap();
    let accept = encode_base64(&sha1(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ));
    stream.write_all(format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    ).as_bytes()).unwrap();
}

fn read_frame(stream: &mut TcpStream) -> Option<String> {
    let mut header = [0; 2];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            return None;
        }
        Err(error) => panic!("reading fixture request: {error}"),
    }
    assert_eq!(header[0], 0x81);
    assert_ne!(header[1] & 0x80, 0);
    let length = match header[1] & 0x7f {
        126 => {
            let mut bytes = [0; 2];
            stream.read_exact(&mut bytes).unwrap();
            u16::from_be_bytes(bytes) as usize
        }
        127 => panic!("oversized test request"),
        count => count as usize,
    };
    let mut mask = [0; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    Some(String::from_utf8(bytes).unwrap())
}

fn send_frame(stream: &mut TcpStream, bytes: &[u8]) {
    let mut header = vec![0x81];
    if bytes.len() < 126 {
        header.push(bytes.len() as u8);
    } else {
        header.push(126);
        header.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    }
    stream.write_all(&header).unwrap();
    stream.write_all(bytes).unwrap();
}

fn assert_page(browser: &mut Browser, target: &str) {
    assert_eq!(browser.evaluate("document.title").unwrap(), quote(target));
}

#[test]
fn selecting_current_owned_page_preserves_it_and_its_ownership() {
    let fixture = Fixture::start(Fault::None);
    let mut browser = Browser::connect(&fixture.endpoint).unwrap();
    let before = fixture.state();
    browser.select(OWNED).unwrap();
    assert_page(&mut browser, OWNED);
    let after = fixture.state();
    assert_eq!(after.sessions, before.sessions);
    assert!(after.closed.is_empty());
    browser.close().unwrap();
    assert!(browser.select(OWNED).is_err());
    let final_state = fixture.finish();
    assert_eq!(final_state.closed, [OWNED]);
    assert!(final_state.targets.contains(FIRST));
}

#[test]
fn switching_to_existing_page_closes_only_the_owned_page() {
    let fixture = Fixture::start(Fault::None);
    let mut browser = Browser::connect(&fixture.endpoint).unwrap();
    browser.select(FIRST).unwrap();
    assert_page(&mut browser, FIRST);
    let before = fixture.state();
    assert_eq!(before.closed, [OWNED]);
    assert_eq!(before.sessions.len(), 1);
    browser.select(FIRST).unwrap();
    assert_page(&mut browser, FIRST);
    assert_eq!(fixture.state().sessions, before.sessions);
    browser.close().unwrap();
    let final_state = fixture.finish();
    assert_eq!(final_state.closed, [OWNED]);
    assert!(final_state.targets.contains(FIRST));
    assert!(final_state.targets.contains(SECOND));
}

fn rejected_candidate_preserves_owned_page(fault: Fault) {
    let fixture = Fixture::start(fault);
    let mut browser = Browser::connect(&fixture.endpoint).unwrap();
    let before = fixture.state();
    assert!(browser.select(FIRST).is_err());
    assert_page(&mut browser, OWNED);
    let after = fixture.state();
    assert_eq!(after.sessions, before.sessions);
    assert_eq!(after.targets, before.targets);
    assert!(after.closed.is_empty());
    browser.close().unwrap();
    assert_eq!(fixture.finish().closed, [OWNED]);
}

#[test]
fn failed_candidate_attachment_keeps_current_page_usable() {
    rejected_candidate_preserves_owned_page(Fault::Attach);
}

#[test]
fn failed_candidate_initialization_keeps_current_page_usable() {
    rejected_candidate_preserves_owned_page(Fault::Enable);
}

#[test]
fn unconfirmed_owned_page_close_keeps_current_page_usable() {
    rejected_candidate_preserves_owned_page(Fault::CloseFalse);
}

#[test]
fn failed_detachment_keeps_selected_existing_page_usable() {
    let fixture = Fixture::start(Fault::None);
    let mut browser = Browser::connect(&fixture.endpoint).unwrap();
    browser.select(FIRST).unwrap();
    let before = fixture.state();
    fixture.state.lock().unwrap().fault = Fault::Detach;
    assert!(browser.select(SECOND).is_err());
    assert_page(&mut browser, FIRST);
    let after = fixture.state();
    assert_eq!(after.sessions, before.sessions);
    assert_eq!(after.targets, before.targets);
    browser.close().unwrap();
    assert_eq!(fixture.finish().closed, [OWNED]);
}

#[test]
fn unknown_target_keeps_current_page_usable() {
    let fixture = Fixture::start(Fault::None);
    let mut browser = Browser::connect(&fixture.endpoint).unwrap();
    let before = fixture.state();
    assert!(browser.select("missing-target").is_err());
    assert_page(&mut browser, OWNED);
    assert_eq!(fixture.state().sessions, before.sessions);
    browser.close().unwrap();
    assert_eq!(fixture.finish().closed, [OWNED]);
}
