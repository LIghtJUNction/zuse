//! Direction-key holds stay within one selected document and focus target.
use super::*;
use crate::{
    key_hold::{self, Direction, Event},
    observation::Cancellation,
};

impl Browser {
    /// Hold one direction key for 1..10000ms without activating a browser tab.
    /// Cancellation, permission revocation and target changes release the key.
    pub fn hold_key(
        &mut self,
        key: &str,
        duration_ms: u64,
        cancellation: &Cancellation,
    ) -> Result<String> {
        let direction = Direction::parse(key)?;
        key_hold::validate_duration(duration_ms)?;
        cancellation.check()?;
        if self.mobile {
            return Err(
                "unsupported_state: browser key holds require a desktop page with verifiable focus"
                    .into(),
            );
        }
        let marker = format!(
            "__zuse_key_hold_{}_{}",
            std::process::id(),
            PROFILE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let install = format!(
            r#"(()=>{{if(!document.hasFocus()||document.visibilityState!=="visible")return false;const e=document.activeElement;if(!e||!e.isConnected)return false;globalThis[{}]={{document,element:e,url:location.href,width:innerWidth,height:innerHeight,scale:visualViewport?visualViewport.scale:1}};return true}})()"#,
            quote(&marker)
        );
        let setup_deadline = Instant::now() + Duration::from_secs(2);
        if self.hold_evaluate(&install, setup_deadline, Some(cancellation))? != "true" {
            return Err(
                "stale_target: key holds require a visible, focused browser document".into(),
            );
        }
        let check = format!(
            r#"(()=>{{const s=globalThis[{}];return !!s&&s.document===document&&s.url===location.href&&s.element.isConnected&&s.element===document.activeElement&&document.hasFocus()&&document.visibilityState==="visible"&&s.width===innerWidth&&s.height===innerHeight&&s.scale===(visualViewport?visualViewport.scale:1)}})()"#,
            quote(&marker)
        );
        let target = self.target.clone();
        let session = self.session.clone();
        let key = direction.browser();
        let vk = direction.virtual_key();
        let mut press_request = None;
        let mut write_failed = false;
        let result = key_hold::run(duration_ms, cancellation, |event| {
            match event {
                Event::Check { deadline } => {
                    if self.target != target || self.session != session {
                        return Err(
                            "stale_target: selected browser page changed during the key hold"
                                .into(),
                        );
                    }
                    if let Some(id) = press_request.take() {
                        self.response_until(
                            id,
                            "Input.dispatchKeyEvent",
                            deadline,
                            Some(cancellation),
                        )?;
                    }
                    if self.hold_evaluate(&check, deadline, Some(cancellation))? != "true" {
                        return Err("stale_target: browser document, geometry or focus target changed during the key hold".into());
                    }
                    Ok(())
                }
                Event::Press { deadline } => {
                    let sent = self.send_request_until("Input.dispatchKeyEvent",
                        &format!(r#"{{"type":"rawKeyDown","key":"{key}","code":"{key}","windowsVirtualKeyCode":{vk},"modifiers":0}}"#), true, deadline);
                    match sent {
                        Ok(id) => {
                            press_request = Some(id);
                            Ok(())
                        }
                        Err(error) => {
                            // A partial WebSocket frame cannot safely be followed
                            // by a second frame. Closing stops this owned stream.
                            write_failed = true;
                            let _ = self.socket.stream.shutdown(std::net::Shutdown::Both);
                            Err(error)
                        }
                    }
                }
                Event::Release { deadline } => {
                    if write_failed {
                        return Err("browser connection failed during keydown; release acknowledgement is unavailable".into());
                    }
                    let id = self.send_request_until("Input.dispatchKeyEvent",
                        &format!(r#"{{"type":"keyUp","key":"{key}","code":"{key}","windowsVirtualKeyCode":{vk},"modifiers":0}}"#), true, deadline)?;
                    // Cleanup deliberately ignores cancellation. Only the key
                    // owned by this hold is released in the original session.
                    // An expired hold may never have checked the keydown reply.
                    // Send keyup first, then consume both acknowledgements even
                    // if keydown failed. Either reply may arrive first.
                    let pressed = press_request
                        .take()
                        .map(|id| self.response_until(id, "Input.dispatchKeyEvent", deadline, None))
                        .transpose();
                    let released =
                        self.response_until(id, "Input.dispatchKeyEvent", deadline, None);
                    match (pressed, released) {
                        (Ok(_), Ok(_)) => Ok(()),
                        (Err(error), Ok(_)) => Err(format!(
                            "keydown acknowledgement failed; keyup confirmed: {error}"
                        )),
                        (Ok(_), Err(error)) => Err(error),
                        (Err(press), Err(release)) => {
                            Err(format!("keydown: {press}; keyup: {release}"))
                        }
                    }
                }
            }
        });
        if !write_failed {
            let cleanup = format!("delete globalThis[{}]", quote(&marker));
            let _ = self.hold_evaluate(&cleanup, Instant::now() + Duration::from_millis(500), None);
        }
        result.map(|elapsed| key_hold::receipt(direction, duration_ms, elapsed))
    }

    fn hold_evaluate(
        &mut self,
        expression: &str,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<String> {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if remaining == 0 {
            return Err("timeout: key hold deadline reached".into());
        }
        let result = self.request_until("Runtime.evaluate",
            &format!(r#"{{"expression":{},"returnByValue":true,"awaitPromise":false,"timeout":{remaining}}}"#, quote(expression)),
            true, deadline, cancellation)?;
        with_json(&result, |root| {
            if let Some(exception) = root.get("exceptionDetails") {
                return Err(format!("page JavaScript failed: {}", exception.raw()));
            }
            root.get("result")
                .and_then(|value| value.get("value"))
                .map(|value| value.raw().to_owned())
                .ok_or_else(|| "key hold target check returned no value".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn request(peer: &mut TcpStream, method: &str, kind: Option<&str>) -> u64 {
        let mut header = [0; 2];
        peer.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 0x81);
        assert_ne!(header[1] & 0x80, 0, "client frames must be masked");
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
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(field_string(&text, &["method"]).unwrap(), method);
        if let Some(kind) = kind {
            assert_eq!(field_string(&text, &["params", "type"]).unwrap(), kind);
            assert_eq!(field_string(&text, &["params", "key"]).unwrap(), "ArrowUp");
        }
        with_json(&text, |root| {
            Ok(root.get("id").unwrap().integer().unwrap() as u64)
        })
        .unwrap()
    }

    fn reply(peer: &mut TcpStream, id: u64, body: &str) {
        let text = format!(r#"{{"id":{id},{body}}}"#);
        assert!(text.len() < 126);
        let mut frame = vec![0x81, text.len() as u8];
        frame.extend_from_slice(text.as_bytes());
        peer.write_all(&frame).unwrap();
    }

    #[test]
    fn expired_hold_releases_before_reading_and_preserves_both_key_replies() {
        for (down_ok, up_ok) in [(false, true), (true, false), (false, false), (true, true)] {
            for up_first in [false, true] {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                let server = thread::spawn(move || {
                    let (mut peer, _) = listener.accept().unwrap();
                    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    for _ in 0..2 {
                        let id = request(&mut peer, "Runtime.evaluate", None);
                        reply(&mut peer, id, r#""result":{"result":{"value":true}}"#);
                    }
                    let down = request(&mut peer, "Input.dispatchKeyEvent", Some("rawKeyDown"));
                    // Neither acknowledgement is available until keyup arrives.
                    // Waiting for keydown before releasing would deadlock here.
                    let up = request(&mut peer, "Input.dispatchKeyEvent", Some("keyUp"));
                    let down_reply = if down_ok {
                        r#""result":{}"#
                    } else {
                        r#""error":{"message":"keydown rejected"}"#
                    };
                    let up_reply = if up_ok {
                        r#""result":{}"#
                    } else {
                        r#""error":{"message":"keyup rejected"}"#
                    };
                    let replies = if up_first {
                        [(up, up_reply), (down, down_reply)]
                    } else {
                        [(down, down_reply), (up, up_reply)]
                    };
                    for (id, body) in replies {
                        reply(&mut peer, id, body);
                    }
                    let cleanup = request(&mut peer, "Runtime.evaluate", None);
                    reply(&mut peer, cleanup, r#""result":{"result":{"value":true}}"#);
                });
                let acquired = AtomicUsize::new(0);
                let cancellation = Cancellation::with_dispatch_guard(move || {
                    // Delay only the first permit recheck after keydown. This
                    // deterministically reaches the expired-duration branch
                    // without consuming the pending keydown in Event::Check.
                    if acquired.fetch_add(1, Ordering::Relaxed) == 1 {
                        thread::sleep(Duration::from_millis(75));
                    }
                    Ok(Box::new(()))
                });
                let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
                let result = browser.hold_key("Up", 50, &cancellation);
                browser.close().unwrap();
                server.join().unwrap();
                let case = format!("down_ok={down_ok}, up_ok={up_ok}, up_first={up_first}");
                if down_ok && up_ok {
                    assert!(result.is_ok(), "{case}: {result:?}");
                } else {
                    let error = result.expect_err(&case);
                    assert_eq!(
                        error.contains("keydown rejected"),
                        !down_ok,
                        "{case}: {error}"
                    );
                    assert_eq!(error.contains("keyup rejected"), !up_ok, "{case}: {error}");
                }
                assert!(browser.events.is_empty(), "unconsumed reply: {case}");
            }
        }
    }
}
