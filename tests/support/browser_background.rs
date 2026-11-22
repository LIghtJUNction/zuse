//! Protocol lifecycle tests plus one opt-in real two-page input-isolation check.
use super::*;

fn request(peer: &mut TcpStream, method: &str, session: Option<&str>) -> String {
    let mut header = [0; 2];
    peer.read_exact(&mut header).unwrap();
    assert_eq!(header[0], 0x81);
    assert_ne!(header[1] & 0x80, 0);
    let length = match header[1] & 0x7f {
        126 => {
            let mut bytes = [0; 2];
            peer.read_exact(&mut bytes).unwrap();
            usize::from(u16::from_be_bytes(bytes))
        }
        127 => panic!("oversized fixture request"),
        length => usize::from(length),
    };
    let mut mask = [0; 4];
    peer.read_exact(&mut mask).unwrap();
    let mut bytes = vec![0; length];
    peer.read_exact(&mut bytes).unwrap();
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    let message = String::from_utf8(bytes).unwrap();
    assert_eq!(field_string(&message, &["method"]).unwrap(), method);
    assert_eq!(
        optional_string(&message, &["sessionId"])
            .unwrap()
            .as_deref(),
        session
    );
    message
}

fn response(peer: &mut TcpStream, message: &str, field: &str, value: &str) {
    let id = with_json(message, |root| {
        Ok(root.get("id").unwrap().integer().unwrap())
    })
    .unwrap();
    let text = format!(r#"{{"id":{id},"{field}":{value}}}"#);
    let mut frame = vec![0x81];
    if text.len() < 126 {
        frame.push(text.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(text.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(text.as_bytes());
    peer.write_all(&frame).unwrap();
}

fn reply(peer: &mut TcpStream, message: &str, result: &str) {
    response(peer, message, "result", result);
}

fn fixture(
    run: impl FnOnce(&mut TcpStream) + Send + 'static,
) -> (TcpStream, thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    stream.set_nodelay(true).unwrap();
    let worker = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_nodelay(true).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        run(&mut peer);
        let mut byte = [0];
        match peer.read(&mut byte) {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("unexpected command after cleanup: {result:?}"),
        }
    });
    (stream, worker)
}

fn page(peer: &mut TcpStream, session: Option<&str>) {
    for method in ["Page.enable", "Page.setLifecycleEventsEnabled"] {
        let message = request(peer, method, session);
        reply(peer, &message, "{}");
    }
}

fn document(peer: &mut TcpStream, session: Option<&str>, loader: &str) {
    let message = request(peer, "Page.getFrameTree", session);
    reply(
        peer,
        &message,
        &format!(
            r#"{{"frameTree":{{"frame":{{"id":"frame-1","loaderId":{},"url":"https://fixture.invalid/"}}}}}}"#,
            quote(loader)
        ),
    );
}

fn emulation(peer: &mut TcpStream, session: Option<&str>, enabled: bool, accepted: bool) {
    let message = request(peer, METHOD, session);
    with_json(&message, |root| {
        assert_eq!(
            root.get("params")
                .unwrap()
                .get("enabled")
                .unwrap()
                .boolean(),
            Some(enabled)
        );
        Ok(())
    })
    .unwrap();
    if accepted {
        reply(peer, &message, "{}");
    } else {
        response(
            peer,
            &message,
            "error",
            r#"{"code":-32000,"message":"fixture rejection"}"#,
        );
    }
}

fn enable(peer: &mut TcpStream, session: Option<&str>) {
    document(peer, session, "loader-1");
    emulation(peer, session, true, true);
    let message = request(peer, "Runtime.evaluate", session);
    let expression = field_string(&message, &["params", "expression"]).unwrap();
    assert!(expression.contains("document.hasFocus()"));
    reply(
        peer,
        &message,
        r#"{"result":{"value":{"focused":true,"visible":true,"activeElement":true}}}"#,
    );
    document(peer, session, "loader-1");
}

#[test]
fn direct_page_background_is_opt_in_and_close_or_drop_clears_it() {
    for implicit in [false, true] {
        let (stream, worker) = fixture(|peer| {
            page(peer, None);
            enable(peer, None);
            emulation(peer, None, false, true);
        });
        let mut browser = Browser::from_socket(WebSocket::new(stream), true, false).unwrap();
        assert!(browser.target.is_empty() && browser.session.is_empty());
        assert!(browser.background_state.is_none());
        let receipt = browser.background(true, &Cancellation::default()).unwrap();
        assert!(receipt.contains(r#""focus_emulated":true"#));
        assert!(receipt.contains(r#""targetId":null"#));
        if implicit {
            drop(browser);
        } else {
            browser.close().unwrap();
            assert!(browser.background_state.is_none());
            browser.close().unwrap();
        }
        worker.join().unwrap();
    }
}

#[test]
fn navigation_and_selection_clear_only_the_old_background_session() {
    let (stream, worker) = fixture(|peer| {
        page(peer, None);
        enable(peer, None);
        emulation(peer, None, false, true);
        let navigate = request(peer, "Page.navigate", None);
        reply(peer, &navigate, "{}");
    });
    let mut browser = Browser::from_socket(WebSocket::new(stream), true, false).unwrap();
    browser.background(true, &Cancellation::default()).unwrap();
    browser.navigate("https://fixture.invalid/next").unwrap();
    assert!(browser.background_state.is_none());
    browser.close().unwrap();
    worker.join().unwrap();

    let (stream, worker) = fixture(|peer| {
        enable(peer, Some("old-session"));
        let info = request(peer, "Target.getTargetInfo", None);
        reply(
            peer,
            &info,
            r#"{"targetInfo":{"targetId":"new-page","type":"page"}}"#,
        );
        let attach = request(peer, "Target.attachToTarget", None);
        reply(peer, &attach, r#"{"sessionId":"new-session"}"#);
        page(peer, Some("new-session"));
        emulation(peer, Some("old-session"), false, true);
        let detach = request(peer, "Target.detachFromTarget", None);
        assert_eq!(
            field_string(&detach, &["params", "sessionId"]).unwrap(),
            "old-session"
        );
        reply(peer, &detach, "{}");
    });
    let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
    browser.target = "old-page".into();
    browser.session = "old-session".into();
    browser.background(true, &Cancellation::default()).unwrap();
    browser.select("new-page").unwrap();
    assert!(browser.background_state.is_none());
    assert_eq!(browser.session, "new-session");
    browser.close().unwrap();
    worker.join().unwrap();
}

#[test]
fn external_document_change_is_rejected_before_the_next_command() {
    let (stream, worker) = fixture(|peer| {
        enable(peer, Some("selected-session"));
        document(peer, Some("selected-session"), "loader-2");
        emulation(peer, Some("selected-session"), false, true);
    });
    let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
    browser.session = "selected-session".into();
    browser.target = "selected-page".into();
    browser.background(true, &Cancellation::default()).unwrap();
    let error = browser.validate_background_document().unwrap_err();
    assert!(error.starts_with("stale_target:"), "{error}");
    assert!(error.contains("was cleared"), "{error}");
    assert!(browser.background_state.is_none());
    browser.close().unwrap();
    worker.join().unwrap();
}

#[test]
fn failed_enable_and_failed_cleanup_are_not_reported_as_restored() {
    let (stream, worker) = fixture(|peer| {
        document(peer, None, "loader-1");
        emulation(peer, None, true, false);
        emulation(peer, None, false, false);
        // An explicit off remains possible with a cancelled operation token.
        emulation(peer, None, false, true);
    });
    let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
    let error = browser
        .background(true, &Cancellation::default())
        .unwrap_err();
    assert!(error.contains("fixture rejection"));
    assert!(error.contains("cleanup not confirmed"));
    assert!(browser.background_state.is_some());
    let cancelled = Cancellation::default();
    cancelled.cancel();
    let receipt = browser.background(false, &cancelled).unwrap();
    assert!(receipt.contains(r#""focus_emulation_cleared":true"#));
    assert!(browser.background_state.is_none());
    browser.close().unwrap();
    worker.join().unwrap();
}

fn focus_state(browser: &mut Browser, deadline: Instant) -> Result<String> {
    let result = browser.request_until("Runtime.evaluate",
        r#"{"expression":"({focused:document.hasFocus(),visibility:document.visibilityState})","returnByValue":true,"awaitPromise":false,"userGesture":false,"timeout":1000}"#,
        true, deadline, None)?;
    with_json(&result, |root| {
        if root.get("exceptionDetails").is_some() {
            return Err("fixture focus observation failed".into());
        }
        root.get("result")
            .and_then(|value| value.get("value"))
            .map(|value| value.raw().to_owned())
            .ok_or_else(|| "fixture focus observation has no value".into())
    })
}

fn require_focus_state(
    browser: &mut Browser,
    expected_focus: Option<bool>,
    expected_visibility: Option<&str>,
    phase: &str,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current =
            focus_state(browser, deadline).unwrap_or_else(|error| panic!("{phase}: {error}"));
        let matches = with_json(&current, |root| {
            let focused = root
                .get("focused")
                .and_then(Value::boolean)
                .ok_or("missing fixture focus")?;
            let visibility = root
                .get("visibility")
                .and_then(Value::string)
                .ok_or("missing fixture visibility")?;
            Ok(expected_focus.is_none_or(|expected| expected == focused)
                && expected_visibility.is_none_or(|expected| visibility.matches(expected)))
        })
        .unwrap();
        if matches {
            return current;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "{phase}: expected focus={expected_focus:?}, visibility={expected_visibility:?}; actual {current}"
        );
        thread::sleep(remaining.min(Duration::from_millis(20)));
    }
}

fn endpoint(browser: &Browser) -> String {
    let text =
        fs::read_to_string(browser.profile.as_ref().unwrap().join("DevToolsActivePort")).unwrap();
    let mut lines = text.lines();
    format!(
        "ws://127.0.0.1:{}{}",
        lines.next().unwrap(),
        lines.next().unwrap()
    )
}

fn input_fixture(title: &str) -> String {
    let html = format!(
        r#"<!doctype html><title>{title}</title><input id="entry"><script>window.events=[];for(const type of ['keydown','keyup'])addEventListener(type,event=>events.push({{type:event.type,key:event.key,trusted:event.isTrusted}}));</script>"#
    );
    format!("data:text/html;base64,{}", encode_base64(html.as_bytes()))
}

#[test]
#[ignore = "requires installed sandboxed Chromium; verifies two real fixture pages"]
fn native_background_keeps_trusted_input_on_selected_page_and_restores_focus() {
    let mut owner = Browser::launch(true).unwrap();
    owner.navigate(&input_fixture("background-target")).unwrap();
    let endpoint = endpoint(&owner);
    let mut foreground = Browser::connect(&endpoint).unwrap();
    foreground
        .navigate(&input_fixture("foreground-other"))
        .unwrap();
    foreground.request("Page.bringToFront", "{}", true).unwrap();
    let baseline = require_focus_state(&mut owner, Some(false), None, "target baseline");
    let baseline_visibility = field_string(&baseline, &["visibility"]).unwrap();
    assert!(
        matches!(baseline_visibility.as_str(), "hidden" | "visible"),
        "{baseline}"
    );
    let foreground_baseline =
        require_focus_state(&mut foreground, Some(true), None, "foreground baseline");
    let foreground_visibility = field_string(&foreground_baseline, &["visibility"]).unwrap();
    let mut selected = Browser::connect_existing(&endpoint, &owner.target).unwrap();

    // With no input, on/off must expose the original focus and visibility.
    selected.background(true, &Cancellation::default()).unwrap();
    require_focus_state(
        &mut owner,
        Some(true),
        Some("visible"),
        "emulation enabled without input",
    );
    let cleared = selected
        .background(false, &Cancellation::default())
        .unwrap();
    assert!(cleared.contains(r#""focus_emulation_cleared":true"#));
    require_focus_state(
        &mut owner,
        Some(false),
        Some(&baseline_visibility),
        "emulation cleared without input",
    );

    selected.background(true, &Cancellation::default()).unwrap();
    selected.validate_background_document().unwrap();
    let receipt = selected
        .hold_key("ArrowUp", 80, &Cancellation::default())
        .unwrap();
    assert!(receipt.contains(r#""released":true"#));
    selected.validate_background_document().unwrap();
    assert_eq!(selected.evaluate("events.length===2 && events[0].type==='keydown' && events[1].type==='keyup' && events.every(event=>event.key==='ArrowUp'&&event.trusted)").unwrap(), "true");
    assert_eq!(
        foreground
            .evaluate("document.hasFocus() && events.length===0")
            .unwrap(),
        "true"
    );
    let cleared = selected
        .background(false, &Cancellation::default())
        .unwrap();
    assert!(cleared.contains(r#""focus_emulation_cleared":true"#));
    assert!(selected.background_state.is_none());
    // Compare the observed baseline, rather than assuming every headless target
    // is a hidden tab. A hidden baseline must become hidden again after cleanup.
    require_focus_state(
        &mut owner,
        None,
        Some(&baseline_visibility),
        "visibility after input and clear",
    );

    // CDP keyboard injection calls RenderWidgetHost::Focus(). Removing focus
    // emulation does not undo that real widget state. Switch only these owned
    // fixture pages to produce a real blur, then insist focus is no longer
    // forced true. A leaked emulation override would still fail this assertion.
    selected.request("Page.bringToFront", "{}", true).unwrap();
    foreground.request("Page.bringToFront", "{}", true).unwrap();
    require_focus_state(
        &mut owner,
        Some(false),
        Some(&baseline_visibility),
        "cleared override permits real blur",
    );
    require_focus_state(
        &mut foreground,
        Some(true),
        Some(&foreground_visibility),
        "owned foreground restored",
    );
    assert_eq!(foreground.evaluate("events.length").unwrap(), "0");
    assert_eq!(
        selected
            .evaluate("events.length===2 && events[1].type==='keyup'")
            .unwrap(),
        "true"
    );

    selected.background(true, &Cancellation::default()).unwrap();
    selected
        .navigate(&input_fixture("background-next-document"))
        .unwrap();
    assert!(selected.background_state.is_none());
    require_focus_state(
        &mut selected,
        Some(false),
        Some(&baseline_visibility),
        "navigation clears override",
    );
    selected.background(true, &Cancellation::default()).unwrap();
    selected.select(&foreground.target).unwrap();
    assert!(selected.background_state.is_none());
    require_focus_state(
        &mut owner,
        Some(false),
        Some(&baseline_visibility),
        "selection clears old override",
    );
    require_focus_state(
        &mut foreground,
        Some(true),
        Some(&foreground_visibility),
        "selection preserves foreground",
    );
    assert_eq!(foreground.evaluate("events.length").unwrap(), "0");
    selected.close().unwrap();
    foreground.close().unwrap();
    owner.close().unwrap();
}
