//! Explicit, single-submission activation of the currently selected page.
//! Protocol acknowledgements and other targets' events never confirm focus.
use super::*;
use crate::observation::Cancellation;

const BUDGET: Duration = Duration::from_secs(5);
const INTERVAL: Duration = Duration::from_millis(20);
const METHOD: &str = "Page.bringToFront";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    target: String,
    frame: String,
    loader: String,
    url: String,
}

impl Identity {
    fn document_json(&self) -> String {
        format!(
            r#"{{"frameId":{},"loaderId":{},"url":{}}}"#,
            quote(&self.frame),
            quote(&self.loader),
            quote(&self.url)
        )
    }
}

#[derive(Clone, Copy)]
enum Submission {
    NotSent,
    Attempted,
    Acknowledged,
}

fn check(deadline: Instant, cancellation: &Cancellation) -> Result<()> {
    cancellation.check()?;
    if Instant::now() >= deadline {
        return Err("timeout: browser activation deadline reached".into());
    }
    Ok(())
}

impl Browser {
    /// Activate this exact page once, then verify its document, visibility and
    /// focus within five seconds. Selection/ownership never change on failure.
    pub fn activate(&mut self, cancellation: &Cancellation) -> Result<String> {
        self.activate_until(Instant::now() + BUDGET, cancellation)
    }

    fn activate_until(&mut self, deadline: Instant, cancellation: &Cancellation) -> Result<String> {
        let started = Instant::now();
        let selected_target = self.target.clone();
        let selected_session = self.session.clone();
        let mut submission = Submission::NotSent;
        let result = (|| {
            check(deadline, cancellation)?;
            if self.closed {
                return Err("browser connection is closed".into());
            }
            if self.background_state.is_some() {
                return Err("unsupported_state: disable browser.background before activation so visibility and focus are not emulated".into());
            }
            let expected = self.activation_identity(deadline, cancellation)?;
            let context = self.activation_context(&expected.frame, deadline, cancellation)?;
            if self.activation_identity(deadline, cancellation)? != expected {
                return Err(
                    "stale_target: selected browser document changed before activation".into(),
                );
            }
            let id = cancellation.dispatch(|| {
                // A host permit may take time. Recheck local selection and the
                // deadline after acquiring it. Do not wait for remote replies
                // while holding Cancellation's input-dispatch critical section.
                check(deadline, cancellation)?;
                self.check_activation_selection(&selected_target, &selected_session)?;
                // A socket write can fail after a partial send. Preserve that
                // uncertainty and never replay the activation automatically.
                submission = Submission::Attempted;
                self.send_request_until(METHOD, "{}", true, deadline)
            })?;
            self.response_until(id, METHOD, deadline, Some(cancellation))?;
            submission = Submission::Acknowledged;
            loop {
                check(deadline, cancellation)?;
                self.check_activation_selection(&selected_target, &selected_session)?;
                let (visible, focused) = self.activation_focus(context, deadline, cancellation)?;
                if self.activation_identity(deadline, cancellation)? != expected {
                    return Err("stale_target: selected browser document changed while confirming activation".into());
                }
                check(deadline, cancellation)?;
                if visible && focused {
                    return Ok(format!(
                        r#"{{"activated":true,"activation_submitted":true,"confirmation":"page_visibility_and_focus","targetId":{},"sessionId":{},"document":{},"visible":true,"focused":true,"elapsed_ms":{}}}"#,
                        quote(&expected.target),
                        if selected_session.is_empty() {
                            "null".into()
                        } else {
                            quote(&selected_session)
                        },
                        expected.document_json(),
                        started.elapsed().as_millis()
                    ));
                }
                cancellation
                    .sleep(INTERVAL.min(deadline.saturating_duration_since(Instant::now())))?;
            }
        })();
        result.map_err(|error: String| {
            let error = if Instant::now() >= deadline && !error.starts_with("cancelled:") && !error.starts_with("timeout:") {
                format!("timeout: browser activation deadline reached: {error}")
            } else { error };
            let state = match submission {
                Submission::NotSent => "activation_submitted=false; browser activation was not submitted",
                Submission::Attempted => "activation_submitted=unknown; browser activation was attempted once and was not retried",
                Submission::Acknowledged => "activation_submitted=true; browser activation was acknowledged once but its result was not confirmed; it was not retried",
            };
            format!("{error}; {state}")
        })
    }

    fn check_activation_selection(&self, target: &str, session: &str) -> Result<()> {
        if self.target != target || self.session != session {
            Err("stale_target: selected browser page session changed during activation".into())
        } else {
            Ok(())
        }
    }

    fn activation_identity(
        &mut self,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Identity> {
        check(deadline, cancellation)?;
        // Omitting targetId queries this page session itself. This also binds a
        // direct-page WebSocket whose local target/session strings are empty.
        let info = self.request_until(
            "Target.getTargetInfo",
            "{}",
            true,
            deadline,
            Some(cancellation),
        )?;
        let target = field_string(&info, &["targetInfo", "targetId"])?;
        if field_string(&info, &["targetInfo", "type"])? != "page"
            || target.is_empty()
            || target.len() > 1024
            || target.contains('\0')
            || (!self.target.is_empty() && self.target != target)
        {
            return Err(
                "stale_target: current CDP session is not the selected browser page".into(),
            );
        }
        let tree = self.request_until(
            "Page.getFrameTree",
            "{}",
            true,
            deadline,
            Some(cancellation),
        )?;
        let mut url = field_string(&tree, &["frameTree", "frame", "url"])?;
        if let Some(fragment) = optional_string(&tree, &["frameTree", "frame", "urlFragment"])? {
            url.push_str(&fragment);
        }
        let identity = Identity {
            target,
            frame: field_string(&tree, &["frameTree", "frame", "id"])?,
            loader: field_string(&tree, &["frameTree", "frame", "loaderId"])?,
            url,
        };
        if identity.frame.is_empty()
            || identity.loader.is_empty()
            || identity.frame.len() > 1024
            || identity.loader.len() > 1024
            || identity.url.len() > 65536
        {
            return Err(
                "unsupported_state: selected page has no bounded document identity for activation"
                    .into(),
            );
        }
        Ok(identity)
    }

    fn activation_context(
        &mut self,
        frame: &str,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<i64> {
        self.request_until("Runtime.enable", "{}", true, deadline, Some(cancellation))?;
        let response = self.request_until("Page.createIsolatedWorld",
            &format!(r#"{{"frameId":{},"worldName":"__zuse_activation_observation","grantUniveralAccess":false}}"#, quote(frame)),
            true, deadline, Some(cancellation))?;
        with_json(&response, |root| {
            root.get("executionContextId")
                .and_then(|value| value.integer().ok())
                .filter(|&value| (1..=i64::from(i32::MAX)).contains(&value))
                .ok_or_else(|| {
                    "unsupported_state: browser activation has no isolated observation context"
                        .into()
                })
        })
    }

    fn activation_focus(
        &mut self,
        context: i64,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<(bool, bool)> {
        check(deadline, cancellation)?;
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1);
        let value = self.request_until("Runtime.evaluate",
            &format!(r#"{{"expression":"({{visible:document.visibilityState==='visible',focused:document.hasFocus()}})","contextId":{context},"returnByValue":true,"awaitPromise":false,"userGesture":false,"timeout":{remaining}}}"#),
            true, deadline, Some(cancellation))?;
        with_json(&value, |root| {
            if root.get("exceptionDetails").is_some() {
                return Err("browser activation focus observation failed".into());
            }
            let state = root
                .get("result")
                .and_then(|value| value.get("value"))
                .ok_or("browser activation observation returned no state")?;
            let visible = state
                .get("visible")
                .and_then(Value::boolean)
                .ok_or("browser activation observation has no visibility")?;
            let focused = state
                .get("focused")
                .and_then(Value::boolean)
                .ok_or("browser activation observation has no focus state")?;
            Ok((visible, focused))
        })
    }
}

#[cfg(test)]
#[path = "../../tests/support/browser_activation.rs"]
mod tests;
