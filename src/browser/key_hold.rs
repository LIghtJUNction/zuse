//! Direction-key holds stay within one selected document and focus target.
use super::*;
use crate::{
    key_hold::{self, Direction, Event},
    observation::Cancellation,
};

fn directions(keys: &str) -> Result<Vec<Direction>> {
    if keys.is_empty() || keys.len() > 64 {
        return Err("browser.hold_keys requires one or two comma-separated direction keys".into());
    }
    let mut parsed = Vec::with_capacity(2);
    for key in keys.split(',') {
        if parsed.len() == 2 {
            return Err("browser.hold_keys permits at most two direction keys".into());
        }
        let direction = Direction::parse(key.trim())?;
        if parsed.contains(&direction) {
            return Err("browser.hold_keys requires unique direction keys".into());
        }
        if parsed.iter().any(|previous| {
            matches!(
                (*previous, direction),
                (Direction::Up, Direction::Down)
                    | (Direction::Down, Direction::Up)
                    | (Direction::Left, Direction::Right)
                    | (Direction::Right, Direction::Left)
            )
        }) {
            return Err("browser.hold_keys does not permit opposite directions".into());
        }
        parsed.push(direction);
    }
    Ok(parsed)
}

const CONDITION_MET: &str = "browser.hold_until: observed condition satisfied";
const CONDITION_TIMEOUT: &str =
    "timeout: browser.hold_until condition was not observed before its deadline";

fn validate_condition(condition: &str) -> Result<()> {
    if condition.trim().is_empty() || condition.len() > 16_384 || condition.contains('\0') {
        return Err("browser.hold_until condition must contain 1..16384 non-NUL bytes".into());
    }
    Ok(())
}

fn checked_condition(value: &str, until: bool) -> Result<bool> {
    if until {
        match value {
            "1" => Ok(false),
            "2" => Ok(true),
            "0" => Err("stale_target: browser document, geometry or focus target changed during the key hold".into()),
            _ => Err("browser.hold_until condition must evaluate to a boolean".into()),
        }
    } else if value == "true" {
        Ok(false)
    } else {
        Err(
            "stale_target: browser document, geometry or focus target changed during the key hold"
                .into(),
        )
    }
}

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
        let elapsed = self.hold_directions(&[direction], duration_ms, cancellation)?;
        Ok(key_hold::receipt(direction, duration_ms, elapsed))
    }

    /// Hold one or two unique, non-opposite directions under one bounded hold.
    /// Both presses share a dispatch permit; no acknowledgement separates them.
    pub fn hold_keys(
        &mut self,
        keys: &str,
        duration_ms: u64,
        cancellation: &Cancellation,
    ) -> Result<String> {
        let directions = directions(keys)?;
        let elapsed = self.hold_directions(&directions, duration_ms, cancellation)?;
        let keys = directions
            .iter()
            .map(|key| quote(key.browser()))
            .collect::<Vec<_>>()
            .join(",");
        Ok(format!(
            r#"{{"ok":true,"keys":[{keys}],"duration_ms":{duration_ms},"held_ms":{},"released":true,"delivery":"submitted","application_confirmation_required":true}}"#,
            elapsed.as_millis()
        ))
    }

    pub(crate) fn validate_hold_keys(keys: &str) -> Result<()> {
        directions(keys).map(|_| ())
    }

    /// Stop a bounded trusted key hold at an explicitly observed condition.
    /// The caller supplies a boolean expression over visible DOM/its observer.
    pub fn hold_until(
        &mut self,
        keys: &str,
        condition: &str,
        timeout_ms: u64,
        cancellation: &Cancellation,
    ) -> Result<String> {
        let directions = directions(keys)?;
        validate_condition(condition)?;
        let (elapsed, pressed) =
            self.hold_directions_until(&directions, timeout_ms, Some(condition), cancellation)?;
        let keys = directions
            .iter()
            .map(|key| quote(key.browser()))
            .collect::<Vec<_>>()
            .join(",");
        Ok(format!(
            r#"{{"ok":true,"keys":[{keys}],"reason":"condition","timeout_ms":{timeout_ms},"held_ms":{},"pressed":{pressed},"released":true,"delivery":"{}","application_confirmation_required":true}}"#,
            elapsed.as_millis(),
            if pressed { "submitted" } else { "none" }
        ))
    }

    pub(crate) fn validate_hold_until(keys: &str, condition: &str) -> Result<()> {
        directions(keys)?;
        validate_condition(condition)
    }

    fn hold_directions(
        &mut self,
        directions: &[Direction],
        duration_ms: u64,
        cancellation: &Cancellation,
    ) -> Result<Duration> {
        self.hold_directions_until(directions, duration_ms, None, cancellation)
            .map(|(elapsed, _)| elapsed)
    }

    fn hold_directions_until(
        &mut self,
        directions: &[Direction],
        duration_ms: u64,
        condition: Option<&str>,
        cancellation: &Cancellation,
    ) -> Result<(Duration, bool)> {
        key_hold::validate_duration(duration_ms)?;
        cancellation.check()?;
        if self.mobile {
            return Err(
                "unsupported_state: browser key holds require a desktop page with verifiable focus"
                    .into(),
            );
        }
        let until_deadline = condition.map(|_| Instant::now() + Duration::from_millis(duration_ms));
        let marker = format!(
            "__zuse_key_hold_{}_{}",
            std::process::id(),
            PROFILE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let install = super::control_keys::target_install(&marker);
        let setup_deadline = until_deadline
            .map_or(Instant::now() + Duration::from_secs(2), |deadline| {
                deadline.min(Instant::now() + Duration::from_secs(2))
            });
        if self.hold_evaluate(&install, setup_deadline, Some(cancellation))? != "true" {
            return Err(
                "stale_target: key holds require a visible, focused browser document".into(),
            );
        }
        let check = super::control_keys::target_check(&marker);
        let check = match condition {
            Some(expression) => format!(
                r#"(()=>{{if(!({check}))return 0;const value=({expression});return typeof value==="boolean"?(value?2:1):3}})()"#
            ),
            None => check,
        };
        // The condition and retained target are checked before any input.
        // Count this observation against the total hold_until timeout. Its
        // immediately preceding result supplies run()'s initial target check.
        let mut skip_initial_check = false;
        let run_ms = if let Some(deadline) = until_deadline {
            let read_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
            let mut initial_pending = None;
            let observed = self
                .send_hold_evaluation(&check, read_deadline, Some(cancellation))
                .and_then(|id| {
                    initial_pending = Some(id);
                    let read = self.read_hold_evaluation(id, read_deadline, Some(cancellation));
                    if read.is_ok()
                        || read.as_ref().is_err_and(|error| {
                            !error.starts_with("cancelled:")
                                && !error.starts_with("CDP Runtime.evaluate read:")
                        })
                    {
                        initial_pending = None;
                    }
                    read.and_then(|value| checked_condition(&value, true))
                });
            let now = Instant::now();
            let remaining = deadline.saturating_duration_since(now).as_millis() as u64;
            if !matches!(&observed, Ok(false)) || remaining == 0 {
                let cleanup = format!("delete globalThis[{}]", quote(&marker));
                if self.socket.failed.is_none() {
                    let cleanup_deadline = Instant::now() + Duration::from_millis(500);
                    if let Some(id) = initial_pending {
                        let _ = self.read_hold_evaluation(id, cleanup_deadline, None);
                    }
                    let _ = self.hold_evaluate(&cleanup, cleanup_deadline, None);
                }
                return match observed {
                    Ok(true) if now < deadline => Ok((Duration::ZERO, false)),
                    Ok(_) => Err(CONDITION_TIMEOUT.into()),
                    Err(error) => Err(error),
                };
            }
            skip_initial_check = true;
            remaining
        } else {
            duration_ms
        };
        let target = self.target.clone();
        let session = self.session.clone();
        let mut press_requests = vec![None; directions.len()];
        let mut attempted = vec![false; directions.len()];
        let mut pending_check = None;
        let mut condition_met = false;
        let mut pressed_at = None;
        let mut held = Duration::ZERO;
        let run_result = key_hold::run(run_ms, cancellation, |event| {
            match event {
                Event::Check { deadline } => {
                    let deadline = until_deadline.map_or(deadline, |until| until.min(deadline));
                    if self.target != target || self.session != session {
                        return Err(
                            "stale_target: selected browser page changed during the key hold"
                                .into(),
                        );
                    }
                    if std::mem::take(&mut skip_initial_check) {
                        return Ok(());
                    }
                    for request in &mut press_requests {
                        if let Some(id) = *request {
                            let acknowledged = self.response_until(
                                id,
                                "Input.dispatchKeyEvent",
                                deadline,
                                Some(cancellation),
                            );
                            // A protocol error consumes its reply; interrupted
                            // reads retain it for release to drain afterwards.
                            if acknowledged.is_ok()
                                || acknowledged.as_ref().is_err_and(|error| {
                                    error.starts_with("CDP Input.dispatchKeyEvent:")
                                })
                            {
                                *request = None;
                            }
                            acknowledged?;
                        }
                    }
                    let holding = attempted.iter().any(|attempt| *attempt);
                    if holding && Instant::now() >= deadline {
                        return if condition.is_some() {
                            Err(CONDITION_TIMEOUT.into())
                        } else {
                            Ok(())
                        };
                    }
                    let read_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
                    let id = match self.send_hold_evaluation(
                        &check,
                        read_deadline,
                        Some(cancellation),
                    ) {
                        Ok(id) => id,
                        Err(error)
                            if holding
                                && Instant::now() >= deadline
                                && matches!(
                                    error.as_str(),
                                    "timeout: key hold deadline reached"
                                        | "browser dispatch deadline timed out"
                                ) =>
                        {
                            return if condition.is_some() {
                                Err(CONDITION_TIMEOUT.into())
                            } else {
                                Ok(())
                            };
                        }
                        Err(error) => return Err(error),
                    };
                    pending_check = Some(id);
                    let checked = self.read_hold_evaluation(id, read_deadline, Some(cancellation));
                    if checked.is_ok()
                        || checked.as_ref().is_err_and(|error| {
                            !error.starts_with("cancelled:")
                                && !error.starts_with("CDP Runtime.evaluate read:")
                        })
                    {
                        pending_check = None;
                    }
                    match checked {
                        Ok(value) => {
                            if checked_condition(&value, condition.is_some())? {
                                if Instant::now() >= deadline {
                                    return Err(CONDITION_TIMEOUT.into());
                                }
                                condition_met = true;
                                return Err(CONDITION_MET.into());
                            }
                        }
                        Err(error)
                            if holding
                                && read_deadline == deadline
                                && Instant::now() >= deadline
                                && self.socket.failed.is_none()
                                && error
                                    == "CDP Runtime.evaluate read: browser response deadline timed out" =>
                        {
                            // The hold has ended while the last read is pending.
                            // Release on time, then check this reply under the
                            // separate cleanup budget before claiming success.
                            if condition.is_some() {
                                return Err(CONDITION_TIMEOUT.into());
                            }
                        }
                        Err(error) => return Err(error),
                    }
                    if condition.is_some() {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining <= Duration::from_millis(20) {
                            // Do not let run()'s next polling sleep extend an
                            // absolute until deadline established before setup.
                            cancellation.sleep(remaining)?;
                            return Err(CONDITION_TIMEOUT.into());
                        }
                    }
                    Ok(())
                }
                Event::Press { deadline } => {
                    let deadline = until_deadline.map_or(deadline, |until| until.min(deadline));
                    if condition.is_some() && Instant::now() >= deadline {
                        return Err(CONDITION_TIMEOUT.into());
                    }
                    pressed_at = Some(Instant::now());
                    for (index, direction) in directions.iter().enumerate() {
                        cancellation.check()?;
                        let key = direction.browser();
                        let vk = direction.virtual_key();
                        attempted[index] = true;
                        match self.send_request_until("Input.dispatchKeyEvent",
                            &format!(r#"{{"type":"rawKeyDown","key":"{key}","code":"{key}","windowsVirtualKeyCode":{vk},"modifiers":0}}"#), true, deadline)
                        {
                            Ok(id) => press_requests[index] = Some(id),
                            Err(error) => {
                                // A pre-dispatch deadline/ID error sent nothing.
                                // A failed frame write has already poisoned the
                                // socket; release remains explicitly unconfirmed.
                                if self.socket.failed.is_none() {
                                    attempted[index] = false;
                                }
                                return Err(error);
                            }
                        }
                    }
                    Ok(())
                }
                Event::Release { deadline } => {
                    held = pressed_at.map_or(Duration::ZERO, |start: Instant| start.elapsed());
                    let mut errors = Vec::new();
                    let mut releases = Vec::with_capacity(directions.len());
                    // Send every owned keyup before awaiting any reply. A
                    // cancelled or rejected first key cannot strand its peer.
                    for (index, direction) in directions.iter().enumerate() {
                        if !attempted[index] {
                            continue;
                        }
                        let key = direction.browser();
                        let vk = direction.virtual_key();
                        match self.send_request_until("Input.dispatchKeyEvent",
                            &format!(r#"{{"type":"keyUp","key":"{key}","code":"{key}","windowsVirtualKeyCode":{vk},"modifiers":0}}"#), true, deadline)
                        {
                            Ok(id) => releases.push((key, id)),
                            Err(error) => errors.push(format!("{key} release unconfirmed: {error}")),
                        }
                    }
                    // Cleanup deliberately ignores cancellation. Pending presses
                    // and all releases are consumed even after a sibling error.
                    for (index, request) in press_requests.iter_mut().enumerate() {
                        if let Some(id) = request.take()
                            && let Err(error) =
                                self.response_until(id, "Input.dispatchKeyEvent", deadline, None)
                        {
                            errors
                                .push(format!("{} keydown: {error}", directions[index].browser()));
                        }
                    }
                    for (key, id) in releases {
                        if let Err(error) =
                            self.response_until(id, "Input.dispatchKeyEvent", deadline, None)
                        {
                            errors.push(format!("{key} release unconfirmed: {error}"));
                        }
                    }
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(errors.join("; "))
                    }
                }
            }
        });
        let mut result = match run_result {
            Ok(_) if condition.is_some() => Err(CONDITION_TIMEOUT.into()),
            Err(error) if condition_met && error == CONDITION_MET => Ok(held),
            Err(error) if condition_met && error.starts_with(CONDITION_MET) => Err(format!(
                "condition observed{}",
                &error[CONDITION_MET.len()..]
            )),
            result => result,
        };
        if self.socket.failed.is_none() {
            let cleanup_deadline = Instant::now() + Duration::from_millis(500);
            if let Some(id) = pending_check {
                let checked = self
                    .read_hold_evaluation(id, cleanup_deadline, None)
                    .and_then(|value| checked_condition(&value, condition.is_some()));
                if let Err(error) = checked {
                    if result.is_ok() {
                        result = Err(format!(
                            "final browser hold target check unconfirmed after key release: {error}"
                        ));
                    } else if result
                        .as_ref()
                        .is_err_and(|error| error == CONDITION_TIMEOUT)
                    {
                        result = Err(format!(
                            "{CONDITION_TIMEOUT}; final target check also failed: {error}"
                        ));
                    }
                }
            }
            let cleanup = format!("delete globalThis[{}]", quote(&marker));
            let _ = self.hold_evaluate(&cleanup, cleanup_deadline, None);
        }
        result.map(|elapsed| (elapsed, pressed_at.is_some()))
    }

    fn hold_evaluate(
        &mut self,
        expression: &str,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<String> {
        let id = self.send_hold_evaluation(expression, deadline, cancellation)?;
        self.read_hold_evaluation(id, deadline, cancellation)
    }

    fn send_hold_evaluation(
        &mut self,
        expression: &str,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<u64> {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if remaining == 0 {
            return Err("timeout: key hold deadline reached".into());
        }
        self.send_request_until("Runtime.evaluate",
            &format!(r#"{{"expression":{},"returnByValue":true,"awaitPromise":false,"timeout":{remaining}}}"#, quote(expression)),
            true, deadline)
    }

    fn read_hold_evaluation(
        &mut self,
        id: u64,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<String> {
        let result = self.response_until(id, "Runtime.evaluate", deadline, cancellation)?;
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

    include!("../../tests/support/browser_hold_until.rs");

    fn request(peer: &mut TcpStream, method: &str, kind: Option<&str>) -> u64 {
        request_key(peer, method, kind, "ArrowUp")
    }

    fn request_key(peer: &mut TcpStream, method: &str, kind: Option<&str>, key: &str) -> u64 {
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
            assert_eq!(field_string(&text, &["params", "key"]).unwrap(), key);
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

    #[test]
    fn paired_holds_release_both_keys_before_any_ack_even_after_rejection_or_cancel() {
        for mode in ["success", "rejected", "cancelled"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let acquired = AtomicUsize::new(0);
            let cancellation = Cancellation::with_dispatch_guard(move || {
                if acquired.fetch_add(1, Ordering::Relaxed) == 1 {
                    thread::sleep(Duration::from_millis(75));
                }
                Ok(Box::new(()))
            });
            let cancel = cancellation.clone();
            let server = thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                for _ in 0..2 {
                    let id = request(&mut peer, "Runtime.evaluate", None);
                    reply(&mut peer, id, r#""result":{"result":{"value":true}}"#);
                }
                let down_up = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("rawKeyDown"),
                    "ArrowUp",
                );
                let down_right = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("rawKeyDown"),
                    "ArrowRight",
                );
                if mode == "cancelled" {
                    cancel.cancel();
                }
                let up_up = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("keyUp"),
                    "ArrowUp",
                );
                let up_right = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("keyUp"),
                    "ArrowRight",
                );
                // No press reply is available until both releases arrive.
                // Deliver all four acknowledgements in a different order.
                reply(&mut peer, up_right, r#""result":{}"#);
                reply(&mut peer, down_up, r#""result":{}"#);
                reply(&mut peer, up_up, r#""result":{}"#);
                reply(
                    &mut peer,
                    down_right,
                    if mode == "rejected" {
                        r#""error":{"message":"second keydown rejected"}"#
                    } else {
                        r#""result":{}"#
                    },
                );
                let cleanup = request(&mut peer, "Runtime.evaluate", None);
                reply(&mut peer, cleanup, r#""result":{"result":{"value":true}}"#);
            });
            let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
            let result = browser.hold_keys("ArrowUp,ArrowRight", 50, &cancellation);
            browser.close().unwrap();
            server.join().unwrap();
            match mode {
                "success" => assert!(
                    result
                        .unwrap()
                        .contains(r#""keys":["ArrowUp","ArrowRight"]"#)
                ),
                "rejected" => {
                    let error = result.unwrap_err();
                    assert!(error.contains("second keydown rejected"), "{error}");
                    assert!(!error.contains("release unconfirmed"), "{error}");
                }
                "cancelled" => assert!(result.unwrap_err().starts_with("cancelled:")),
                _ => unreachable!(),
            }
            assert!(browser.events.is_empty(), "unconsumed reply: {mode}");
        }
    }

    #[test]
    fn final_check_reply_cannot_delay_release_or_hide_a_protocol_error() {
        for rejected in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            stream.set_nodelay(true).unwrap();
            let server = thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_nodelay(true).unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                for _ in 0..2 {
                    let id = request(&mut peer, "Runtime.evaluate", None);
                    reply(&mut peer, id, r#""result":{"result":{"value":true}}"#);
                }
                let down_up = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("rawKeyDown"),
                    "ArrowUp",
                );
                let pressed = Instant::now();
                let down_right = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("rawKeyDown"),
                    "ArrowRight",
                );
                reply(&mut peer, down_up, r#""result":{}"#);
                reply(&mut peer, down_right, r#""result":{}"#);
                let check = request(&mut peer, "Runtime.evaluate", None);
                // The final observation cannot complete before the hold ends.
                // Both keys must be released before we return that response.
                let up_up = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("keyUp"),
                    "ArrowUp",
                );
                let up_right = request_key(
                    &mut peer,
                    "Input.dispatchKeyEvent",
                    Some("keyUp"),
                    "ArrowRight",
                );
                let held = pressed.elapsed();
                assert!(
                    held >= Duration::from_millis(100),
                    "released early: {held:?}"
                );
                assert!(held < Duration::from_secs(1), "late release: {held:?}");
                reply(&mut peer, up_right, r#""result":{}"#);
                reply(&mut peer, up_up, r#""result":{}"#);
                reply(
                    &mut peer,
                    check,
                    if rejected {
                        r#""error":{"message":"final target check rejected"}"#
                    } else {
                        r#""result":{"result":{"value":true}}"#
                    },
                );
                let cleanup = request(&mut peer, "Runtime.evaluate", None);
                reply(&mut peer, cleanup, r#""result":{"result":{"value":true}}"#);
            });
            let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
            let result = browser.hold_keys("ArrowUp,ArrowRight", 150, &Cancellation::default());
            browser.close().unwrap();
            server.join().unwrap();
            if rejected {
                let error = result.unwrap_err();
                assert!(error.contains("final target check rejected"), "{error}");
                assert!(!error.contains("release unconfirmed"), "{error}");
            } else {
                assert!(result.is_ok(), "healthy delayed check: {result:?}");
            }
            assert!(
                browser.events.is_empty(),
                "unconsumed reply; rejected={rejected}"
            );
        }
    }
}
