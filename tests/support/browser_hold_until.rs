// Included by browser::key_hold::tests to reuse its framed CDP peer helpers.

#[test]
fn hold_until_initial_condition_does_not_press_and_requires_a_boolean() {
    for (value, success) in [("2", true), ("3", false)] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        stream.set_nodelay(true).unwrap();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_nodelay(true).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let install = request(&mut peer, "Runtime.evaluate", None);
            reply(&mut peer, install, r#""result":{"result":{"value":true}}"#);
            let condition = request(&mut peer, "Runtime.evaluate", None);
            reply(&mut peer, condition, &format!(r#""result":{{"result":{{"value":{value}}}}}"#));
            // No input request is allowed between precheck and marker cleanup.
            let cleanup = request(&mut peer, "Runtime.evaluate", None);
            reply(&mut peer, cleanup, r#""result":{"result":{"value":true}}"#);
        });
        let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
        let result = browser.hold_until("Up,Right", "document.body.dataset.ready === 'yes'", 1000, &Cancellation::default());
        browser.close().unwrap();
        server.join().unwrap();
        if success {
            let receipt = result.unwrap();
            assert!(receipt.contains(r#""reason":"condition""#), "{receipt}");
            assert!(receipt.contains(r#""held_ms":0"#), "{receipt}");
            assert!(receipt.contains(r#""pressed":false"#), "{receipt}");
        } else {
            assert!(result.unwrap_err().contains("must evaluate to a boolean"));
        }
        assert!(browser.events.is_empty());
    }
}

#[test]
fn hold_until_releases_both_keys_for_condition_error_change_cancel_and_timeout() {
    for mode in ["condition", "navigation", "protocol_error", "cancelled", "late_condition", "late_error", "release_error"] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        stream.set_nodelay(true).unwrap();
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_nodelay(true).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let install = request(&mut peer, "Runtime.evaluate", None);
            reply(&mut peer, install, r#""result":{"result":{"value":true}}"#);
            let initial = request(&mut peer, "Runtime.evaluate", None);
            reply(&mut peer, initial, r#""result":{"result":{"value":1}}"#);
            let down_up = request_key(&mut peer, "Input.dispatchKeyEvent", Some("rawKeyDown"), "ArrowUp");
            let down_right = request_key(&mut peer, "Input.dispatchKeyEvent", Some("rawKeyDown"), "ArrowRight");
            let pressed = Instant::now();
            reply(&mut peer, down_up, r#""result":{}"#);
            reply(&mut peer, down_right, r#""result":{}"#);
            let mut check = request(&mut peer, "Runtime.evaluate", None);
            if matches!(mode, "condition" | "release_error") {
                // First observation stays false; the next public observation
                // changes, so success must end a real ongoing hold.
                reply(&mut peer, check, r#""result":{"result":{"value":1}}"#);
                check = request(&mut peer, "Runtime.evaluate", None);
            }
            match mode {
                "condition" | "release_error" => reply(&mut peer, check, r#""result":{"result":{"value":2}}"#),
                "navigation" => reply(&mut peer, check, r#""result":{"result":{"value":0}}"#),
                "protocol_error" => reply(&mut peer, check, r#""error":{"message":"observable condition rejected"}"#),
                "cancelled" => {
                    cancel.cancel();
                    reply(&mut peer, check, r#""result":{"result":{"value":1}}"#);
                }
                // A condition first returned after timeout cannot be promoted
                // to successful completion by the cleanup response drain.
                "late_condition" | "late_error" => {}
                _ => unreachable!(),
            }
            let up_up = request_key(&mut peer, "Input.dispatchKeyEvent", Some("keyUp"), "ArrowUp");
            let up_right = request_key(&mut peer, "Input.dispatchKeyEvent", Some("keyUp"), "ArrowRight");
            assert!(pressed.elapsed() < Duration::from_secs(1), "release waited for a pending observation: {mode}");
            reply(&mut peer, up_right, if mode == "release_error" {
                r#""error":{"message":"second release rejected"}"#
            } else { r#""result":{}"# });
            reply(&mut peer, up_up, r#""result":{}"#);
            if mode == "late_condition" {
                reply(&mut peer, check, r#""result":{"result":{"value":2}}"#);
            } else if mode == "late_error" {
                reply(&mut peer, check, r#""error":{"message":"late condition protocol failure"}"#);
            }
            let cleanup = request(&mut peer, "Runtime.evaluate", None);
            reply(&mut peer, cleanup, r#""result":{"result":{"value":true}}"#);
        });
        let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
        let result = browser.hold_until("ArrowUp,ArrowRight", "globalThis.visibleObserver.ready === true", 250, &cancellation);
        browser.close().unwrap();
        server.join().unwrap();
        match mode {
            "condition" => {
                let receipt = result.unwrap();
                assert!(receipt.contains(r#""reason":"condition""#), "{receipt}");
                assert!(receipt.contains(r#""pressed":true"#), "{receipt}");
                assert!(receipt.contains(r#""released":true"#), "{receipt}");
            }
            "navigation" => assert!(result.unwrap_err().starts_with("stale_target:")),
            "protocol_error" => assert!(result.unwrap_err().contains("observable condition rejected")),
            "cancelled" => assert!(result.unwrap_err().starts_with("cancelled:")),
            "late_condition" => assert!(result.unwrap_err().starts_with("timeout:")),
            "late_error" => assert!(result.unwrap_err().contains("late condition protocol failure")),
            "release_error" => assert!(result.unwrap_err().contains("second release rejected")),
            _ => unreachable!(),
        }
        assert!(browser.events.is_empty(), "unconsumed reply for {mode}");
    }
}
