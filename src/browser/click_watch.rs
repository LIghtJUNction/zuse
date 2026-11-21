//! Explicit, bounded DOM-event-to-CDP-input loop for the already selected page.
use super::*;
use crate::observation::{Cancellation, ClickWatchOptions};

struct Dispatch {
    episode: u64,
    element: String,
    details: String,
    received: Option<String>,
    release_request: Option<u64>,
    release_confirmed: bool,
    release_error: Option<String>,
}

impl Browser {
    /// Wait locally for one unique visible target and click it using trusted
    /// CDP mouse input. Repeated mutations do not click the same visible episode
    /// again; only an observed gone-to-visible transition opens a new episode.
    /// Observation methods remain read-only. No page activation is performed.
    pub fn watch_until_click(
        &mut self,
        options: &ClickWatchOptions,
        cancellation: &Cancellation,
    ) -> Result<String> {
        options.validate()?;
        cancellation.check()?;
        if self.mobile {
            return Err(
                "unsupported: browser click watches require a desktop page with verifiable focus"
                    .into(),
            );
        }
        if self.observation_ids.len() >= 16 {
            return Err("at most 16 observation watches may be active; close one first".into());
        }
        let began = Instant::now();
        let deadline = began + Duration::from_millis(options.timeout_ms);
        let id = format!(
            "click-watch-{}-{}",
            std::process::id(),
            PROFILE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let target = self.target.clone();
        let session = self.session.clone();
        let mut initial = String::from("null");
        let mut dispatches = Vec::<Dispatch>::new();
        let mut stage = "install";
        let outcome = (|| -> Result<()> {
            if self.observation_ids.is_empty() {
                self.request_until("Runtime.enable", "{}", true, deadline, Some(cancellation))?;
                self.request_until(
                    "Runtime.addBinding",
                    &format!(r#"{{"name":{}}}"#, quote(&self.observation_binding)),
                    true,
                    deadline,
                    Some(cancellation),
                )?;
            }
            // Mark the ID before evaluating. If its response is cancelled, the
            // cleanup command still removes an observer the page may install.
            self.observation_ids.insert(id.clone());
            initial =
                self.click_watch_command("install", &id, options, "null", deadline, cancellation)?;
            let mut event = with_json(&initial, |root| {
                if let Some(reason) = root.get("terminal").and_then(Value::string) {
                    return Err(format!("blocked: {}", reason.chars().collect::<String>()));
                }
                Ok(root
                    .get("candidate")
                    .filter(|value| value.raw() != "null")
                    .map(|value| value.raw().to_owned()))
            })?;
            loop {
                cancellation.check()?;
                if self.target != target || self.session != session {
                    return Err("blocked: selected browser page changed".into());
                }
                if Instant::now() >= deadline {
                    return Err("timeout: click watch deadline reached".into());
                }
                stage = if dispatches
                    .iter()
                    .any(|dispatch| dispatch.received.is_none())
                {
                    "confirm_click"
                } else {
                    "wait_for_target"
                };
                let payload = if let Some(event) = event.take() {
                    event
                } else {
                    self.observation_event(&id, deadline, cancellation)?
                        .ok_or("timeout: click watch deadline reached")?
                };
                let observed = Instant::now();
                match field_string(&payload, &["type"])?.as_str() {
                    "stopped" => {
                        return Err(format!("blocked: {}", field_string(&payload, &["reason"])?));
                    }
                    "received" => {
                        let episode = integer_field(&payload, "episode")?;
                        let element = field_string(&payload, &["element"])?;
                        let trusted = with_json(&payload, |root| {
                            Ok(root.get("trusted").and_then(Value::boolean) == Some(true))
                        })?;
                        if !trusted {
                            return Err(
                                "blocked: click acknowledgement was not trusted input".into()
                            );
                        }
                        if let Some(dispatch) = dispatches.iter_mut().find(|dispatch| {
                            dispatch.episode == episode && dispatch.element == element
                        }) && dispatch.received.is_none()
                        {
                            dispatch.received = Some(payload);
                        }
                        if dispatches
                            .iter()
                            .filter(|dispatch| dispatch.received.is_some())
                            .count()
                            >= options.max_clicks as usize
                        {
                            cancellation.check()?;
                            stage = "completed";
                            return Ok(());
                        }
                    }
                    "candidate" => {
                        let episode = integer_field(&payload, "episode")?;
                        let revision = integer_field(&payload, "revision")?;
                        if dispatches
                            .iter()
                            .any(|dispatch| dispatch.episode == episode)
                        {
                            continue;
                        }
                        if dispatches.len() >= options.max_clicks as usize {
                            continue;
                        }
                        let expected = format!(r#"{{"episode":{episode},"revision":{revision}}}"#);
                        stage = "verify_cached_target";
                        let verified = self.click_watch_command(
                            "claim",
                            &id,
                            options,
                            &expected,
                            deadline,
                            cancellation,
                        )?;
                        match field_string(&verified, &["status"])?.as_str() {
                            "stale" => continue,
                            "blocked" => {
                                return Err(format!(
                                    "blocked: {}",
                                    field_string(&verified, &["reason"])?
                                ));
                            }
                            "ready" => {}
                            _ => return Err("invalid click watch verification status".into()),
                        }
                        let element = field_string(&verified, &["element"])?;
                        if integer_field(&verified, "episode")? != episode {
                            return Err(
                                "blocked: click target episode changed during verification".into(),
                            );
                        }
                        let (x, y) = with_json(&verified, |root| {
                            Ok((number(root.get("x"))?, number(root.get("y"))?))
                        })?;
                        if !(0.0..=1_048_576.0).contains(&x) || !(0.0..=1_048_576.0).contains(&y) {
                            return Err(
                                "blocked: click point lies outside supported screen coordinates"
                                    .into(),
                            );
                        }
                        stage = "dispatch_click";
                        let mut attempted = false;
                        let mut sent_events = 0;
                        let mut requests = [0; 2];
                        // The final cancellation check and both frame writes
                        // share one gate. Never hold it while waiting for CDP.
                        let sent = cancellation.dispatch(|| {
                            if Instant::now() >= deadline { return Err("timeout: click dispatch deadline reached".into()); }
                            if self.target != target || self.session != session {
                                return Err("blocked: selected browser page changed".into());
                            }
                            self.check_pending_click_events(&id, episode, revision)?;
                            attempted = true;
                            for (index, kind) in ["mousePressed", "mouseReleased"].iter().enumerate() {
                                let buttons = if index == 0 { 1 } else { 0 };
                                // Once pressed, always send the matching release
                                // under the same permits, even if the wait's
                                // deadline elapsed during the press write.
                                let send_deadline = Instant::now() + Duration::from_millis(500);
                                let send_deadline = if index == 0 { deadline.min(send_deadline) }
                                    else { send_deadline };
                                requests[index] = self.send_request_until("Input.dispatchMouseEvent",
                                    &format!(r#"{{"type":"{kind}","x":{x},"y":{y},"button":"left","buttons":{buttons},"clickCount":1}}"#),
                                    true, send_deadline)?;
                                sent_events += 1;
                            }
                            Ok(())
                        });
                        if attempted {
                            dispatches.push(Dispatch { episode, element,
                                details: format!(r#"{{"episode":{episode},"event_to_dispatch_us":{},"sent_mouse_events":{sent_events},"verification":{verified}}}"#,
                                    observed.elapsed().as_micros()), received: None,
                                release_request: (sent_events == 2).then_some(requests[1]),
                                release_confirmed: false, release_error: None });
                        }
                        if attempted && sent.is_err() {
                            // A failed write can leave a partial frame. Never
                            // append an observer-cleanup request to that frame
                            // or claim that zero completed writes means no input.
                            let _ = self.socket.stream.shutdown(std::net::Shutdown::Both);
                        }
                        sent?;
                        stage = "confirm_click";
                        for (index, request) in requests.into_iter().enumerate() {
                            self.response_until(
                                request,
                                "Input.dispatchMouseEvent",
                                deadline,
                                Some(cancellation),
                            )?;
                            if index == 1 {
                                dispatches
                                    .last_mut()
                                    .expect("sent click has a dispatch record")
                                    .release_confirmed = true;
                            }
                        }
                    }
                    "gone" => {}
                    other => return Err(format!("invalid click watch event: {other}")),
                }
            }
        })();
        // Cancellation can arrive after both frames were written but before
        // the release acknowledgement. Read that response under one short
        // cleanup deadline, without resending input and without a host permit.
        self.confirm_click_releases(&mut dispatches);
        // The page observer never dispatches input itself, including when this
        // cleanup cannot run because the page or connection has disappeared.
        let cleanup = if self.observation_ids.contains(&id) {
            self.close_click_watch(&id)
        } else {
            Ok(())
        };
        let status = match &outcome {
            Ok(()) if cleanup.is_ok() => "completed",
            Ok(()) => "cleanup_failed",
            Err(error) if error.starts_with("cancelled:") => "cancelled",
            Err(error) if error.starts_with("timeout:") || error.contains("deadline") => "timeout",
            Err(_) => "stopped",
        };
        let confirmed = dispatches
            .iter()
            .filter(|dispatch| dispatch.received.is_some())
            .count();
        let clicks = dispatches.iter().map(|dispatch| {
            let details = dispatch.details.strip_suffix('}').expect("dispatch details object");
            let error = dispatch.release_error.as_ref().map(|error| quote(error)).unwrap_or_else(|| "null".into());
            format!(r#"{{"dispatch":{details},"release_confirmed":{},"release_error":{error}}},"received":{}}}"#,
                dispatch.release_confirmed, dispatch.received.as_deref().unwrap_or("null"))
        }).collect::<Vec<_>>().join(",");
        let error = outcome
            .as_ref()
            .err()
            .map(|error| quote(error))
            .unwrap_or_else(|| "null".into());
        let cleanup_error = cleanup
            .as_ref()
            .err()
            .map(|error| quote(error))
            .unwrap_or_else(|| "null".into());
        let receipt = format!(
            r#"{{"action":"watch_until_click","watch":{},"status":{},"stage":{},"target_id":{},"initial":{initial},"max_clicks":{},"confirmed_clicks":{confirmed},"dispatch_attempts":{},"clicks":[{clicks}],"elapsed_ms":{},"screenshots":0,"error":{error},"cleanup_error":{cleanup_error}}}"#,
            quote(&id),
            quote(status),
            quote(stage),
            quote(&target),
            options.max_clicks,
            dispatches.len(),
            began.elapsed().as_millis()
        );
        if outcome.is_ok() && cleanup.is_ok() {
            Ok(receipt)
        } else {
            Err(format!("{status}: {receipt}"))
        }
    }

    fn confirm_click_releases(&mut self, dispatches: &mut [Dispatch]) {
        let deadline = Instant::now() + Duration::from_millis(500);
        for dispatch in dispatches {
            if dispatch.release_confirmed {
                continue;
            }
            let Some(request) = dispatch.release_request else {
                continue;
            };
            match self.response_until(request, "Input.dispatchMouseEvent", deadline, None) {
                Ok(_) => dispatch.release_confirmed = true,
                Err(error) => dispatch.release_error = Some(error),
            }
        }
    }

    fn click_watch_command(
        &mut self,
        action: &str,
        id: &str,
        options: &ClickWatchOptions,
        expected: &str,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<String> {
        let origin = options
            .expected_origin
            .as_ref()
            .map(|value| quote(value))
            .unwrap_or_else(|| "null".into());
        let options = format!(
            r#"{{"scope":{},"selector":{},"expected_origin":{origin}}}"#,
            quote(&options.scope),
            quote(&options.selector)
        );
        let expression = format!(
            "(()=>{{const action={},id={},binding={},options={options},expected={expected};{} }})()",
            quote(action),
            quote(id),
            quote(&self.observation_binding),
            include_str!("../browser_click_watch.js")
        );
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(30000);
        if remaining == 0 {
            return Err("timeout: click watch deadline reached".into());
        }
        let result = self.request_until("Runtime.evaluate",
            &format!(r#"{{"expression":{},"returnByValue":true,"awaitPromise":false,"timeout":{remaining}}}"#, quote(&expression)),
            true, deadline, Some(cancellation))?;
        with_json(&result, |root| {
            if let Some(exception) = root.get("exceptionDetails") {
                return Err(format!("page JavaScript failed: {}", exception.raw()));
            }
            root.get("result")
                .and_then(|value| value.get("value"))
                .map(|value| value.raw().to_owned())
                .ok_or_else(|| "click watch returned no value".into())
        })
    }

    fn close_click_watch(&mut self, id: &str) -> Result<()> {
        // Cancellation must not turn into an unbounded cleanup read when a
        // peer paused halfway through a WebSocket frame. Preserve the frame
        // and watch ID on failure so a later navigation/close can retry.
        let deadline = Instant::now() + Duration::from_millis(500);
        let expression = format!(
            "(()=>{{const w=globalThis[Symbol.for('zuse.observation.v1')];w?.get({0})?.close();w?.delete({0});return true}})()",
            quote(id)
        );
        let result = self.request_until(
            "Runtime.evaluate",
            &format!(
                r#"{{"expression":{},"returnByValue":true,"awaitPromise":false,"timeout":500}}"#,
                quote(&expression)
            ),
            true,
            deadline,
            None,
        )?;
        with_json(&result, |root| {
            if let Some(error) = root.get("exceptionDetails") {
                return Err(format!("click watch cleanup failed: {}", error.raw()));
            }
            Ok(())
        })?;
        self.observation_ids.remove(id);
        if self.observation_ids.is_empty() {
            self.request_until(
                "Runtime.removeBinding",
                &format!(r#"{{"name":{}}}"#, quote(&self.observation_binding)),
                true,
                deadline,
                None,
            )?;
        }
        Ok(())
    }

    fn check_pending_click_events(&self, id: &str, episode: u64, revision: u64) -> Result<()> {
        for message in &self.events {
            with_json(message, |root| {
                if !self.session.is_empty()
                    && !root
                        .get("sessionId")
                        .and_then(Value::string)
                        .is_some_and(|value| value.matches(&self.session))
                {
                    return Ok(());
                }
                let Some(method) = root.get("method").and_then(Value::string) else {
                    return Ok(());
                };
                if method.matches("Runtime.executionContextsCleared") {
                    return Err("blocked: document changed before click dispatch".into());
                }
                let Some(params) = root.get("params") else {
                    return Ok(());
                };
                if method.matches("Page.frameNavigated")
                    && params
                        .get("frame")
                        .is_some_and(|frame| frame.get("parentId").is_none())
                {
                    return Err("blocked: page navigated before click dispatch".into());
                }
                if !method.matches("Runtime.bindingCalled")
                    || !params
                        .get("name")
                        .and_then(Value::string)
                        .is_some_and(|value| value.matches(&self.observation_binding))
                {
                    return Ok(());
                }
                let Some(payload) = params.get("payload").and_then(Value::string) else {
                    return Ok(());
                };
                let payload: String = payload.chars().collect();
                with_json(&payload, |event| {
                    if !event
                        .get("watch")
                        .and_then(Value::string)
                        .is_some_and(|watch| watch.matches(id))
                    {
                        return Ok(());
                    }
                    let kind = event.get("type").and_then(Value::string);
                    if kind.is_some_and(|kind| kind.matches("stopped")) {
                        return Err(format!(
                            "blocked before dispatch: {}",
                            event
                                .get("reason")
                                .map(|value| value.raw())
                                .unwrap_or("unknown")
                        ));
                    }
                    let next_episode = event
                        .get("episode")
                        .and_then(|value| value.integer().ok())
                        .unwrap_or(0);
                    let next_revision = event
                        .get("revision")
                        .and_then(|value| value.integer().ok())
                        .unwrap_or(0);
                    if kind.is_some_and(|kind| kind.matches("gone"))
                        && next_episode >= episode as i64
                    {
                        return Err("blocked: target disappeared before click dispatch".into());
                    }
                    if kind.is_some_and(|kind| kind.matches("candidate"))
                        && (next_episode > episode as i64
                            || (next_episode == episode as i64 && next_revision > revision as i64))
                    {
                        return Err("blocked: target changed before click dispatch".into());
                    }
                    Ok(())
                })
            })?;
        }
        Ok(())
    }
}

fn integer_field(input: &str, key: &str) -> Result<u64> {
    with_json(input, |root| {
        root.get(key)
            .and_then(|value| value.integer().ok())
            .filter(|value| *value > 0)
            .map(|value| value as u64)
            .ok_or_else(|| format!("invalid click watch {key}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_attachment_cancels_stalled_discovery_and_handshake() {
        use std::io::Read;
        use std::sync::mpsc;
        for scheme in ["http", "ws"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!(
                "{scheme}://{}/devtools/browser/owned-fixture",
                listener.local_addr().unwrap()
            );
            let (ready_tx, ready_rx) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                ready_tx.send(()).unwrap();
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).unwrap(), 0);
            });
            let token = Cancellation::default();
            let worker_token = token.clone();
            let (done_tx, done_rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let error = Browser::connect_existing_cancellable(
                    &endpoint,
                    "existing-page",
                    &worker_token,
                )
                .err()
                .expect("unanswered attachment cannot succeed");
                done_tx.send(error).unwrap();
            });
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            token.cancel();
            let error = done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("attachment did not wake on cancellation");
            assert!(error.starts_with("cancelled:"), "{scheme}: {error}");
            worker.join().unwrap();
            server.join().unwrap();
        }
    }

    #[test]
    fn attached_close_bounds_a_stalled_observer_cleanup_and_closes_the_socket() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).unwrap();
            assert!(!bytes.is_empty());
        });
        let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
        browser.observation_ids.insert("owned-observation".into());
        let start = Instant::now();
        assert!(browser.close().is_err());
        assert!(browser.closed);
        assert!(start.elapsed() < Duration::from_millis(1500));
        server.join().unwrap();
    }

    #[test]
    fn release_confirmation_reads_the_original_reply_without_replaying_input() {
        use std::io::{Read, Write};
        for accepted in [true, false] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let server = std::thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let reply = if accepted {
                    r#"{"id":17,"result":{}}"#
                } else {
                    r#"{"id":17,"error":{"message":"release rejected"}}"#
                };
                let mut frame = vec![0x81, reply.len() as u8];
                frame.extend_from_slice(reply.as_bytes());
                peer.write_all(&frame).unwrap();
                let mut replay = Vec::new();
                peer.read_to_end(&mut replay).unwrap();
                assert!(
                    replay.is_empty(),
                    "release confirmation must never resend input"
                );
            });
            let mut browser = Browser::unattached_socket(WebSocket::new(stream), false);
            let mut dispatches = [Dispatch {
                episode: 1,
                element: "owned-fixture".into(),
                details: r#"{"sent_mouse_events":2}"#.into(),
                received: None,
                release_request: Some(17),
                release_confirmed: false,
                release_error: None,
            }];
            browser.confirm_click_releases(&mut dispatches);
            assert_eq!(dispatches[0].release_confirmed, accepted);
            assert_eq!(dispatches[0].release_error.is_none(), accepted);
            browser.close().unwrap();
            server.join().unwrap();
        }
    }

    fn endpoint(browser: &Browser) -> String {
        let text = fs::read_to_string(browser.profile.as_ref().unwrap().join("DevToolsActivePort"))
            .unwrap();
        let mut lines = text.lines();
        format!(
            "ws://127.0.0.1:{}{}",
            lines.next().unwrap(),
            lines.next().unwrap()
        )
    }

    fn pages(browser: &mut Browser) -> usize {
        with_json(&browser.tabs().unwrap(), |root| {
            Ok(root
                .get("targetInfos")
                .unwrap()
                .elements()
                .filter(|target| {
                    target
                        .get("type")
                        .and_then(Value::string)
                        .is_some_and(|kind| kind.matches("page"))
                })
                .count())
        })
        .unwrap()
    }

    #[test]
    #[ignore = "requires installed sandboxed Chromium; verifies existing-tab ownership"]
    fn browser_click_watch_existing_attachment_neither_creates_nor_activates_a_tab() {
        let mut browser = Browser::launch(true).unwrap();
        let endpoint = endpoint(&browser);
        let mut foreground = Browser::connect(&endpoint).unwrap();
        foreground.request("Page.bringToFront", "{}", true).unwrap();
        let before = pages(&mut browser);
        let mut attached = Browser::connect_existing(&endpoint, &browser.target).unwrap();
        assert!(!attached.owned_target);
        assert_eq!(pages(&mut browser), before);
        assert_eq!(foreground.evaluate("document.hasFocus()").unwrap(), "true");
        assert_eq!(attached.evaluate("document.hasFocus()").unwrap(), "false");
        attached.close().unwrap();
        assert_eq!(
            pages(&mut browser),
            before,
            "closing an attached session must keep the user's tab open"
        );
        browser
            .evaluate("document.title='still open';true")
            .unwrap();
        foreground.close().unwrap();
        browser.close().unwrap();
    }

    #[test]
    #[ignore = "requires installed sandboxed Chromium; verifies real tab focus events"]
    fn browser_click_watch_stops_on_real_tab_focus_loss() {
        let mut browser = Browser::launch(true).unwrap();
        let mut other = Browser::connect(&endpoint(&browser)).unwrap();
        browser.request("Page.bringToFront", "{}", true).unwrap();
        let url = format!(
            "data:text/html;base64,{}",
            encode_base64(include_bytes!("../../tests/browser_click_fixture.html"))
        );
        browser.navigate(&url).unwrap();
        browser.evaluate("fixture.start('hold',1,160)").unwrap();
        let change_focus = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            other.request("Page.bringToFront", "{}", true).unwrap();
            thread::sleep(Duration::from_millis(220));
            other.close().unwrap();
        });
        let options = ClickWatchOptions {
            scope: "#arena".into(),
            selector: ".mole".into(),
            timeout_ms: 2000,
            ..ClickWatchOptions::default()
        };
        let result = browser
            .watch_until_click(&options, &Cancellation::default())
            .unwrap_err();
        assert!(
            result.contains("focus") && result.contains("\"dispatch_attempts\":0"),
            "{result}"
        );
        change_focus.join().unwrap();
        assert_eq!(
            browser
                .evaluate("fixture.clicks.length + fixture.outsideClicks")
                .unwrap(),
            "0"
        );
        browser.close().unwrap();
    }
}
