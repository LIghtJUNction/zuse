use super::{Browser, PROFILE_ID, field_string, hold_events, optional_string, quote, with_json};
use crate::{
    key_hold::{self, Result, Target},
    observation::Cancellation,
};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const EDGE: &str = "Input.dispatchKeyEvent";
const IO_BUDGET: Duration = Duration::from_millis(500);
const STALE: &str = "stale_target: original browser document or focused element is unavailable";

// A missing remote object/context means that the captured document was
// destroyed. A transport timeout leaves the target unknown; cleanup gets
// one fresh, bounded verification before deciding whether it can release.
fn guard_error(error: String) -> String {
    if error.starts_with("CDP DOM.describeNode:")
        || error.starts_with("CDP Runtime.callFunctionOn:")
    {
        format!("{STALE}: {error}")
    } else {
        error
    }
}

struct PageKey<'a> {
    browser: &'a mut Browser,
    guard: String,
    node: String,
    down: String,
    up: String,
}

impl Browser {
    fn hold_request(&mut self, method: &str, params: &str) -> Result<String> {
        self.request_until(method, params, true, Instant::now() + IO_BUDGET, None)
    }

    /// Hold one key on the selected desktop page, bound to its focused element.
    pub fn hold(&mut self, chord: &str, duration_ms: u64, cancel: &Cancellation) -> Result<()> {
        cancel.check()?;
        if !(1..=key_hold::MAX_MS as u64).contains(&duration_ms) {
            return Err("hold duration must be 1..60000 milliseconds".into());
        }
        if self.mobile {
            return Err(
                "unsupported_state: browser.hold requires a desktop page with verifiable focus"
                    .into(),
            );
        }
        let (down, up) = hold_events(chord)?;
        let document = self.hold_document()?;
        let frame = &document.0;
        let world = self.hold_request(
            "Page.createIsolatedWorld",
            &format!(
                r#"{{"frameId":{},"worldName":"zuse-key-hold"}}"#,
                quote(frame)
            ),
        )?;
        let context = with_json(&world, |v| {
            v.get("executionContextId")
                .and_then(|id| id.integer().ok())
                .ok_or_else(|| STALE.to_owned())
        })?;
        let group = format!(
            "zuse-key-hold-{}",
            PROFILE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let mut guard = None;
        let result = (|| {
            let params = format!(
                r#"{{"expression":{},"contextId":{context},"objectGroup":{}}}"#,
                quote(include_str!("hold_guard.js")),
                quote(&group)
            );
            let installed = self.hold_request("Runtime.evaluate", &params)?;
            let id = field_string(&installed, &["result", "objectId"])?;
            guard = Some(id.clone());
            let node = self.hold_call(&id, "function(){return this.node}", false)?;
            let node =
                field_string(&node, &["result", "objectId"]).map_err(|_| STALE.to_owned())?;
            if self.hold_document()? != document {
                return Err(STALE.into());
            }
            key_hold::run(
                &mut PageKey {
                    browser: self,
                    guard: id,
                    node,
                    down,
                    up,
                },
                duration_ms,
                cancel,
            )
        })();
        if let Some(id) = guard {
            let _ = self.hold_call(&id, "function(){this.close()}", true);
        }
        let _ = self.hold_request(
            "Runtime.releaseObjectGroup",
            &format!(r#"{{"objectGroup":{}}}"#, quote(&group)),
        );
        result
    }

    fn hold_document(&mut self) -> Result<(String, String, String, Option<String>)> {
        let tree = self.hold_request("Page.getFrameTree", "{}")?;
        let frame = |key| field_string(&tree, &["frameTree", "frame", key]);
        Ok((
            frame("id")?,
            frame("loaderId")?,
            frame("url")?,
            optional_string(&tree, &["frameTree", "frame", "urlFragment"])?,
        ))
    }

    fn hold_call(&mut self, id: &str, function: &str, by_value: bool) -> Result<String> {
        self.hold_request(
            "Runtime.callFunctionOn",
            &format!(
                r#"{{"objectId":{},"functionDeclaration":{},"returnByValue":{by_value}}}"#,
                quote(id),
                quote(function)
            ),
        )
    }
}

impl Target for PageKey<'_> {
    fn verify(&mut self) -> Result<()> {
        // Shadow hosts can conceal a different focused descendant, including
        // inside a closed root. Inspect the original node through CDP.
        let desc = self
            .browser
            .hold_request(
                "DOM.describeNode",
                &format!(r#"{{"objectId":{},"depth":0}}"#, quote(&self.node)),
            )
            .map_err(guard_error)?;
        let shadow = with_json(&desc, |v| {
            let node = v.get("node").ok_or(STALE)?;
            Ok(node.get("shadowRoots").is_some_and(|roots| {
                roots.elements().any(|root| {
                    !root
                        .get("shadowRootType")
                        .and_then(|t| t.string())
                        .is_some_and(|s| s.matches("user-agent"))
                })
            }))
        })?;
        if shadow {
            return Err("stale_target: keyboard focus inside a shadow host is ambiguous".into());
        }
        let result = self
            .browser
            .hold_call(&self.guard, "function(){return this.valid()}", true)
            .map_err(guard_error)?;
        with_json(&result, |v| {
            if v.get("result")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.boolean())
                == Some(true)
            {
                Ok(())
            } else {
                Err(STALE.into())
            }
        })
    }
    fn press(&mut self, cancel: &Cancellation, attempted: &mut bool) -> Result<()> {
        let deadline = Instant::now() + IO_BUDGET;
        let id = cancel.dispatch(|| {
            *attempted = true;
            self.browser
                .send_request_until(EDGE, &self.down, true, deadline)
                .inspect_err(|_| {
                    // A partial WebSocket frame cannot be followed by a cleanup command.
                    let _ = self
                        .browser
                        .socket
                        .stream
                        .shutdown(std::net::Shutdown::Both);
                })
        })?;
        self.browser
            .response_until(id, EDGE, deadline, Some(cancel))
            .map(|_| ())
    }
    fn release(&mut self) -> Result<()> {
        self.verify()?;
        self.browser.hold_request(EDGE, &self.up).map(|_| ())
    }
}
