//! Explicit focus emulation for this connection's selected desktop page.
//!
//! CDP's experimental Emulation.setFocusEmulationEnabled changes the selected
//! page's reported focus/activity; it does not route input through OS focus.
//! Existing hold/pointer document, focus-target and geometry guards stay intact.
//! The synchronous client checks external navigation at Zuse command boundaries;
//! it cannot promise immediate cleanup while idle without reading the socket.
use super::*;
use crate::observation::Cancellation;

const BUDGET: Duration = Duration::from_secs(2);
const METHOD: &str = "Emulation.setFocusEmulationEnabled";

#[derive(Clone, Debug, PartialEq, Eq)]
struct DocumentIdentity {
    frame: String,
    loader: String,
    url: String,
}

impl DocumentIdentity {
    fn json(&self) -> String {
        format!(
            r#"{{"frameId":{},"loaderId":{},"url":{}}}"#,
            quote(&self.frame),
            quote(&self.loader),
            quote(&self.url)
        )
    }
}

#[derive(Clone, Debug)]
pub(super) struct State {
    session: String,
    target: String,
    document: DocumentIdentity,
    confirmed: bool,
}

impl Browser {
    /// Explicitly emulate focus/activity on the selected desktop page only.
    /// `false` is also the cleanup path: it ignores cancellation and only clears
    /// an override attempted by this Browser. Unsupported CDP methods fail closed.
    pub fn background(&mut self, enabled: bool, cancellation: &Cancellation) -> Result<String> {
        if self.closed {
            return Err("browser connection is closed".into());
        }
        if self.mobile {
            return Err("unsupported_state: browser.background requires a desktop page".into());
        }
        if !enabled {
            let changed = self.background_state.is_some();
            self.clear_background()?;
            return Ok(self.background_receipt(false, changed));
        }
        cancellation.check()?;
        if self
            .background_state
            .as_ref()
            .is_some_and(|state| state.confirmed)
        {
            self.validate_background_document()?;
            return Ok(self.background_receipt(true, false));
        }
        // A failed cleanup leaves an attempted, not confirmed, override owned.
        // Never turn that uncertainty into a successful enable receipt.
        self.clear_background()?;
        let deadline = Instant::now() + BUDGET;
        let document = self.background_document(deadline, Some(cancellation))?;
        self.background_state = Some(State {
            session: self.session.clone(),
            target: self.target.clone(),
            document,
            confirmed: false,
        });
        // Retain ownership before attempting the write: a missing response must
        // not turn an ambiguously applied override into an unowned one.
        let result: Result<()> = (|| {
            let id = cancellation.dispatch(|| {
                self.send_request_until(METHOD, r#"{"enabled":true}"#, true, deadline)
            })?;
            self.response_until(id, METHOD, deadline, Some(cancellation))?;
            loop {
                let value = self.request_until("Runtime.evaluate",
                    r#"{"expression":"({focused:document.hasFocus(),visible:document.visibilityState==='visible',activeElement:!!document.activeElement&&document.activeElement.isConnected})","returnByValue":true,"awaitPromise":false,"userGesture":false,"timeout":1000}"#,
                    true, deadline, Some(cancellation))?;
                let ready = with_json(&value, |root| {
                    if root.get("exceptionDetails").is_some() {
                        return Err("background focus observation failed".into());
                    }
                    let state = root
                        .get("result")
                        .and_then(|value| value.get("value"))
                        .ok_or("background focus observation returned no state")?;
                    Ok(["focused", "visible", "activeElement"]
                        .iter()
                        .all(|key| state.get(key).and_then(Value::boolean) == Some(true)))
                })?;
                if ready {
                    break;
                }
                if Instant::now() + Duration::from_millis(20) >= deadline {
                    return Err("unsupported_state: selected page did not confirm emulated focus and visibility".into());
                }
                cancellation.sleep(Duration::from_millis(20))?;
            }
            let actual = self.background_document(deadline, Some(cancellation))?;
            if self.background_state.as_ref().map(|state| &state.document) != Some(&actual) {
                return Err(
                    "stale_target: browser document changed while enabling background focus".into(),
                );
            }
            cancellation.check()
        })();
        if let Err(error) = result {
            return match self.clear_background() {
                Ok(()) => Err(format!("{error}; background focus emulation was cleared")),
                Err(cleanup) => Err(format!("{error}; {cleanup}")),
            };
        }
        self.background_state.as_mut().unwrap().confirmed = true;
        Ok(self.background_receipt(true, true))
    }

    /// Called before and after a Zuse command, outside any owned input hold.
    /// An external navigation can remain emulated while this synchronous client
    /// is idle; the next boundary detects it and refuses further work on it.
    pub(crate) fn validate_background_document(&mut self) -> Result<()> {
        let Some(expected) = self.background_state.clone() else {
            return Ok(());
        };
        if self.session != expected.session || self.target != expected.target {
            return Err("stale_target: background focus still belongs to the previous page session; cleanup must use that session".into());
        }
        let actual = if expected.confirmed {
            self.background_document(Instant::now() + BUDGET, None)
        } else {
            Err("background focus emulation was not confirmed".into())
        };
        let error = match actual {
            Ok(actual) if actual == expected.document => return Ok(()),
            Ok(_) => "stale_target: browser document changed; background focus is not inherited by the next Zuse command".to_owned(),
            Err(error) => format!("background document identity could not be verified: {error}"),
        };
        match self.clear_background() {
            Ok(()) => Err(format!("{error}; background focus emulation was cleared")),
            Err(cleanup) => Err(format!("{error}; {cleanup}")),
        }
    }

    /// Clear only our current page-session override, including direct-page
    /// sockets whose target/session strings are empty. Failure retains ownership
    /// for another explicit cleanup attempt; it never claims restoration.
    pub(crate) fn clear_background(&mut self) -> Result<()> {
        let Some(state) = self.background_state.as_ref() else {
            return Ok(());
        };
        if self.session != state.session || self.target != state.target {
            return Err(
                "background focus cleanup not confirmed: selected session differs from its owner"
                    .into(),
            );
        }
        self.request_until(
            METHOD,
            r#"{"enabled":false}"#,
            true,
            Instant::now() + BUDGET,
            None,
        )
        .map_err(|error| format!("background focus cleanup not confirmed: {error}"))?;
        self.background_state = None;
        Ok(())
    }

    fn background_document(
        &mut self,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<DocumentIdentity> {
        let result = self.request_until("Page.getFrameTree", "{}", true, deadline, cancellation)?;
        let mut url = field_string(&result, &["frameTree", "frame", "url"])?;
        if let Some(fragment) = optional_string(&result, &["frameTree", "frame", "urlFragment"])? {
            url.push_str(&fragment);
        }
        let document = DocumentIdentity {
            frame: field_string(&result, &["frameTree", "frame", "id"])?,
            loader: field_string(&result, &["frameTree", "frame", "loaderId"])?,
            url,
        };
        if document.frame.is_empty()
            || document.loader.is_empty()
            || document.frame.len() > 1024
            || document.loader.len() > 1024
            || document.url.len() > 65536
        {
            return Err("unsupported_state: selected page has no bounded document identity".into());
        }
        Ok(document)
    }

    // Clearing this CDP override does not undo RenderWidget focus set by input,
    // nor does it restore or activate an OS window or another tab.
    fn background_receipt(&self, enabled: bool, changed: bool) -> String {
        let document = self
            .background_state
            .as_ref()
            .map(|state| state.document.json())
            .unwrap_or_else(|| "null".into());
        let target = if self.target.is_empty() {
            "null".into()
        } else {
            quote(&self.target)
        };
        let session = if self.session.is_empty() {
            "null".into()
        } else {
            quote(&self.session)
        };
        format!(
            r#"{{"enabled":{enabled},"focus_emulated":{enabled},"changed":{changed},"focus_emulation_cleared":{},"scope":"selected_page_session","targetId":{target},"sessionId":{session},"document":{document},"input_delivery":"cdp_selected_page","idle_navigation_cleanup":"next_zuse_command"}}"#,
            !enabled
        )
    }
}

#[cfg(test)]
#[path = "../../tests/support/browser_background.rs"]
mod tests;
