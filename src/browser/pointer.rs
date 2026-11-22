//! Bounded viewport-coordinate input. All actions use trusted CDP events.
use super::*;
use crate::{drawing, motion::Point, observation::Cancellation};

const TRANSPORT_BUDGET: Duration = Duration::from_secs(2);
const CHECK_INTERVAL: Duration = Duration::from_millis(50);
const STATE: &str = r#"({url:location.href,timeOrigin:performance.timeOrigin,width:innerWidth,height:innerHeight,scale:visualViewport?visualViewport.scale:1,scrollX,scrollY,focused:document.hasFocus(),visible:document.visibilityState==='visible'})"#;

pub(crate) fn validate_point(point: Point) -> Result<()> {
    if [point.x, point.y]
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=crate::motion::MAX_COORDINATE).contains(value))
    {
        return Err(
            "browser coordinates must be finite, nonnegative CSS viewport coordinates".into(),
        );
    }
    Ok(())
}

pub(crate) fn button(button: &str) -> Result<(&'static str, u8)> {
    match button {
        "1" | "left" => Ok(("left", 1)),
        "2" | "middle" => Ok(("middle", 4)),
        "3" | "right" => Ok(("right", 2)),
        "back" => Ok(("back", 8)),
        "forward" => Ok(("forward", 16)),
        _ => Err("unknown browser mouse button".into()),
    }
}

pub(crate) fn validate_stroke(points: &[Point], duration_ms: u32) -> Result<()> {
    if !(2..=drawing::MAX_POINTS).contains(&points.len()) {
        return Err("a browser stroke requires 2..4096 points".into());
    }
    for point in points {
        validate_point(*point)?;
    }
    if duration_ms > 60_000 {
        return Err("browser stroke duration must be 0..60000ms".into());
    }
    if duration_ms != 0 && (duration_ms as usize) < points.len() - 1 {
        return Err("browser stroke duration must allow one millisecond per segment; use duration=0 for explicitly immediate input".into());
    }
    Ok(())
}

impl Browser {
    /// Click one point in CSS viewport coordinates without scrolling the page.
    /// A protocol acknowledgement confirms submission, not the application's effect.
    pub fn click_at(
        &mut self,
        x: f64,
        y: f64,
        button_name: &str,
        cancellation: &Cancellation,
    ) -> Result<String> {
        let point = Point { x, y };
        validate_point(point)?;
        button(button_name)?;
        self.pointer_path(&[point], 0, button_name, cancellation)
    }

    /// Drag through 2..4096 CSS viewport points, then release the owned button.
    /// The complete path and current viewport are checked before sending input.
    pub fn stroke(
        &mut self,
        points: &[Point],
        duration_ms: u32,
        button_name: &str,
        cancellation: &Cancellation,
    ) -> Result<String> {
        validate_stroke(points, duration_ms)?;
        button(button_name)?;
        self.pointer_path(points, duration_ms, button_name, cancellation)
    }

    fn pointer_state(&mut self, deadline: Instant, cancellation: &Cancellation) -> Result<String> {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if remaining == 0 {
            return Err("timeout: browser pointer deadline reached".into());
        }
        let result = self.request_until(
            "Runtime.evaluate",
            &format!(r#"{{"expression":{},"returnByValue":true,"awaitPromise":false,"timeout":{remaining}}}"#, quote(STATE)),
            true,
            deadline,
            Some(cancellation),
        )?;
        with_json(&result, |root| {
            if let Some(exception) = root.get("exceptionDetails") {
                return Err(format!(
                    "browser pointer observation failed: {}",
                    exception.raw()
                ));
            }
            root.get("result")
                .and_then(|value| value.get("value"))
                .map(|value| value.raw().to_owned())
                .ok_or_else(|| "browser pointer observation returned no state".into())
        })
    }

    fn pointer_path(
        &mut self,
        points: &[Point],
        duration_ms: u32,
        button_name: &str,
        cancellation: &Cancellation,
    ) -> Result<String> {
        cancellation.check()?;
        if self.mobile {
            return Err("unsupported_state: browser pointer paths require a desktop page with verifiable focus".into());
        }
        let began = Instant::now();
        let initial = self.pointer_state(began + TRANSPORT_BUDGET, cancellation)?;
        with_json(&initial, |root| {
            if root.get("focused").and_then(Value::boolean) != Some(true)
                || root.get("visible").and_then(Value::boolean) != Some(true)
            {
                return Err(
                    "stale_target: browser pointer input requires a visible, focused document"
                        .into(),
                );
            }
            let width = number(root.get("width"))?;
            let height = number(root.get("height"))?;
            if points
                .iter()
                .any(|point| point.x >= width || point.y >= height)
            {
                return Err("browser pointer path lies outside the current CSS viewport".into());
            }
            Ok(())
        })?;
        let (button, mask) = button(button_name)?;
        let mut input = PointerInput {
            browser: self,
            button,
            mask,
            point: points[0],
            armed: false,
            write_failed: false,
            pending: None,
        };
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(duration_ms)) + TRANSPORT_BUDGET;
        let result: Result<()> = (|| {
            input.dispatch("mouseMoved", points[0], deadline, cancellation)?;
            input.acknowledge(deadline, Some(cancellation))?;
            input.check_state(&initial, deadline, cancellation)?;
            input.dispatch("mousePressed", points[0], deadline, cancellation)?;
            // Clicks release before reading the press acknowledgement. A delayed
            // acknowledgement must never prolong an otherwise immediate click.
            if points.len() == 1 {
                return Ok(());
            }
            let start = Instant::now();
            input.acknowledge(deadline, Some(cancellation))?;
            let mut checked = Instant::now();
            for (index, point) in points.iter().enumerate().skip(1) {
                let at = start
                    + Duration::from_millis(
                        u64::from(duration_ms) * index as u64 / (points.len() - 1) as u64,
                    );
                while let Some(delay) = at.checked_duration_since(Instant::now()) {
                    cancellation.sleep(delay.min(CHECK_INTERVAL))?;
                    if checked.elapsed() >= CHECK_INTERVAL {
                        input.check_state(&initial, deadline, cancellation)?;
                        checked = Instant::now();
                    }
                }
                input.check_state(&initial, deadline, cancellation)?;
                checked = Instant::now();
                input.dispatch("mouseMoved", *point, deadline, cancellation)?;
                input.acknowledge(deadline, Some(cancellation))?;
            }
            Ok(())
        })();
        let released = input.release();
        match (result, released) {
            (Err(error), Ok(())) => return Err(error),
            (Ok(()), Err(error)) => return Err(error),
            (Err(error), Err(cleanup)) => return Err(format!("{error}; {cleanup}")),
            (Ok(()), Ok(())) => {}
        }
        cancellation.check()?;
        let point = points[points.len() - 1];
        Ok(format!(
            r#"{{"x":{},"y":{},"points":{},"duration_ms":{duration_ms},"elapsed_ms":{},"button":{},"button_released":true,"coordinate_space":"css_viewport","delivery":"submitted","application_confirmation_required":true}}"#,
            point.x,
            point.y,
            points.len(),
            began.elapsed().as_millis(),
            quote(button)
        ))
    }
}

/// Own only the button pressed by this operation, and retain at most one pending
/// input reply. Cleanup ignores cancellation and has its own protocol budget.
struct PointerInput<'a> {
    browser: &'a mut Browser,
    button: &'static str,
    mask: u8,
    point: Point,
    armed: bool,
    write_failed: bool,
    pending: Option<u64>,
}

impl PointerInput<'_> {
    fn check_state(
        &mut self,
        initial: &str,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<()> {
        cancellation.dispatch(|| Ok(()))?;
        let current = self.browser.pointer_state(
            deadline.min(Instant::now() + TRANSPORT_BUDGET),
            cancellation,
        )?;
        if current != initial {
            return Err("stale_target: browser document, viewport, scroll or focus changed during pointer input".into());
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        kind: &str,
        point: Point,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<()> {
        cancellation.dispatch(|| {
            let previous_armed = self.armed;
            let previous_point = self.point;
            if kind == "mousePressed" {
                self.armed = true;
            }
            self.point = point;
            let button = if self.armed { self.button } else { "none" };
            let mask = if self.armed { self.mask } else { 0 };
            let count = usize::from(kind == "mousePressed");
            match self.browser.send_request_until("Input.dispatchMouseEvent",
                &format!(r#"{{"type":"{kind}","x":{},"y":{},"button":"{button}","buttons":{mask},"clickCount":{count}}}"#, point.x, point.y),
                true, deadline.min(Instant::now() + TRANSPORT_BUDGET)) {
                Ok(id) => { self.pending = Some(id); Ok(()) }
                Err(error) => {
                    self.write_failed = self.browser.socket.failed.is_some();
                    if !self.write_failed {
                        // A pre-dispatch deadline/ID error wrote no frame.
                        // Keep the prior button ownership and last sent point
                        // so cleanup can release on the still usable socket.
                        self.armed = previous_armed;
                        self.point = previous_point;
                    }
                    Err(error)
                }
            }
        })
    }

    fn acknowledge(
        &mut self,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<()> {
        let Some(id) = self.pending else {
            return Ok(());
        };
        let result = self.browser.response_until(
            id,
            "Input.dispatchMouseEvent",
            deadline.min(Instant::now() + TRANSPORT_BUDGET),
            cancellation,
        );
        // Protocol errors consume their response; interrupted reads leave it
        // pending so release can drain it after sending mouseup first.
        if result.is_ok()
            || result
                .as_ref()
                .is_err_and(|error| error.starts_with("CDP Input.dispatchMouseEvent:"))
        {
            self.pending = None;
        }
        result.map(|_| ())
    }

    fn release(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.armed) {
            return Ok(());
        }
        if self.write_failed {
            return Err(
                "button release unconfirmed: browser connection failed during input".into(),
            );
        }
        let deadline = Instant::now() + TRANSPORT_BUDGET;
        let point = self.point;
        let button = self.button;
        let sent = self.browser.send_request_until("Input.dispatchMouseEvent",
            &format!(r#"{{"type":"mouseReleased","x":{},"y":{},"button":"{button}","buttons":0,"clickCount":1}}"#, point.x, point.y), true, deadline);
        let id = match sent {
            Ok(id) => id,
            Err(error) => {
                self.write_failed = true;
                let _ = self
                    .browser
                    .socket
                    .stream
                    .shutdown(std::net::Shutdown::Both);
                return Err(format!("button release unconfirmed: {error}"));
            }
        };
        let released = self
            .browser
            .response_until(id, "Input.dispatchMouseEvent", deadline, None);
        let preceding = self.acknowledge(deadline, None);
        match (preceding, released) {
            (Ok(()), Ok(_)) => Ok(()),
            (Err(error), Ok(_)) => Err(format!(
                "input acknowledgement failed; button release confirmed: {error}"
            )),
            (Ok(()), Err(error)) => Err(format!("button release unconfirmed: {error}")),
            (Err(input), Err(release)) => Err(format!(
                "input acknowledgement: {input}; button release unconfirmed: {release}"
            )),
        }
    }
}

impl Drop for PointerInput<'_> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"{"url":"https://fixture.invalid/","timeOrigin":1,"width":640,"height":480,"scale":1,"scrollX":0,"scrollY":0,"focused":true,"visible":true}"#;

    fn request(peer: &mut TcpStream) -> String {
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
        String::from_utf8(bytes).unwrap()
    }

    fn id(message: &str) -> u64 {
        with_json(message, |root| {
            Ok(root.get("id").unwrap().integer().unwrap() as u64)
        })
        .unwrap()
    }

    fn reply(peer: &mut TcpStream, id: u64, body: &str) {
        let text = format!(r#"{{"id":{id},{body}}}"#);
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

    fn setup(peer: &mut TcpStream) {
        for (method, kind) in [
            ("Runtime.evaluate", None),
            ("Input.dispatchMouseEvent", Some("mouseMoved")),
            ("Runtime.evaluate", None),
        ] {
            let message = request(peer);
            assert_eq!(field_string(&message, &["method"]).unwrap(), method);
            if let Some(kind) = kind {
                assert_eq!(field_string(&message, &["params", "type"]).unwrap(), kind);
                reply(peer, id(&message), r#""result":{}"#);
            } else {
                reply(
                    peer,
                    id(&message),
                    &format!(r#""result":{{"result":{{"value":{PAGE}}}}}"#),
                );
            }
        }
    }

    #[test]
    fn clicks_release_before_waiting_and_drain_out_of_order_replies() {
        for release_first in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let worker = thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                setup(&mut peer);
                let down = request(&mut peer);
                assert_eq!(
                    field_string(&down, &["params", "type"]).unwrap(),
                    "mousePressed"
                );
                let up = request(&mut peer);
                assert_eq!(
                    field_string(&up, &["params", "type"]).unwrap(),
                    "mouseReleased"
                );
                let order = if release_first {
                    [id(&up), id(&down)]
                } else {
                    [id(&down), id(&up)]
                };
                for id in order {
                    reply(&mut peer, id, r#""result":{}"#);
                }
            });
            let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
            let result = browser
                .click_at(10.5, 20.5, "right", &Cancellation::default())
                .unwrap();
            assert!(result.contains("\"button_released\":true"));
            assert!(browser.events.is_empty());
            browser.close().unwrap();
            worker.join().unwrap();
        }
    }

    #[test]
    fn expired_move_dispatch_keeps_the_socket_for_release_at_the_last_sent_point() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let worker = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let down = request(&mut peer);
            assert_eq!(
                field_string(&down, &["params", "type"]).unwrap(),
                "mousePressed"
            );
            reply(&mut peer, id(&down), r#""result":{}"#);
            let up = request(&mut peer);
            assert_eq!(
                field_string(&up, &["params", "type"]).unwrap(),
                "mouseReleased"
            );
            with_json(&up, |root| {
                let params = root.get("params").unwrap();
                assert_eq!(number(params.get("x"))?, 10.0);
                assert_eq!(number(params.get("y"))?, 20.0);
                Ok(())
            })
            .unwrap();
            reply(&mut peer, id(&up), r#""result":{}"#);
        });
        let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
        let cancellation = Cancellation::default();
        {
            let point = Point { x: 10.0, y: 20.0 };
            let mut input = PointerInput {
                browser: &mut browser,
                button: "left",
                mask: 1,
                point,
                armed: false,
                write_failed: false,
                pending: None,
            };
            let deadline = Instant::now() + TRANSPORT_BUDGET;
            input
                .dispatch("mousePressed", point, deadline, &cancellation)
                .unwrap();
            input.acknowledge(deadline, Some(&cancellation)).unwrap();
            let error = input
                .dispatch(
                    "mouseMoved",
                    Point { x: 30.0, y: 40.0 },
                    Instant::now() - Duration::from_millis(1),
                    &cancellation,
                )
                .unwrap_err();
            assert!(error.contains("deadline"), "{error}");
            assert!(input.browser.socket.failed.is_none());
            assert!(!input.write_failed);
            assert!(input.armed);
            assert_eq!(input.point, point);
            input.release().unwrap();
        }
        browser.close().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn cancellation_waiting_for_mousedown_still_releases_before_reading() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let worker = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            setup(&mut peer);
            let down = request(&mut peer);
            assert_eq!(
                field_string(&down, &["params", "type"]).unwrap(),
                "mousePressed"
            );
            cancel.cancel();
            let up = request(&mut peer);
            assert_eq!(
                field_string(&up, &["params", "type"]).unwrap(),
                "mouseReleased"
            );
            reply(&mut peer, id(&up), r#""result":{}"#);
            reply(&mut peer, id(&down), r#""result":{}"#);
        });
        let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
        let began = Instant::now();
        let error = browser
            .stroke(
                &[Point { x: 10.0, y: 20.0 }, Point { x: 30.0, y: 40.0 }],
                5000,
                "left",
                &cancellation,
            )
            .unwrap_err();
        assert!(error.starts_with("cancelled:"), "{error}");
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(browser.events.is_empty());
        browser.close().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn rejected_drag_move_releases_the_owned_button() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let worker = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            setup(&mut peer);
            let down = request(&mut peer);
            assert_eq!(
                field_string(&down, &["params", "type"]).unwrap(),
                "mousePressed"
            );
            reply(&mut peer, id(&down), r#""result":{}"#);
            loop {
                let message = request(&mut peer);
                if field_string(&message, &["method"]).unwrap() == "Runtime.evaluate" {
                    reply(
                        &mut peer,
                        id(&message),
                        &format!(r#""result":{{"result":{{"value":{PAGE}}}}}"#),
                    );
                    continue;
                }
                assert_eq!(
                    field_string(&message, &["params", "type"]).unwrap(),
                    "mouseMoved"
                );
                reply(
                    &mut peer,
                    id(&message),
                    r#""error":{"message":"move rejected"}"#,
                );
                break;
            }
            let up = request(&mut peer);
            assert_eq!(
                field_string(&up, &["params", "type"]).unwrap(),
                "mouseReleased"
            );
            reply(&mut peer, id(&up), r#""result":{}"#);
        });
        let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
        let error = browser
            .stroke(
                &[Point { x: 10.0, y: 20.0 }, Point { x: 30.0, y: 40.0 }],
                0,
                "left",
                &Cancellation::default(),
            )
            .unwrap_err();
        assert!(error.contains("move rejected"), "{error}");
        assert!(!error.contains("release unconfirmed"), "{error}");
        browser.close().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn changed_viewport_stops_drag_and_releases_without_another_move() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let worker = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            setup(&mut peer);
            let down = request(&mut peer);
            assert_eq!(
                field_string(&down, &["params", "type"]).unwrap(),
                "mousePressed"
            );
            reply(&mut peer, id(&down), r#""result":{}"#);
            let check = request(&mut peer);
            assert_eq!(
                field_string(&check, &["method"]).unwrap(),
                "Runtime.evaluate"
            );
            let changed = PAGE.replace("\"width\":640", "\"width\":800");
            reply(
                &mut peer,
                id(&check),
                &format!(r#""result":{{"result":{{"value":{changed}}}}}"#),
            );
            let up = request(&mut peer);
            assert_eq!(
                field_string(&up, &["params", "type"]).unwrap(),
                "mouseReleased"
            );
            reply(&mut peer, id(&up), r#""result":{}"#);
        });
        let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
        let error = browser
            .stroke(
                &[Point { x: 10.0, y: 20.0 }, Point { x: 30.0, y: 40.0 }],
                0,
                "left",
                &Cancellation::default(),
            )
            .unwrap_err();
        assert!(error.starts_with("stale_target:"), "{error}");
        assert!(!error.contains("release unconfirmed"), "{error}");
        browser.close().unwrap();
        worker.join().unwrap();
    }
}
