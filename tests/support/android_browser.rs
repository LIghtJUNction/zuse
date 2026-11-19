//! Offline Android/CDP protocol tests. All listeners bind ephemeral loopback
//! ports. No ADB executable, real ADB server, desktop, or device is contacted.
use super::{Browser, android_debugging_path, encode_base64, field_string, quote, sha1, with_json};
use crate::android::{Android, validate_browser_options};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SERIAL: &str = "ZUSE-MOCK-DEVICE";
const SOCKET: &str = "chrome_devtools_remote";
const OWNED: &str = "zuse-owned-tab";
const SESSION: &str = "zuse-owned-session";

fn socket_line(name: &str) -> String {
    format!("00000000: 00000002 00000000 00010000 0001 01 1234 @{name}\n")
}

#[derive(Clone, Copy)]
enum DiscoveryFault {
    ShortBody,
    DuplicateLength,
    ConflictingLength,
    InvalidLength,
    TransferEncoding,
    Oversize,
}

#[derive(Clone, Copy)]
enum Cdp {
    Ready,
    AttachFailure,
    CreationFailure,
    ExternalDiscovery,
    WrongTargetInfo,
    CloseRefused,
    DiscoveryKeepAlive,
    DiscoveryEof,
    InvalidDiscovery(DiscoveryFault),
}

struct Server {
    address: SocketAddr,
    worker: JoinHandle<Vec<String>>,
}

impl Server {
    fn start(devices: &str, sockets: Option<String>, cdp: Option<Cdp>, selected: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let devices = devices.to_owned();
        let selected = selected.to_owned();
        let worker = thread::spawn(move || {
            let mut observed = Vec::new();
            let mut stream = accept(&listener);
            service(&mut stream, "host:devices-l", &mut observed);
            stream
                .write_all(format!("{:04x}{devices}", devices.len()).as_bytes())
                .unwrap();
            drop(stream);

            let Some(sockets) = sockets else {
                return observed;
            };
            let mut stream = accept(&listener);
            service(
                &mut stream,
                &format!("host:transport:{SERIAL}"),
                &mut observed,
            );
            service(
                &mut stream,
                "shell,v2,raw:cat /proc/net/unix",
                &mut observed,
            );
            let mut stdin_close = [0; 5];
            stream.read_exact(&mut stdin_close).unwrap();
            assert_eq!(stdin_close, [4, 0, 0, 0, 0]);
            stream.write_all(&[1]).unwrap();
            stream
                .write_all(&(sockets.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(sockets.as_bytes()).unwrap();
            stream.write_all(&[3, 1, 0, 0, 0, 0]).unwrap();
            drop(stream);

            let Some(cdp) = cdp else { return observed };
            let mut stream = tunnel(&listener, &selected, &mut observed);
            let request = http_head(&mut stream);
            assert!(request.starts_with("GET /json/version HTTP/1.1\r\n"));
            assert!(request.contains("\r\nHost: localhost\r\n"));
            observed.push("GET /json/version".into());
            let url = if matches!(cdp, Cdp::ExternalDiscovery) {
                "ws://192.0.2.1:9222/devtools/browser/remote"
            } else {
                "ws://localhost/devtools/browser"
            };
            let body = format!(
                r#"{{"Browser":"Chrome/136.0.0.0","webSocketDebuggerUrl":{}}}"#,
                quote(url)
            );
            let length_fields = match cdp {
                Cdp::InvalidDiscovery(DiscoveryFault::ShortBody) => {
                    format!("Content-Length: {}\r\n", body.len() + 10)
                }
                Cdp::InvalidDiscovery(DiscoveryFault::DuplicateLength) => {
                    format!(
                        "Content-Length: {}\r\ncontent-length: {}\r\n",
                        body.len(),
                        body.len()
                    )
                }
                Cdp::InvalidDiscovery(DiscoveryFault::ConflictingLength) => {
                    format!(
                        "Content-Length: {}\r\nContent-Length: {}\r\n",
                        body.len(),
                        body.len() + 1
                    )
                }
                Cdp::InvalidDiscovery(DiscoveryFault::InvalidLength) => {
                    "Content-Length: +12\r\n".into()
                }
                Cdp::InvalidDiscovery(DiscoveryFault::TransferEncoding) => {
                    format!(
                        "Content-Length: {}\r\nTransfer-Encoding: chunked\r\n",
                        body.len()
                    )
                }
                Cdp::InvalidDiscovery(DiscoveryFault::Oversize) => {
                    "Content-Length: 65536\r\n".into()
                }
                Cdp::DiscoveryEof => String::new(),
                _ => format!("Content-Length: {}\r\n", body.len()),
            };
            let connection = if matches!(cdp, Cdp::DiscoveryKeepAlive) {
                "keep-alive"
            } else {
                "close"
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\n{length_fields}Connection: {connection}\r\n\r\n{body}",
                    )
                    .as_bytes(),
                )
                .unwrap();
            // A real complete HTTP response whose ADB stream deliberately stays
            // open throughout the subsequent WebSocket/CDP conversation.
            let _kept_open_discovery = if matches!(cdp, Cdp::DiscoveryKeepAlive) {
                Some(stream)
            } else {
                drop(stream);
                None
            };
            if matches!(cdp, Cdp::ExternalDiscovery | Cdp::InvalidDiscovery(_)) {
                return observed;
            }

            let mut stream = tunnel(&listener, &selected, &mut observed);
            let request = http_head(&mut stream);
            assert!(request.starts_with("GET /devtools/browser HTTP/1.1\r\n"));
            assert!(request.contains("\r\nHost: localhost\r\n"));
            let key = request
                .lines()
                .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
                .unwrap();
            let accept = encode_base64(&sha1(
                format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
            ));
            stream.write_all(format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            ).as_bytes()).unwrap();

            loop {
                let message = client_message(&mut stream);
                let method = field_string(&message, &["method"]).unwrap();
                let id = with_json(&message, |root| {
                    root.get("id")
                        .ok_or("missing request id")?
                        .integer()
                        .map_err(|error| error.to_string())
                })
                .unwrap();
                observed.push(method.clone());
                let result = match method.as_str() {
                    "Target.createTarget" => {
                        assert_eq!(
                            field_string(&message, &["params", "url"]).unwrap(),
                            "about:blank"
                        );
                        assert!(
                            with_json(&message, |root| Ok(root
                                .get("params")
                                .and_then(|value| value.get("background"))
                                .is_some_and(|value| value.raw() == "true")))
                            .unwrap()
                        );
                        if matches!(cdp, Cdp::CreationFailure) {
                            server_message(
                                &mut stream,
                                &format!(
                                    r#"{{"id":{id},"error":{{"code":-32000,"message":"synthetic target creation failure"}}}}"#
                                ),
                            );
                            break;
                        }
                        format!(r#"{{"targetId":{}}}"#, quote(OWNED))
                    }
                    "Target.attachToTarget" => {
                        assert_eq!(
                            field_string(&message, &["params", "targetId"]).unwrap(),
                            OWNED
                        );
                        if matches!(cdp, Cdp::AttachFailure) {
                            server_message(
                                &mut stream,
                                &format!(
                                    r#"{{"id":{id},"error":{{"code":-32000,"message":"synthetic attach failure"}}}}"#
                                ),
                            );
                            continue;
                        }
                        format!(r#"{{"sessionId":{}}}"#, quote(SESSION))
                    }
                    "Page.enable" | "Page.setLifecycleEventsEnabled" => {
                        assert_eq!(field_string(&message, &["sessionId"]).unwrap(), SESSION);
                        "{}".into()
                    }
                    "Runtime.evaluate" => {
                        assert_eq!(field_string(&message, &["sessionId"]).unwrap(), SESSION);
                        r#"{"result":{"type":"string","value":"mobile fixture"}}"#.into()
                    }
                    "Target.getTargetInfo" => {
                        assert_eq!(
                            field_string(&message, &["params", "targetId"]).unwrap(),
                            OWNED
                        );
                        let target = if matches!(cdp, Cdp::WrongTargetInfo) {
                            "unrelated-tab"
                        } else {
                            OWNED
                        };
                        format!(
                            r#"{{"targetInfo":{{"targetId":{},"type":"page","url":"about:blank","title":"Zuse fixture"}}}}"#,
                            quote(target)
                        )
                    }
                    "Target.closeTarget" => {
                        assert_eq!(
                            field_string(&message, &["params", "targetId"]).unwrap(),
                            OWNED
                        );
                        let success = !matches!(cdp, Cdp::CloseRefused);
                        server_message(
                            &mut stream,
                            &format!(r#"{{"id":{id},"result":{{"success":{success}}}}}"#),
                        );
                        break;
                    }
                    other => {
                        panic!("unexpected CDP operation, possibly outside owned target: {other}")
                    }
                };
                server_message(&mut stream, &format!(r#"{{"id":{id},"result":{result}}}"#));
            }
            observed
        });
        Self { address, worker }
    }

    fn ready(cdp: Cdp) -> Self {
        Self::start(
            &format!("{SERIAL}\tdevice product:synthetic\n"),
            Some(socket_line(SOCKET)),
            Some(cdp),
            SOCKET,
        )
    }

    fn finish(self) -> Vec<String> {
        self.worker.join().expect("mock ADB/CDP server failed")
    }
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "expected mock connection did not arrive"
                );
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("mock accept: {error}"),
        }
    }
}

fn service(stream: &mut TcpStream, expected: &str, observed: &mut Vec<String>) {
    let mut size = [0; 4];
    stream.read_exact(&mut size).unwrap();
    let len = usize::from_str_radix(std::str::from_utf8(&size).unwrap(), 16).unwrap();
    assert!(len < 1024);
    let mut payload = vec![0; len];
    stream.read_exact(&mut payload).unwrap();
    let actual = String::from_utf8(payload).unwrap();
    assert_eq!(actual, expected);
    observed.push(actual);
    stream.write_all(b"OKAY").unwrap();
}

fn tunnel(listener: &TcpListener, socket: &str, observed: &mut Vec<String>) -> TcpStream {
    let mut stream = accept(listener);
    service(&mut stream, &format!("host:transport:{SERIAL}"), observed);
    service(&mut stream, &format!("localabstract:{socket}"), observed);
    stream
}

fn http_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 16384);
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}

fn client_message(stream: &mut TcpStream) -> String {
    let mut header = [0; 2];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(header[0], 0x81);
    assert_ne!(header[1] & 0x80, 0, "client must mask WebSocket frames");
    let len = match header[1] & 0x7f {
        126 => {
            let mut raw = [0; 2];
            stream.read_exact(&mut raw).unwrap();
            u16::from_be_bytes(raw) as usize
        }
        127 => {
            let mut raw = [0; 8];
            stream.read_exact(&mut raw).unwrap();
            usize::try_from(u64::from_be_bytes(raw)).unwrap()
        }
        value => usize::from(value),
    };
    assert!(len <= 65536);
    let mut mask = [0; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut payload = vec![0; len];
    stream.read_exact(&mut payload).unwrap();
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    String::from_utf8(payload).unwrap()
}

fn server_message(stream: &mut TcpStream, payload: &str) {
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(payload.as_bytes());
    stream.write_all(&frame).unwrap();
}

#[test]
fn android_browser_requires_explicit_valid_arguments_before_io() {
    for serial in ["", "bad serial", "bad\nserial", &"x".repeat(257)] {
        assert!(validate_browser_options(serial, None).is_err());
    }
    for socket in [
        "webview_devtools_remote_12",
        "localabstract:chrome_devtools_remote",
        "@chrome_devtools_remote",
        "chrome_devtools_remote_",
        "chrome_devtools_remote_2;other",
        "chrome_devtools_remote_/etc/passwd",
    ] {
        assert!(validate_browser_options(SERIAL, Some(socket)).is_err());
    }
    assert!(validate_browser_options(SERIAL, Some(SOCKET)).is_ok());
    assert!(validate_browser_options("127.0.0.1:5555", Some("chrome_devtools_remote_123")).is_ok());
}

#[test]
fn android_browser_discovery_accepts_only_browser_paths_on_loopback() {
    for url in [
        "ws://localhost/devtools/browser",
        "ws://127.0.0.1/devtools/browser",
        "ws://[::1]/devtools/browser/abc-123",
        "ws://localhost:9222/devtools/browser/abc_123",
    ] {
        assert!(android_debugging_path(url).is_ok(), "{url}");
    }
    for url in [
        "ws://192.0.2.1:9222/devtools/browser",
        "ws://example.test:9222/devtools/browser",
        "ws://user@localhost:9222/devtools/browser",
        "wss://localhost/devtools/browser",
        "ws://localhost/devtools/page/user-tab",
        "ws://localhost/devtools/browser/../page/x",
        "ws://localhost/devtools/browser?other=x",
        "ws://localhost/devtools/browser/\r\nx",
    ] {
        assert!(android_debugging_path(url).is_err(), "{url}");
    }
}

#[test]
fn android_browser_lists_listening_chrome_sockets_without_tabs() {
    let mut sockets = socket_line(SOCKET);
    sockets.push_str(&socket_line("webview_devtools_remote_99"));
    sockets.push_str(&socket_line(SOCKET)); // duplicate kernel row is harmless
    sockets.push_str(&socket_line("chrome_devtools_remote_123"));
    sockets.push_str("0000: 00000002 00000000 00000000 0001 01 33 @chrome_devtools_remote_456\n");
    sockets.push_str("0000: 00000002 00000000 00010000 0002 01 44 @chrome_devtools_remote_789\n");
    sockets.push_str("0000: 00000002 00000000 00010000 0001 01 55 /chrome_devtools_remote_777\n");
    let server = Server::start(&format!("{SERIAL}\tdevice\n"), Some(sockets), None, SOCKET);
    let result = Android::browser_sockets_at(server.address, SERIAL).unwrap();
    assert!(
        result.contains(r#""sockets":["chrome_devtools_remote","chrome_devtools_remote_123"]"#)
    );
    assert!(!result.contains("webview"));
    assert!(!result.contains("456"));
    assert!(!result.contains("789"));
    assert!(!result.contains("777"));
    assert_eq!(server.finish().len(), 3);
}

#[test]
fn android_browser_rejects_unauthorized_device_before_socket_access() {
    let server = Server::start(&format!("{SERIAL}\tunauthorized\n"), None, None, SOCKET);
    let error = Browser::connect_android_at(server.address, SERIAL, None)
        .err()
        .unwrap();
    assert!(error.contains("unauthorized"));
    assert_eq!(server.finish(), ["host:devices-l"]);
}

#[test]
fn android_browser_rejects_ambiguous_absent_and_webview_targets() {
    for (sockets, requested, expected) in [
        (
            socket_line(SOCKET) + &socket_line("chrome_devtools_remote_2"),
            None,
            "multiple",
        ),
        (
            socket_line(SOCKET),
            Some("chrome_devtools_remote_2"),
            "not observed",
        ),
        (
            socket_line("webview_devtools_remote_99"),
            None,
            "WebView-only",
        ),
    ] {
        let server = Server::start(&format!("{SERIAL}\tdevice\n"), Some(sockets), None, SOCKET);
        let error = Browser::connect_android_at(server.address, SERIAL, requested)
            .err()
            .unwrap();
        assert!(error.contains(expected), "{error}");
        assert_eq!(server.finish().len(), 3);
    }
}

#[test]
fn android_browser_owns_one_tab_and_never_requests_all_targets_or_forwards() {
    let selected = "chrome_devtools_remote_2";
    let server = Server::start(
        &format!("{SERIAL}\tdevice\n"),
        Some(socket_line(SOCKET) + &socket_line(selected)),
        Some(Cdp::Ready),
        selected,
    );
    let mut browser = Browser::connect_android_at(server.address, SERIAL, Some(selected)).unwrap();
    let tabs = browser.tabs().unwrap();
    assert!(tabs.contains(OWNED));
    assert!(tabs.contains(r#""scope":"owned""#));
    assert!(browser.select("user-private-tab").is_err());
    assert!(browser.select(OWNED).is_ok());
    assert!(
        browser
            .evaluate("'mobile fixture'")
            .unwrap()
            .contains("mobile fixture")
    );
    browser.close().unwrap();
    drop(browser);
    let observed = server.finish();
    assert!(
        !observed
            .iter()
            .any(|item| item.contains("forward") || item == "Target.getTargets")
    );
    assert_eq!(
        observed
            .iter()
            .filter(|item| *item == "Target.closeTarget")
            .count(),
        1
    );
    assert_eq!(
        observed
            .iter()
            .filter(|item| *item == "Target.attachToTarget")
            .count(),
        1
    );
}

#[test]
fn android_browser_drop_closes_only_its_created_tab() {
    let server = Server::ready(Cdp::Ready);
    let browser = Browser::connect_android_at(server.address, SERIAL, None).unwrap();
    drop(browser);
    assert_eq!(
        server.finish().last().map(String::as_str),
        Some("Target.closeTarget")
    );
}

#[test]
fn android_browser_failed_attach_still_closes_created_tab() {
    let server = Server::ready(Cdp::AttachFailure);
    let error = Browser::connect_android_at(server.address, SERIAL, None)
        .err()
        .unwrap();
    assert!(error.contains("synthetic attach failure"));
    assert_eq!(
        server.finish().last().map(String::as_str),
        Some("Target.closeTarget")
    );
}

#[test]
fn android_browser_failed_creation_never_closes_unknown_tabs() {
    let server = Server::ready(Cdp::CreationFailure);
    let error = Browser::connect_android_at(server.address, SERIAL, None)
        .err()
        .unwrap();
    assert!(error.contains("synthetic target creation failure"));
    let observed = server.finish();
    assert!(!observed.iter().any(|method| method == "Target.closeTarget"));
}

#[test]
fn android_browser_rejects_external_discovery_before_websocket_or_tab_creation() {
    let server = Server::ready(Cdp::ExternalDiscovery);
    let error = Browser::connect_android_at(server.address, SERIAL, None)
        .err()
        .unwrap();
    assert!(error.contains("loopback"));
    let observed = server.finish();
    assert_eq!(
        observed.last().map(String::as_str),
        Some("GET /json/version")
    );
}

#[test]
fn android_browser_rejects_mismatched_owned_tab_information() {
    let server = Server::ready(Cdp::WrongTargetInfo);
    let mut browser = Browser::connect_android_at(server.address, SERIAL, None).unwrap();
    let error = browser.tabs().unwrap_err();
    assert!(error.contains("different target"));
    browser.close().unwrap();
    assert_eq!(
        server.finish().last().map(String::as_str),
        Some("Target.closeTarget")
    );
}

#[test]
fn android_browser_close_refusal_is_error_and_never_touches_other_targets() {
    let server = Server::ready(Cdp::CloseRefused);
    let mut browser = Browser::connect_android_at(server.address, SERIAL, None).unwrap();
    let error = browser.close().unwrap_err();
    assert!(error.contains("success=false"));
    // Repeated close and Drop preserve the failed cleanup outcome without
    // retrying or enumerating unrelated tabs after the tunnel was disconnected.
    assert_eq!(browser.close().unwrap_err(), error);
    drop(browser);
    let observed = server.finish();
    assert_eq!(
        observed
            .iter()
            .filter(|method| *method == "Target.closeTarget")
            .count(),
        1
    );
    assert!(!observed.iter().any(|method| method == "Target.getTargets"));
    assert_eq!(
        observed.last().map(String::as_str),
        Some("Target.closeTarget")
    );
}

#[test]
fn android_browser_discovery_uses_length_without_waiting_for_adb_eof() {
    let server = Server::ready(Cdp::DiscoveryKeepAlive);
    let started = Instant::now();
    let mut browser = Browser::connect_android_at(server.address, SERIAL, None).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    browser.close().unwrap();
    let observed = server.finish();
    assert!(
        observed
            .iter()
            .any(|method| method == "Target.createTarget")
    );
    assert_eq!(
        observed.last().map(String::as_str),
        Some("Target.closeTarget")
    );
}

#[test]
fn android_browser_discovery_retains_bounded_eof_fallback_without_length() {
    let server = Server::ready(Cdp::DiscoveryEof);
    let mut browser = Browser::connect_android_at(server.address, SERIAL, None).unwrap();
    browser.close().unwrap();
    assert_eq!(
        server.finish().last().map(String::as_str),
        Some("Target.closeTarget")
    );
}

#[test]
fn android_browser_discovery_rejects_short_or_ambiguous_http_framing() {
    for (fault, expected) in [
        (DiscoveryFault::ShortBody, "HTTP /json/version"),
        (
            DiscoveryFault::DuplicateLength,
            "duplicate HTTP discovery Content-Length",
        ),
        (
            DiscoveryFault::ConflictingLength,
            "duplicate HTTP discovery Content-Length",
        ),
        (
            DiscoveryFault::InvalidLength,
            "invalid HTTP discovery Content-Length",
        ),
        (
            DiscoveryFault::TransferEncoding,
            "unsupported HTTP discovery Transfer-Encoding",
        ),
        (DiscoveryFault::Oversize, "browser discovery exceeds 64 KiB"),
    ] {
        let server = Server::ready(Cdp::InvalidDiscovery(fault));
        let error = Browser::connect_android_at(server.address, SERIAL, None)
            .err()
            .unwrap();
        assert!(error.contains(expected), "{error}");
        let observed = server.finish();
        assert_eq!(
            observed.last().map(String::as_str),
            Some("GET /json/version")
        );
    }
}
