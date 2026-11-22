//! Loopback protocol checks for canvas cleanup on page-endpoint connections.
//! The peer does not execute JavaScript: these tests verify the production
//! Browser lifecycle sends cleanup on the correct session, in the correct order.
use super::*;
use crate::observation::Cancellation;

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
        127 => panic!("unexpected oversized fixture request"),
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

fn reply(peer: &mut TcpStream, message: &str, result: &str) {
    let id = with_json(message, |root| {
        Ok(root.get("id").unwrap().integer().unwrap())
    })
    .unwrap();
    let text = format!(r#"{{"id":{id},"result":{result}}}"#);
    let mut frame = vec![0x81];
    if text.len() < 126 {
        frame.push(text.len() as u8);
    } else {
        assert!(text.len() <= usize::from(u16::MAX));
        frame.push(126);
        frame.extend_from_slice(&(text.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(text.as_bytes());
    peer.write_all(&frame).unwrap();
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
        // A completed cleanup clears ownership. close()/Drop must not dispatch
        // a second cleanup, and closing an attachment must not close its tab.
        let mut extra = [0; 1];
        match peer.read(&mut extra) {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("unexpected request after fixture completion: {result:?}"),
        }
    });
    (stream, worker)
}

fn enabled_page(peer: &mut TcpStream, session: Option<&str>) {
    for method in ["Page.enable", "Page.setLifecycleEventsEnabled"] {
        let message = request(peer, method, session);
        reply(peer, &message, "{}");
    }
}

fn canvas_operation(peer: &mut TcpStream, operation: &str, session: Option<&str>) {
    let message = request(peer, "Runtime.evaluate", session);
    let expression = field_string(&message, &["params", "expression"]).unwrap();
    assert!(expression.contains(&format!(r#""op":"{operation}""#)));
    assert!(expression.contains(r#""owner":"__zuse_observation_"#));
    with_json(&message, |root| {
        let params = root.get("params").unwrap();
        assert_eq!(params.get("awaitPromise").unwrap().boolean(), Some(false));
        assert_eq!(params.get("userGesture").unwrap().boolean(), Some(false));
        Ok(())
    })
    .unwrap();
    let receipt = if operation == "watch" {
        r#"{"ok":true,"status":"watching","active":true}"#
    } else {
        assert_eq!(operation, "cleanup");
        r#"{"ok":true,"status":"closed","closed":true,"restored":true,"restore_conflict":false}"#
    };
    reply(
        peer,
        &message,
        &format!(r#"{{"result":{{"value":{receipt}}}}}"#),
    );
}

#[test]
fn page_endpoint_close_and_drop_cleanup_canvas_without_a_target_id() {
    for implicit_drop in [false, true] {
        let (stream, worker) = fixture(|peer| {
            enabled_page(peer, None);
            canvas_operation(peer, "watch", None);
            canvas_operation(peer, "cleanup", None);
        });
        // This is the production direct-page constructor branch: it enables
        // the page without creating a target or attaching a flattened session.
        let mut browser = Browser::from_socket(WebSocket::new(stream), true, false).unwrap();
        assert!(browser.target.is_empty());
        assert!(browser.session.is_empty());
        browser
            .canvas_watch("#park-canvas", "parking", 5000, &Cancellation::default())
            .unwrap();
        assert!(browser.canvas_observer_owned);
        if implicit_drop {
            drop(browser);
        } else {
            browser.close().unwrap();
            assert!(!browser.canvas_observer_owned);
            browser.close().unwrap();
            drop(browser);
        }
        worker.join().unwrap();
    }
}

#[test]
fn selecting_another_page_cleans_the_old_session_before_detaching_it() {
    let (stream, worker) = fixture(|peer| {
        canvas_operation(peer, "watch", Some("old-session"));
        let info = request(peer, "Target.getTargetInfo", None);
        assert_eq!(
            field_string(&info, &["params", "targetId"]).unwrap(),
            "new-page"
        );
        reply(
            peer,
            &info,
            r#"{"targetInfo":{"targetId":"new-page","type":"page"}}"#,
        );
        let attach = request(peer, "Target.attachToTarget", None);
        reply(peer, &attach, r#"{"sessionId":"new-session"}"#);
        enabled_page(peer, Some("new-session"));
        canvas_operation(peer, "cleanup", Some("old-session"));
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
    browser
        .canvas_watch("#park-canvas", "parking", 5000, &Cancellation::default())
        .unwrap();
    browser.select("new-page").unwrap();
    assert_eq!(browser.target, "new-page");
    assert_eq!(browser.session, "new-session");
    assert!(!browser.canvas_observer_owned);
    browser.close().unwrap();
    worker.join().unwrap();
}

#[test]
fn navigation_cleans_canvas_before_replacing_the_document() {
    let (stream, worker) = fixture(|peer| {
        enabled_page(peer, None);
        canvas_operation(peer, "watch", None);
        canvas_operation(peer, "cleanup", None);
        let navigate = request(peer, "Page.navigate", None);
        assert_eq!(
            field_string(&navigate, &["params", "url"]).unwrap(),
            "https://fixture.invalid/next"
        );
        reply(peer, &navigate, "{}");
    });
    let mut browser = Browser::from_socket(WebSocket::new(stream), true, false).unwrap();
    browser
        .canvas_watch("#park-canvas", "parking", 5000, &Cancellation::default())
        .unwrap();
    browser.navigate("https://fixture.invalid/next").unwrap();
    assert!(!browser.canvas_observer_owned);
    browser.close().unwrap();
    worker.join().unwrap();
}
