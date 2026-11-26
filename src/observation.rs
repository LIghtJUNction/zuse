//! Scoped observations and event-driven waits. Observations never capture images.
use crate::{
    accessibility,
    browser::{self, Browser},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
type DispatchPermit = Box<dyn Send>;
type DispatchAcquire = Arc<dyn Fn() -> Result<DispatchPermit> + Send + Sync>;
static NEXT_WATCH: AtomicU64 = AtomicU64::new(1);

/// A cancellation handle for one execution. Cancelling wakes registered waits.
/// A new execution uses a fresh handle; cancellation is never silently reset.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<CancelState>);

#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    next: AtomicU64,
    dispatch: Mutex<()>,
    acquire: Option<DispatchAcquire>,
    wake: Mutex<BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>>,
}

impl Cancellation {
    /// Bind an embedding host's permit to every input dispatch for the lifetime
    /// of this token. A permit is held only while sending input, never while
    /// waiting for a browser acknowledgement or an observation event.
    pub fn with_dispatch_guard(
        acquire: impl Fn() -> Result<Box<dyn Send>> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(CancelState {
            acquire: Some(Arc::new(acquire)),
            ..CancelState::default()
        }))
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        // Drain an already-started dispatch. Once cancel returns, no new input
        // can be sent through this token, even across a check/send race.
        {
            let _guard = self.0.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        }
        self.wake_waiters();
    }
    /// Request cancellation without waiting for an active native dispatch.
    /// Controllers must wait for execution to finish before confirming cleanup.
    #[cfg(any(unix, test))]
    pub(crate) fn request_cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.wake_waiters();
    }
    fn wake_waiters(&self) {
        let wake = self
            .0
            .wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for callback in wake {
            callback();
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err("cancelled: execution was cancelled".into())
        } else {
            Ok(())
        }
    }
    /// Serialize the final cancellation check with the actual input send.
    /// The closure must only dispatch input; do not wait for its remote reply.
    pub(crate) fn dispatch<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.0.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        self.check()?;
        let _permit = match &self.0.acquire {
            Some(acquire) => Some(acquire()?),
            None => None,
        };
        self.check()?;
        action()
    }
    /// Wait locally until the duration expires or cancellation wakes the wait.
    pub fn sleep(&self, duration: Duration) -> Result<()> {
        self.check()?;
        let signal = Arc::new((Mutex::new(false), Condvar::new()));
        let wake = signal.clone();
        let _guard = self.on_cancel(move || {
            let mut ready = wake.0.lock().unwrap_or_else(|e| e.into_inner());
            *ready = true;
            wake.1.notify_all();
        });
        let ready = signal.0.lock().unwrap_or_else(|e| e.into_inner());
        let _wait = signal
            .1
            .wait_timeout_while(ready, duration, |ready| !*ready)
            .unwrap_or_else(|e| e.into_inner());
        self.check()
    }
    pub(crate) fn on_cancel(&self, callback: impl Fn() + Send + Sync + 'static) -> WakeGuard {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let callback: Arc<dyn Fn() + Send + Sync> = Arc::new(callback);
        self.0
            .wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, callback.clone());
        if self.is_cancelled() {
            callback();
        }
        WakeGuard {
            cancellation: self.clone(),
            id,
        }
    }
}

pub(crate) struct WakeGuard {
    cancellation: Cancellation,
    id: u64,
}
impl Drop for WakeGuard {
    fn drop(&mut self) {
        self.cancellation
            .0
            .wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// A browser CSS container or a desktop accessibility subtree.
#[derive(Clone, Debug)]
pub struct Query {
    pub source: String,
    pub selector: String,
    pub scope: String,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            source: "desktop".into(),
            selector: "body".into(),
            scope: "0".into(),
        }
    }
}

impl Query {
    pub fn validate(&self, watching: bool) -> Result<()> {
        match self.source.as_str() {
            "browser" => {
                if self.selector.is_empty()
                    || self.selector.len() > 4096
                    || self.selector.contains('\0')
                {
                    return Err(
                        "browser observation selector must contain 1..4096 non-NUL bytes".into(),
                    );
                }
            }
            "desktop" => {
                accessibility::validate_scope(&self.scope)?;
                if watching && self.scope.split('/').count() < 3 {
                    return Err("desktop watch requires an explicit window/subtree scope from observe, such as 0/app/window".into());
                }
            }
            _ => return Err("observation source must be browser or desktop".into()),
        }
        Ok(())
    }
}

/// `changed` waits for a changed scoped snapshot. Other conditions can match
/// one visible node by its exact accessible text and optional native role.
#[derive(Clone, Debug)]
pub struct Condition {
    pub kind: String,
    pub text: Option<String>,
    pub role: Option<String>,
}

impl Default for Condition {
    fn default() -> Self {
        Self {
            kind: "changed".into(),
            text: None,
            role: None,
        }
    }
}

impl Condition {
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.kind.as_str(), "changed" | "visible" | "gone" | "text") {
            return Err("condition must be changed, visible, gone or text".into());
        }
        for (name, value) in [("text", &self.text), ("role", &self.role)] {
            if value
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.len() > 65536 || s.contains('\0'))
            {
                return Err(format!(
                    "condition {name} must be nonblank, NUL-free and at most 64 KiB"
                ));
            }
        }
        if self.kind == "text" && self.text.is_none() {
            return Err("text condition requires text".into());
        }
        if self.kind == "changed" && (self.text.is_some() || self.role.is_some()) {
            return Err(
                "changed observes the whole selected scope; use visible/text for a node condition"
                    .into(),
            );
        }
        Ok(())
    }
}

/// An explicitly requested, bounded browser action. The selected page, origin
/// and DOM container are fixed when the watch starts. The target is a simple
/// compound CSS selector (tag, id, class and/or attributes), maintained from
/// mutations. One visible episode can produce at most one click.
#[derive(Clone, Debug)]
pub struct ClickWatchOptions {
    pub scope: String,
    pub selector: String,
    pub expected_origin: Option<String>,
    pub max_clicks: u32,
    pub timeout_ms: u64,
}

impl Default for ClickWatchOptions {
    fn default() -> Self {
        Self {
            scope: "body".into(),
            selector: String::new(),
            expected_origin: None,
            max_clicks: 1,
            timeout_ms: 30000,
        }
    }
}

impl ClickWatchOptions {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [("scope", &self.scope), ("selector", &self.selector)] {
            if value.trim().is_empty() || value.len() > 4096 || value.contains('\0') {
                return Err(format!(
                    "click watch {name} must contain 1..4096 nonblank, NUL-free bytes"
                ));
            }
        }
        if let Some(origin) = &self.expected_origin
            && (origin.trim().is_empty() || origin.len() > 4096 || origin.contains('\0'))
        {
            return Err(
                "click watch expected_origin must contain 1..4096 nonblank, NUL-free bytes".into(),
            );
        }
        if !(1..=64).contains(&self.max_clicks) {
            return Err("click watch max_clicks must be 1..64".into());
        }
        if !(1..=60000).contains(&self.timeout_ms) {
            return Err("click watch timeout must be 1..60000 milliseconds".into());
        }
        Ok(())
    }
}

enum BackendWatch {
    Browser,
    Desktop(accessibility::Watch),
}

struct Watch {
    query: Query,
    backend: BackendWatch,
    current: String,
    reported: String,
    revision: u64,
    reported_revision: u64,
}

/// Watches belong to the runtime that created them. Reuse their returned IDs
/// across calls; navigation, a removed scope or a closed browser fails explicitly.
#[derive(Default)]
pub struct Observer {
    watches: BTreeMap<String, Watch>,
}

impl Observer {
    /// Unlike observe/watch/wait_until, this method explicitly dispatches input.
    pub fn watch_until_click(
        options: &ClickWatchOptions,
        cancellation: &Cancellation,
        browser: Option<&mut Browser>,
    ) -> Result<String> {
        options.validate()?;
        required_browser(browser)?.watch_until_click(options, cancellation)
    }

    pub fn observe(query: &Query, browser: Option<&mut Browser>) -> Result<String> {
        query.validate(false)?;
        match query.source.as_str() {
            "browser" => required_browser(browser)?.observe_scope(&query.selector),
            "desktop" => accessibility::snapshot_scope(&query.scope, 512, 16),
            _ => unreachable!(),
        }
    }

    pub fn watch(&mut self, query: Query, browser: Option<&mut Browser>) -> Result<String> {
        query.validate(true)?;
        if self.watches.len() >= 16 {
            return Err("at most 16 observation watches may be active; close one first".into());
        }
        let id = format!(
            "watch-{}-{}",
            std::process::id(),
            NEXT_WATCH.fetch_add(1, Ordering::Relaxed)
        );
        let (backend, current) = match query.source.as_str() {
            "browser" => (
                BackendWatch::Browser,
                required_browser(browser)?.start_observation(&id, &query.selector)?,
            ),
            "desktop" => {
                let mut native = accessibility::Watch::new(&query.scope)?;
                let snapshot = native.snapshot()?;
                (BackendWatch::Desktop(native), snapshot)
            }
            _ => unreachable!(),
        };
        let receipt = format!(
            "{{\"watch\":{},\"source\":{},\"revision\":0,\"observation\":{current},\"screenshots\":0}}",
            browser::quote(&id),
            browser::quote(&query.source)
        );
        self.watches.insert(
            id,
            Watch {
                query,
                backend,
                reported: current.clone(),
                current,
                revision: 0,
                reported_revision: 0,
            },
        );
        Ok(receipt)
    }

    pub fn snapshot(&mut self, id: &str, browser: Option<&mut Browser>) -> Result<String> {
        let watch = self
            .watches
            .get_mut(id)
            .ok_or("unknown observation watch")?;
        refresh(watch, id, browser)?;
        let result = receipt(id, watch, "observed", 0)?;
        mark_reported(watch);
        Ok(result)
    }

    pub fn close(&mut self, id: &str, browser: Option<&mut Browser>) -> Result<String> {
        let watch = self.watches.get(id).ok_or("unknown observation watch")?;
        if matches!(watch.backend, BackendWatch::Browser) {
            required_browser(browser)?.stop_observation(id)?;
        }
        self.watches.remove(id);
        Ok(format!(
            "{{\"watch\":{},\"closed\":true}}",
            browser::quote(id)
        ))
    }

    pub fn wait_until(
        &mut self,
        id: &str,
        condition: &Condition,
        timeout_ms: u64,
        cancellation: &Cancellation,
        mut browser: Option<&mut Browser>,
    ) -> Result<String> {
        condition.validate()?;
        if timeout_ms > 60000 {
            return Err("observation timeout must be 0..60000 milliseconds".into());
        }
        cancellation.check()?;
        let watch = self
            .watches
            .get_mut(id)
            .ok_or("unknown observation watch")?;
        let start = Instant::now();
        let deadline = start + Duration::from_millis(timeout_ms);
        loop {
            cancellation.check()?;
            refresh(watch, id, browser.as_deref_mut())?;
            if condition_matches(
                &watch.current,
                condition,
                watch.revision != watch.reported_revision,
            )? {
                cancellation.check()?;
                let result = receipt(id, watch, "satisfied", start.elapsed().as_millis())?;
                mark_reported(watch);
                return Ok(result);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timeout: {}",
                    receipt(id, watch, "timeout", start.elapsed().as_millis())?
                ));
            }
            let changed = match &mut watch.backend {
                BackendWatch::Browser => required_browser(browser.as_deref_mut())?
                    .wait_observation_event(id, deadline, cancellation)?,
                BackendWatch::Desktop(native) => native.wait_event(deadline, cancellation)?,
            };
            if !changed {
                return Err(format!(
                    "timeout: {}",
                    receipt(id, watch, "timeout", start.elapsed().as_millis())?
                ));
            }
        }
    }
}

fn required_browser(browser: Option<&mut Browser>) -> Result<&mut Browser> {
    browser.ok_or_else(|| "use browser.launch or browser.connect before browser observation".into())
}

fn refresh(watch: &mut Watch, id: &str, browser: Option<&mut Browser>) -> Result<()> {
    let current = match &mut watch.backend {
        BackendWatch::Browser => required_browser(browser)?.observation_snapshot(id)?,
        BackendWatch::Desktop(native) => native.snapshot()?,
    };
    if current != watch.current {
        watch.revision = watch
            .revision
            .checked_add(1)
            .ok_or("observation revision exhausted")?;
        watch.current = current;
    }
    Ok(())
}

fn mark_reported(watch: &mut Watch) {
    watch.reported.clone_from(&watch.current);
    watch.reported_revision = watch.revision;
}

fn receipt(id: &str, watch: &Watch, status: &str, elapsed: u128) -> Result<String> {
    Ok(format!(
        "{{\"watch\":{},\"source\":{},\"status\":{},\"revision\":{},\"previous_revision\":{},\"elapsed_ms\":{elapsed},\"observation\":{},\"changes\":{},\"screenshots\":0}}",
        browser::quote(id),
        browser::quote(&watch.query.source),
        browser::quote(status),
        watch.revision,
        watch.reported_revision,
        watch.current,
        delta(&watch.reported, &watch.current)?
    ))
}

fn nodes(snapshot: &str) -> Result<BTreeMap<String, String>> {
    browser::with_json(snapshot, |root| {
        let mut result = BTreeMap::new();
        for node in root
            .get("nodes")
            .ok_or("observation has no nodes")?
            .elements()
        {
            let key = node
                .get("id")
                .or_else(|| node.get("path"))
                .and_then(|n| n.string())
                .ok_or("observation node has no identity")?
                .chars()
                .collect();
            result.insert(key, node.raw().to_owned());
        }
        Ok(result)
    })
}

fn delta(previous: &str, current: &str) -> Result<String> {
    let previous = nodes(previous)?;
    let current = nodes(current)?;
    let changed = current
        .iter()
        .filter(|(id, value)| previous.get(*id) != Some(*value))
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    let removed = previous
        .keys()
        .filter(|id| !current.contains_key(*id))
        .map(|id| browser::quote(id))
        .collect::<Vec<_>>();
    Ok(format!(
        "{{\"updated\":[{}],\"removed\":[{}]}}",
        changed.join(","),
        removed.join(",")
    ))
}

fn condition_matches(snapshot: &str, condition: &Condition, changed: bool) -> Result<bool> {
    if condition.kind == "changed" {
        return Ok(changed);
    }
    browser::with_json(snapshot, |root| {
        if root.get("truncated").and_then(|n| n.boolean()) == Some(true)
            || root
                .get("errors")
                .and_then(|n| n.integer().ok())
                .unwrap_or(0)
                != 0
        {
            return Err(
                "observation is incomplete; a condition cannot be confirmed from it".into(),
            );
        }
        let mut count = 0;
        for (index, node) in root
            .get("nodes")
            .ok_or("observation has no nodes")?
            .elements()
            .enumerate()
        {
            if condition.text.is_none() && condition.role.is_none() && index != 0 {
                break;
            }
            if ["visible", "showing"]
                .iter()
                .any(|key| node.get(key).and_then(|n| n.boolean()) == Some(false))
            {
                continue;
            }
            let string = |key| {
                node.get(key)
                    .and_then(|n| n.string())
                    .map(|s| s.chars().collect::<String>())
                    .unwrap_or_default()
            };
            let role = string("role");
            if let Some(expected) = &condition.role {
                let button = |s: &str| {
                    ["button", "push button", "controltype.button", "axbutton"]
                        .iter()
                        .any(|x| s.eq_ignore_ascii_case(x))
                };
                if !role.eq_ignore_ascii_case(expected) && !(button(&role) && button(expected)) {
                    continue;
                }
            }
            if let Some(expected) = &condition.text {
                let name = string("name");
                let text = string("text");
                if &name != expected
                    && (&text != expected
                        || text == "[redacted]"
                        || role.to_ascii_lowercase().contains("password"))
                {
                    continue;
                }
            }
            count += 1;
        }
        if condition.kind == "gone" {
            return Ok(count == 0);
        }
        if count > 1 {
            return Err(format!(
                "observation condition is ambiguous: {count} visible matches; narrow its scope or role"
            ));
        }
        Ok(count == 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conditions_refuse_ambiguous_or_incomplete_observations() {
        let one = r#"{"nodes":[{"id":"1","role":"button","name":"Save","visible":true}],"truncated":false}"#;
        let condition = Condition {
            kind: "visible".into(),
            text: Some("Save".into()),
            role: Some("button".into()),
        };
        assert!(condition_matches(one, &condition, false).unwrap());
        let two = one.replace(
            "],",
            ",{\"id\":\"2\",\"role\":\"button\",\"name\":\"Save\",\"visible\":true}],",
        );
        assert!(
            condition_matches(&two, &condition, false)
                .unwrap_err()
                .contains("ambiguous")
        );
        assert!(condition_matches(&one.replace("false", "true"), &condition, false).is_err());
    }
    #[test]
    fn deltas_contain_only_changed_and_removed_nodes() {
        let before = r#"{"nodes":[{"id":"1","name":"old"},{"id":"2","name":"same"},{"id":"3"}]}"#;
        let after = r#"{"nodes":[{"id":"1","name":"new"},{"id":"2","name":"same"}]}"#;
        assert_eq!(
            delta(before, after).unwrap(),
            r#"{"updated":[{"id":"1","name":"new"}],"removed":["3"]}"#
        );
    }
    #[test]
    fn cancellation_wakes_waiters_and_is_not_reset() {
        let token = Cancellation::default();
        let observed = Arc::new(AtomicBool::new(false));
        let copy = observed.clone();
        let _guard = token.on_cancel(move || {
            copy.store(true, Ordering::Release);
        });
        token.cancel();
        assert!(observed.load(Ordering::Acquire));
        assert!(token.check().is_err());
        let immediate = Arc::new(AtomicBool::new(false));
        let copy = immediate.clone();
        let _late = token.on_cancel(move || {
            copy.store(true, Ordering::Release);
        });
        assert!(immediate.load(Ordering::Acquire));
    }
    #[test]
    fn cancellation_drains_the_dispatch_gate_and_blocks_later_input() {
        use std::{sync::mpsc, thread};
        let token = Cancellation::default();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let active = token.clone();
        let dispatch = thread::spawn(move || {
            active.dispatch(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let cancel = token.clone();
        let thread = thread::spawn(move || {
            started_tx.send(()).unwrap();
            cancel.cancel();
            done_tx.send(()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release_tx.send(()).unwrap();
        dispatch.join().unwrap().unwrap();
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
        let mut sent = false;
        assert!(
            token
                .dispatch(|| {
                    sent = true;
                    Ok(())
                })
                .unwrap_err()
                .starts_with("cancelled:")
        );
        assert!(!sent);
    }
    #[test]
    fn cancellation_request_wakes_waiters_without_waiting_for_dispatch() {
        use std::{sync::mpsc, thread};
        let token = Cancellation::default();
        let woke = Arc::new(AtomicBool::new(false));
        let observed = woke.clone();
        let _wake = token.on_cancel(move || {
            observed.store(true, Ordering::Release);
        });
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let active = token.clone();
        let dispatch = thread::spawn(move || {
            active.dispatch(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let requested = token.clone();
        let request = thread::spawn(move || {
            requested.request_cancel();
            done_tx.send(()).unwrap();
        });
        let nonblocking = done_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        let woke_before_drain = woke.load(Ordering::Acquire);
        // Always release the fixture before assertions, even on a regression.
        release_tx.send(()).unwrap();
        dispatch.join().unwrap().unwrap();
        request.join().unwrap();
        assert!(nonblocking && woke_before_drain);
        assert!(token.dispatch(|| Ok(())).is_err());
    }
    #[test]
    fn cancellation_wakes_a_local_sleep_without_polling() {
        let token = Cancellation::default();
        let copy = token.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            copy.cancel();
        });
        let start = Instant::now();
        assert!(
            token
                .sleep(Duration::from_secs(30))
                .unwrap_err()
                .starts_with("cancelled:")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        thread.join().unwrap();
        assert!(token.sleep(Duration::ZERO).is_err());
    }
    #[test]
    fn external_dispatch_permit_surrounds_only_the_send() {
        struct Permit(Arc<AtomicBool>);
        impl Drop for Permit {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let held = Arc::new(AtomicBool::new(false));
        let external = held.clone();
        let token = Cancellation::with_dispatch_guard(move || {
            external.store(true, Ordering::Release);
            Ok(Box::new(Permit(external.clone())))
        });
        token
            .dispatch(|| {
                assert!(held.load(Ordering::Acquire));
                Ok(())
            })
            .unwrap();
        assert!(!held.load(Ordering::Acquire));
        token.sleep(Duration::ZERO).unwrap();
        assert!(!held.load(Ordering::Acquire));
        token.cancel();
        assert!(token.dispatch(|| Ok(())).is_err());
        assert!(!held.load(Ordering::Acquire));
    }
    #[test]
    fn an_external_guard_can_deny_input_before_the_action() {
        let token = Cancellation::with_dispatch_guard(|| Err("host disabled input".into()));
        let mut sent = false;
        assert_eq!(
            token
                .dispatch(|| {
                    sent = true;
                    Ok(())
                })
                .unwrap_err(),
            "host disabled input"
        );
        assert!(!sent);
    }
}
