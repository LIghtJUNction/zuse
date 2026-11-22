use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    OtherEvents,
    WrongTargetBefore,
    WrongType,
    ChangedBefore,
    ChangedAfter,
    WrongTargetAfter,
    RejectActivation,
    RejectWorld,
    TimeoutAfter,
    CancelAfter,
    Background,
}

#[derive(Default)]
struct Counts {
    activation: usize,
    identity: usize,
    focus: usize,
}

fn read_frame(peer: &mut TcpStream) -> Option<String> {
    let mut header = [0; 2];
    match peer.read_exact(&mut header) {
        Ok(()) => (),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            return None;
        }
        Err(error) => panic!("activation fixture read: {error}"),
    }
    assert_eq!(header[0], 0x81);
    assert_ne!(header[1] & 0x80, 0);
    let length = match header[1] & 0x7f {
        126 => {
            let mut bytes = [0; 2];
            peer.read_exact(&mut bytes).unwrap();
            usize::from(u16::from_be_bytes(bytes))
        }
        127 => panic!("oversized activation fixture request"),
        value => usize::from(value),
    };
    let mut mask = [0; 4];
    peer.read_exact(&mut mask).unwrap();
    let mut bytes = vec![0; length];
    peer.read_exact(&mut bytes).unwrap();
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    Some(String::from_utf8(bytes).unwrap())
}

fn send_frame(peer: &mut TcpStream, message: &str) {
    let mut frame = vec![0x81];
    if message.len() < 126 {
        frame.push(message.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(message.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(message.as_bytes());
    peer.write_all(&frame).unwrap();
}

fn reply(peer: &mut TcpStream, request: &str, value: &str, error: bool) {
    let id = with_json(request, |root| {
        Ok(root.get("id").unwrap().integer().unwrap())
    })
    .unwrap();
    let key = if error { "error" } else { "result" };
    send_frame(peer, &format!(r#"{{"id":{id},"{key}":{value}}}"#));
}

fn mock(
    direct: bool,
    fault: Fault,
    cancellation: Cancellation,
) -> (Browser, Arc<Mutex<Counts>>, thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let counts = Arc::new(Mutex::new(Counts::default()));
    let observed = counts.clone();
    let worker = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut frames = 0;
        let mut waiting_for_close = false;
        let mut runtime_enabled = false;
        while let Some(request) = read_frame(&mut peer) {
            assert!(
                !waiting_for_close,
                "no replay after cancellation/timeout: {request}"
            );
            let method = field_string(&request, &["method"]).unwrap();
            assert_eq!(
                optional_string(&request, &["sessionId"])
                    .unwrap()
                    .as_deref(),
                if direct { None } else { Some("session-a") }
            );
            let mut error = false;
            let response = match method.as_str() {
                "Target.getTargetInfo" => {
                    assert!(
                        optional_string(&request, &["params", "targetId"])
                            .unwrap()
                            .is_none()
                    );
                    let mut counts = observed.lock().unwrap();
                    counts.identity += 1;
                    let target = if fault == Fault::WrongTargetBefore
                        || (fault == Fault::WrongTargetAfter && counts.activation != 0)
                    {
                        "page-b"
                    } else {
                        "page-a"
                    };
                    let kind = if fault == Fault::WrongType {
                        "worker"
                    } else {
                        "page"
                    };
                    format!(r#"{{"targetInfo":{{"targetId":"{target}","type":"{kind}"}}}}"#)
                }
                "Page.getFrameTree" => {
                    frames += 1;
                    let loader = if (fault == Fault::ChangedBefore && frames >= 2)
                        || (fault == Fault::ChangedAfter
                            && observed.lock().unwrap().activation != 0)
                    {
                        "loader-b"
                    } else {
                        "loader-a"
                    };
                    format!(
                        r#"{{"frameTree":{{"frame":{{"id":"frame-a","loaderId":"{loader}","url":"https://fixture.invalid/a"}}}}}}"#
                    )
                }
                "Runtime.enable" => {
                    runtime_enabled = true;
                    "{}".into()
                }
                "Page.createIsolatedWorld" => {
                    assert!(
                        runtime_enabled,
                        "Runtime must be enabled before creating the observation world"
                    );
                    assert_eq!(
                        field_string(&request, &["params", "frameId"]).unwrap(),
                        "frame-a"
                    );
                    with_json(&request, |root| {
                        assert_eq!(
                            root.get("params")
                                .unwrap()
                                .get("grantUniveralAccess")
                                .unwrap()
                                .boolean(),
                            Some(false)
                        );
                        Ok(())
                    })
                    .unwrap();
                    if fault == Fault::RejectWorld {
                        error = true;
                        r#"{"code":-32601,"message":"isolated world unavailable"}"#.into()
                    } else {
                        r#"{"executionContextId":73}"#.into()
                    }
                }
                METHOD => {
                    let mut counts = observed.lock().unwrap();
                    counts.activation += 1;
                    assert_eq!(counts.activation, 1, "activation must never be replayed");
                    assert!(counts.identity >= 2);
                    if fault == Fault::OtherEvents {
                        send_frame(
                            &mut peer,
                            r#"{"method":"Page.lifecycleEvent","sessionId":"session-b","params":{"frameId":"frame-b","name":"active"}}"#,
                        );
                        send_frame(
                            &mut peer,
                            r#"{"method":"Target.targetInfoChanged","params":{"targetInfo":{"targetId":"page-b","type":"page","active":true}}}"#,
                        );
                    }
                    if fault == Fault::RejectActivation {
                        error = true;
                        r#"{"code":-32000,"message":"activation rejected"}"#.into()
                    } else {
                        "{}".into()
                    }
                }
                "Runtime.evaluate" => {
                    let mut counts = observed.lock().unwrap();
                    counts.focus += 1;
                    with_json(&request, |root| {
                        let params = root.get("params").unwrap();
                        assert_eq!(params.get("userGesture").unwrap().boolean(), Some(false));
                        assert_eq!(params.get("awaitPromise").unwrap().boolean(), Some(false));
                        if fault != Fault::Background {
                            assert_eq!(params.get("contextId").unwrap().integer().unwrap(), 73);
                        }
                        Ok(())
                    })
                    .unwrap();
                    if matches!(fault, Fault::TimeoutAfter | Fault::CancelAfter) {
                        waiting_for_close = true;
                        if fault == Fault::CancelAfter {
                            cancellation.cancel();
                        }
                        continue;
                    }
                    let ready = fault != Fault::OtherEvents || counts.focus > 1;
                    format!(
                        r#"{{"result":{{"value":{{"visible":{ready},"focused":{ready},"activeElement":true}}}}}}"#
                    )
                }
                "Emulation.setFocusEmulationEnabled" if fault == Fault::Background => "{}".into(),
                _ => panic!("unexpected activation fixture command: {request}"),
            };
            reply(&mut peer, &request, &response, error);
        }
    });
    let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
    if !direct {
        browser.target = "page-a".into();
        browser.session = "session-a".into();
    }
    (browser, counts, worker)
}

#[test]
fn activation_uses_only_the_selected_session_and_confirms_fresh_focus() {
    for direct in [false, true] {
        let cancellation = Cancellation::default();
        let (mut browser, counts, worker) = mock(direct, Fault::OtherEvents, cancellation.clone());
        let selected = (
            browser.target.clone(),
            browser.session.clone(),
            browser.owned_target,
        );
        let receipt = browser.activate(&cancellation).unwrap();
        assert_eq!(field_string(&receipt, &["targetId"]).unwrap(), "page-a");
        assert!(receipt.contains(r#""confirmation":"page_visibility_and_focus""#));
        assert_eq!(
            (
                browser.target.clone(),
                browser.session.clone(),
                browser.owned_target
            ),
            selected
        );
        browser.close().unwrap();
        worker.join().unwrap();
        let counts = counts.lock().unwrap();
        assert_eq!(counts.activation, 1);
        assert_eq!(
            counts.focus, 2,
            "another page's active events cannot confirm this page"
        );
    }
}

#[test]
fn activation_rejects_unverified_identity_before_sending() {
    for fault in [
        Fault::WrongTargetBefore,
        Fault::WrongType,
        Fault::ChangedBefore,
        Fault::RejectWorld,
    ] {
        let cancellation = Cancellation::default();
        let (mut browser, counts, worker) = mock(false, fault, cancellation.clone());
        let selected = (browser.target.clone(), browser.session.clone());
        let error = browser.activate(&cancellation).unwrap_err();
        assert!(error.contains("activation_submitted=false"), "{error}");
        assert_eq!((browser.target.clone(), browser.session.clone()), selected);
        browser.close().unwrap();
        worker.join().unwrap();
        assert_eq!(counts.lock().unwrap().activation, 0);
    }
}

#[test]
fn activation_reports_submission_on_failure_without_replay_or_selection_change() {
    for (direct, fault) in [
        (false, Fault::ChangedAfter),
        (false, Fault::WrongTargetAfter),
        (true, Fault::ChangedAfter),
        (true, Fault::WrongTargetAfter),
        (false, Fault::RejectActivation),
        (false, Fault::TimeoutAfter),
        (false, Fault::CancelAfter),
    ] {
        let cancellation = Cancellation::default();
        let (mut browser, counts, worker) = mock(direct, fault, cancellation.clone());
        let selected = (browser.target.clone(), browser.session.clone());
        let error = browser
            .activate_until(Instant::now() + Duration::from_millis(750), &cancellation)
            .unwrap_err();
        assert!(error.contains("was not retried"), "{error}");
        let state = if fault == Fault::RejectActivation {
            "unknown"
        } else {
            "true"
        };
        assert!(
            error.contains(&format!("activation_submitted={state}")),
            "{error}"
        );
        if fault == Fault::TimeoutAfter {
            assert!(error.starts_with("timeout:"), "{error}");
        }
        if fault == Fault::CancelAfter {
            assert!(error.starts_with("cancelled:"), "{error}");
        }
        assert_eq!((browser.target.clone(), browser.session.clone()), selected);
        browser.close().unwrap();
        worker.join().unwrap();
        assert_eq!(counts.lock().unwrap().activation, 1);
    }
}

#[test]
fn cancelled_denied_or_expired_activation_never_dispatches() {
    for scenario in 0..3 {
        let cancellation = if scenario == 1 {
            Cancellation::with_dispatch_guard(|| Err("fixture permission denied".into()))
        } else {
            Cancellation::default()
        };
        if scenario == 0 {
            cancellation.cancel();
        }
        let (mut browser, counts, worker) = mock(false, Fault::None, cancellation.clone());
        let deadline = if scenario == 2 {
            Instant::now()
        } else {
            Instant::now() + BUDGET
        };
        let error = browser.activate_until(deadline, &cancellation).unwrap_err();
        assert!(error.contains("activation_submitted=false"), "{error}");
        browser.close().unwrap();
        worker.join().unwrap();
        assert_eq!(counts.lock().unwrap().activation, 0);
    }
}

#[test]
fn owned_focus_emulation_cannot_confirm_activation() {
    let cancellation = Cancellation::default();
    let (mut browser, counts, worker) = mock(false, Fault::Background, cancellation.clone());
    browser.background(true, &cancellation).unwrap();
    let error = browser.activate(&cancellation).unwrap_err();
    assert!(error.contains("disable browser.background"), "{error}");
    assert!(error.contains("activation_submitted=false"), "{error}");
    browser.background(false, &cancellation).unwrap();
    browser.close().unwrap();
    worker.join().unwrap();
    let counts = counts.lock().unwrap();
    assert_eq!(counts.activation, 0);
    assert_eq!(counts.identity, 0);
}

#[test]
fn public_connect_creates_a_background_target_and_select_does_not_activate() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "ws://{}/devtools/browser/fixture",
        listener.local_addr().unwrap()
    );
    let worker = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len() < 16384);
            let mut byte = [0];
            peer.read_exact(&mut byte).unwrap();
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
        peer.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).unwrap();
        for (method, session, response) in [
            ("Target.createTarget", None, r#"{"targetId":"owned"}"#),
            (
                "Target.attachToTarget",
                None,
                r#"{"sessionId":"owned-session"}"#,
            ),
            ("Page.enable", Some("owned-session"), "{}"),
            (
                "Page.setLifecycleEventsEnabled",
                Some("owned-session"),
                "{}",
            ),
            (
                "Target.getTargetInfo",
                None,
                r#"{"targetInfo":{"targetId":"page-a","type":"page"}}"#,
            ),
            (
                "Target.attachToTarget",
                None,
                r#"{"sessionId":"session-a"}"#,
            ),
            ("Page.enable", Some("session-a"), "{}"),
            ("Page.setLifecycleEventsEnabled", Some("session-a"), "{}"),
            ("Target.closeTarget", None, r#"{"success":true}"#),
        ] {
            let request = read_frame(&mut peer).unwrap();
            assert_eq!(field_string(&request, &["method"]).unwrap(), method);
            assert_eq!(
                optional_string(&request, &["sessionId"])
                    .unwrap()
                    .as_deref(),
                session
            );
            if method == "Target.createTarget" {
                with_json(&request, |root| {
                    assert_eq!(
                        root.get("params")
                            .unwrap()
                            .get("background")
                            .unwrap()
                            .boolean(),
                        Some(true)
                    );
                    Ok(())
                })
                .unwrap();
            }
            reply(&mut peer, &request, response, false);
        }
        assert!(
            read_frame(&mut peer).is_none(),
            "connect/select must not send activation or input"
        );
    });
    let mut browser = Browser::connect(&endpoint).unwrap();
    browser.select("page-a").unwrap();
    browser.select("page-a").unwrap();
    assert_eq!(browser.target, "page-a");
    assert_eq!(browser.session, "session-a");
    browser.close().unwrap();
    worker.join().unwrap();
}

fn endpoint(browser: &Browser) -> String {
    let value =
        fs::read_to_string(browser.profile.as_ref().unwrap().join("DevToolsActivePort")).unwrap();
    let mut lines = value.lines();
    format!(
        "ws://127.0.0.1:{}{}",
        lines.next().unwrap(),
        lines.next().unwrap()
    )
}

fn native_state(browser: &mut Browser) -> (bool, bool) {
    let cancellation = Cancellation::default();
    let deadline = Instant::now() + BUDGET;
    let identity = browser
        .activation_identity(deadline, &cancellation)
        .unwrap();
    let context = browser
        .activation_context(&identity.frame, deadline, &cancellation)
        .unwrap();
    browser
        .activation_focus(context, deadline, &cancellation)
        .unwrap()
}

fn fixture_url(title: &str, shadow_main_world: bool) -> String {
    let shadow = if shadow_main_world {
        "document.hasFocus=()=>false;Object.defineProperty(document,'visibilityState',{get:()=> 'hidden',configurable:true});"
    } else {
        ""
    };
    let page = format!(
        "<!doctype html><title>{title}</title><p>Owned activation fixture</p><script>window.inputEvents=[];for(const type of ['keydown','keyup','pointerdown','pointerup'])addEventListener(type,e=>inputEvents.push(e.type));{shadow}</script>"
    );
    format!("data:text/html;base64,{}", encode_base64(page.as_bytes()))
}

#[test]
#[ignore = "requires installed sandboxed Chromium; only activates tabs in its owned headless profile"]
fn native_activation_preserves_selection_and_verifies_owned_tabs() {
    let cancellation = Cancellation::default();
    let mut owner = Browser::launch(true).unwrap();
    owner.navigate(&fixture_url("activation-A", true)).unwrap();
    assert_eq!(
        native_state(&mut owner),
        (true, true),
        "launch must retain its original foreground creation behavior"
    );
    owner.activate(&cancellation).unwrap();
    assert_eq!(native_state(&mut owner), (true, true));
    assert_eq!(
        owner
            .evaluate("[document.visibilityState,document.hasFocus()]")
            .unwrap(),
        r#"["hidden",false]"#,
        "the fixture shadows main-world getters; confirmation must use its isolated world"
    );
    let endpoint = endpoint(&owner);
    let mut other = Browser::connect(&endpoint).unwrap();
    other.navigate(&fixture_url("activation-B", false)).unwrap();
    assert_eq!(
        native_state(&mut owner),
        (true, true),
        "connecting must leave A active"
    );
    assert!(
        !native_state(&mut other).1,
        "new public connect target must be in the background"
    );
    let activated = other.activate(&cancellation).unwrap();
    assert_eq!(
        field_string(&activated, &["targetId"]).unwrap(),
        other.target
    );
    assert_eq!(native_state(&mut other), (true, true));
    assert!(!native_state(&mut owner).1);

    let mut selected = Browser::connect_existing(&endpoint, &owner.target).unwrap();
    selected.select(&other.target).unwrap();
    selected.select(&owner.target).unwrap();
    assert_eq!(
        native_state(&mut other),
        (true, true),
        "attachment and selection must leave B active"
    );
    let selection = (selected.target.clone(), selected.session.clone());
    let receipt = selected.activate(&cancellation).unwrap();
    assert_eq!(field_string(&receipt, &["targetId"]).unwrap(), owner.target);
    assert_eq!(
        (selected.target.clone(), selected.session.clone()),
        selection
    );
    assert_eq!(native_state(&mut owner), (true, true));
    assert!(!native_state(&mut other).1);

    let direct_endpoint = format!(
        "{}/devtools/page/{}",
        endpoint.split("/devtools/").next().unwrap(),
        other.target
    );
    let mut direct = Browser::connect(&direct_endpoint).unwrap();
    assert_eq!(
        native_state(&mut owner),
        (true, true),
        "direct-page connection must not activate B"
    );
    let receipt = direct.activate(&cancellation).unwrap();
    assert_eq!(field_string(&receipt, &["targetId"]).unwrap(), other.target);
    assert!(direct.target.is_empty() && direct.session.is_empty());
    assert_eq!(native_state(&mut other), (true, true));
    assert!(!native_state(&mut owner).1);
    assert!(
        owner.background_state.is_none()
            && other.background_state.is_none()
            && selected.background_state.is_none()
            && direct.background_state.is_none()
    );
    assert_eq!(owner.evaluate("inputEvents.length").unwrap(), "0");
    assert_eq!(other.evaluate("inputEvents.length").unwrap(), "0");
    direct.close().unwrap();
    selected.close().unwrap();
    other.close().unwrap();
    owner.close().unwrap();
}
