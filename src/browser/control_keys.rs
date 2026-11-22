//! Bounded, feedback-driven trusted direction keys for one retained page.
//! Policies run synchronously in an isolated world with V8 side-effect checks.
//! Only controller-owned, freshly parsed JSON memory survives between ticks.
use super::*;
use crate::{key_hold::Direction, observation::Cancellation};
use zmcpio::json::Kind;

const TIMEOUT_ERROR: &str = "timeout: browser.control_keys did not finish before its deadline";
const MAX_POLICY_BYTES: usize = 16_384;
const MAX_MEMORY_BYTES: usize = 4096;
const TICK: Duration = Duration::from_millis(20);

// Shared with fixed key holds through the integration patch. The policy world
// retains the same document, element, URL and geometry checks as ordinary holds.
pub(super) fn target_install(marker: &str) -> String {
    format!(
        r#"(()=>{{if(!document.hasFocus()||document.visibilityState!=="visible")return false;const e=document.activeElement;if(!e||!e.isConnected)return false;globalThis[{}]={{document,element:e,url:location.href,width:innerWidth,height:innerHeight,scale:visualViewport?visualViewport.scale:1}};return true}})()"#,
        quote(marker)
    )
}

pub(super) fn target_check(marker: &str) -> String {
    format!(
        r#"(()=>{{const s=globalThis[{}];return !!s&&s.document===document&&s.url===location.href&&s.element.isConnected&&s.element===document.activeElement&&document.hasFocus()&&document.visibilityState==="visible"&&s.width===innerWidth&&s.height===innerHeight&&s.scale===(visualViewport?visualViewport.scale:1)}})()"#,
        quote(marker)
    )
}

fn allowlist(keys: &str) -> Result<Vec<Direction>> {
    if keys.is_empty() || keys.len() > 64 || keys.contains('\0') {
        return Err(
            "control_keys allowed_keys must contain one to four comma-separated directions".into(),
        );
    }
    let mut result = Vec::with_capacity(4);
    for key in keys.split(',') {
        if result.len() == 4 {
            return Err("control_keys allows at most four direction keys".into());
        }
        let key = Direction::parse(key.trim())?;
        if result.contains(&key) {
            return Err("control_keys allowed_keys must be unique directions".into());
        }
        result.push(key);
    }
    Ok(result)
}

fn opposite(a: Direction, b: Direction) -> bool {
    matches!(
        (a, b),
        (Direction::Up, Direction::Down)
            | (Direction::Down, Direction::Up)
            | (Direction::Left, Direction::Right)
            | (Direction::Right, Direction::Left)
    )
}

fn keys_json(keys: impl IntoIterator<Item = Direction>) -> String {
    format!(
        "[{}]",
        keys.into_iter()
            .map(|key| quote(key.browser()))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn exact_fields(value: Value<'_>, expected: &[&str]) -> bool {
    value.kind() == Kind::Object
        && value.members().count() == expected.len()
        && expected
            .iter()
            .all(|name| value.members().filter(|(key, _)| key.matches(name)).count() == 1)
}

#[derive(Debug)]
struct Decision {
    keys: Vec<Direction>,
    done: bool,
    memory: String,
}

fn decision(value: &str, allowed: &[Direction]) -> Result<Decision> {
    if value.len() > 8192 {
        return Err("control_keys policy reply exceeded its bounded envelope".into());
    }
    with_json(value, |root| {
        if root
            .get("status")
            .and_then(Value::string)
            .is_some_and(|s| s.matches("stale"))
        {
            return Err(
                "stale_target: control_keys document, geometry or focus target changed".into(),
            );
        }
        if !exact_fields(root, &["status", "decision", "memory"])
            || !root
                .get("status")
                .and_then(Value::string)
                .is_some_and(|s| s.matches("decision"))
        {
            return Err("control_keys policy returned an invalid envelope".into());
        }
        let result = root
            .get("decision")
            .ok_or("control_keys policy returned no decision")?;
        if !exact_fields(result, &["keys", "done"]) {
            return Err("control_keys policy must return exactly {keys,done}".into());
        }
        let done = result
            .get("done")
            .and_then(Value::boolean)
            .ok_or("control_keys policy done must be boolean")?;
        let keys = result
            .get("keys")
            .ok_or("control_keys policy keys are missing")?;
        if keys.kind() != Kind::Array || keys.elements().count() > 2 {
            return Err(
                "control_keys policy keys must be an array of at most two directions".into(),
            );
        }
        let mut directions = Vec::with_capacity(2);
        for value in keys.elements() {
            let key: String = value
                .string()
                .ok_or("control_keys policy keys must be strings")?
                .chars()
                .collect();
            let key = Direction::parse(&key)?;
            if !allowed.contains(&key) {
                return Err("control_keys policy requested a key outside allowed_keys".into());
            }
            if directions.contains(&key)
                || directions.iter().any(|previous| opposite(*previous, key))
            {
                return Err(
                    "control_keys policy directions must be unique and non-opposite".into(),
                );
            }
            directions.push(key);
        }
        let memory = root.get("memory").ok_or("control_keys memory is missing")?;
        if memory.kind() != Kind::Object || memory.raw().len() > MAX_MEMORY_BYTES {
            return Err("control_keys memory must be a JSON object of at most 4096 bytes".into());
        }
        let mut nodes = 0;
        validate_memory(memory, 0, &mut nodes)?;
        Ok(Decision {
            keys: directions,
            done,
            memory: memory.raw().to_owned(),
        })
    })
}

fn validate_memory(value: Value<'_>, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    if depth > 8 || *nodes > 512 {
        return Err("control_keys memory exceeds depth 8 or 512 values".into());
    }
    match value.kind() {
        Kind::Object => {
            let mut names = BTreeSet::new();
            for (name, child) in value.members() {
                if !names.insert(name.chars().collect::<String>()) {
                    return Err("control_keys memory contains duplicate object fields".into());
                }
                validate_memory(child, depth + 1, nodes)?;
            }
        }
        Kind::Array => {
            for child in value.elements() {
                validate_memory(child, depth + 1, nodes)?;
            }
        }
        Kind::Number if !value.raw().parse::<f64>().is_ok_and(f64::is_finite) => {
            return Err("control_keys memory numbers must be finite".into());
        }
        _ => {}
    }
    Ok(())
}

fn evaluation_value(result: &str) -> Result<String> {
    with_json(result, |root| {
        if let Some(exception) = root.get("exceptionDetails") {
            return Err(format!(
                "control_keys JavaScript failed: {}",
                exception.raw()
            ));
        }
        root.get("result")
            .and_then(|value| value.get("value"))
            .map(|value| value.raw().to_owned())
            .ok_or_else(|| "control_keys evaluation returned no value".into())
    })
}

fn consumed(result: &Result<String>, method: &str) -> bool {
    result.is_ok()
        || result
            .as_ref()
            .is_err_and(|error| error.starts_with(&format!("CDP {method}:")))
}

#[derive(Clone)]
struct Key {
    direction: Direction,
    owned: bool,
    down_pending: Option<u64>,
    up_pending: Option<u64>,
    up_attempted: bool,
    release_error: Option<String>,
}

struct Controller<'a> {
    browser: &'a mut Browser,
    keys: Vec<Key>,
    target: String,
    session: String,
    marker: String,
    context: Option<i64>,
    marker_attempted: bool,
    group_attempted: bool,
    pending: Option<(u64, &'static str)>,
    started: Instant,
    deadline: Instant,
    ticks: u64,
    downs: u64,
    ups: u64,
    memory: String,
    finished: bool,
}

impl Browser {
    pub(crate) fn validate_control_keys(allowed_keys: &str, policy: &str) -> Result<()> {
        allowlist(allowed_keys)?;
        if policy.trim().is_empty() || policy.len() > MAX_POLICY_BYTES || policy.contains('\0') {
            return Err("control_keys policy must contain 1..16384 non-NUL bytes".into());
        }
        Ok(())
    }

    /// Run a bounded, synchronous policy over public observations. Policy code
    /// has no page-world globals; DOM access retains V8's side-effect checks.
    /// An empty active chord is a valid wait. No action is replayed on failure.
    pub fn control_keys(
        &mut self,
        allowed_keys: &str,
        policy: &str,
        timeout_ms: u64,
        canvas: Option<&str>,
        cancellation: &Cancellation,
    ) -> Result<String> {
        Self::validate_control_keys(allowed_keys, policy)?;
        crate::key_hold::validate_duration(timeout_ms)?;
        if let Some(name) = canvas {
            super::canvas::validate_name(name)?;
        }
        cancellation.check()?;
        if self.mobile {
            return Err(
                "unsupported_state: control_keys requires a desktop page with verifiable focus"
                    .into(),
            );
        }
        let allowed = allowlist(allowed_keys)?;
        let started = Instant::now();
        let mut controller = Controller {
            target: self.target.clone(),
            session: self.session.clone(),
            browser: self,
            keys: allowed
                .iter()
                .map(|direction| Key {
                    direction: *direction,
                    owned: false,
                    down_pending: None,
                    up_pending: None,
                    up_attempted: false,
                    release_error: None,
                })
                .collect(),
            marker: format!(
                "__zuse_control_keys_{}_{}",
                std::process::id(),
                PROFILE_ID.fetch_add(1, Ordering::Relaxed)
            ),
            context: None,
            marker_attempted: false,
            group_attempted: false,
            pending: None,
            started,
            deadline: started + Duration::from_millis(timeout_ms),
            ticks: 0,
            downs: 0,
            ups: 0,
            memory: "{}".into(),
            finished: false,
        };
        let result = controller.run(policy, canvas, &allowed, cancellation);
        let elapsed = started.elapsed().as_millis();
        let cleanup = controller.finish();
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(format!(
                r#"{{"ok":true,"reason":"done","timeout_ms":{timeout_ms},"elapsed_ms":{elapsed},"ticks":{},"keydowns":{},"keyups":{},"pressed":{},"released":true,"delivery":"{}","policy_world":"isolated","side_effect_checks":true,"memory":{},"application_confirmation_required":true}}"#,
                controller.ticks,
                controller.downs,
                controller.ups,
                controller.downs != 0,
                if controller.downs == 0 {
                    "none"
                } else {
                    "submitted"
                },
                controller.memory
            )),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(cleanup)) => Err(format!("control_keys cleanup failed: {cleanup}")),
            (Err(error), Err(cleanup)) => {
                Err(format!("{error}; control_keys cleanup failed: {cleanup}"))
            }
        }
    }
}

impl Controller<'_> {
    fn boundary(&self, cancellation: &Cancellation) -> Result<()> {
        cancellation.check()?;
        if Instant::now() >= self.deadline {
            return Err(TIMEOUT_ERROR.into());
        }
        if self.browser.target != self.target || self.browser.session != self.session {
            return Err("stale_target: selected browser page changed during control_keys".into());
        }
        Ok(())
    }

    fn rpc(
        &mut self,
        method: &'static str,
        params: &str,
        cancellation: &Cancellation,
    ) -> Result<String> {
        self.boundary(cancellation)?;
        let deadline = self.deadline.min(Instant::now() + Duration::from_secs(2));
        let id = self
            .browser
            .send_request_until(method, params, true, deadline)?;
        self.pending = Some((id, method));
        let result = self
            .browser
            .response_until(id, method, deadline, Some(cancellation));
        if consumed(&result, method) {
            self.pending = None;
        }
        match result {
            Err(error)
                if self.browser.socket.failed.is_none()
                    && Instant::now() >= self.deadline
                    && error
                        == format!("CDP {method} read: browser response deadline timed out") =>
            {
                Err(TIMEOUT_ERROR.into())
            }
            result => result,
        }
    }

    fn evaluate(
        &mut self,
        expression: &str,
        isolated: bool,
        checked: bool,
        cancellation: &Cancellation,
    ) -> Result<String> {
        self.boundary(cancellation)?;
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if remaining == 0 {
            return Err(TIMEOUT_ERROR.into());
        }
        let timeout = remaining.min(if checked { 50 } else { 100 });
        let context = if isolated {
            format!(
                ",\"contextId\":{}",
                self.context
                    .ok_or("control_keys isolated world is unavailable")?
            )
        } else {
            String::new()
        };
        self.group_attempted = true;
        let result = self.rpc("Runtime.evaluate", &format!(r#"{{"expression":{},"objectGroup":{},"returnByValue":true,"awaitPromise":false,"userGesture":false,"throwOnSideEffect":{checked},"timeout":{timeout}{context}}}"#, quote(expression), quote(&self.marker)), cancellation)?;
        evaluation_value(&result)
    }

    fn run(
        &mut self,
        policy: &str,
        canvas: Option<&str>,
        allowed: &[Direction],
        cancellation: &Cancellation,
    ) -> Result<()> {
        // compileScript requires the Runtime domain, unlike evaluate. Enable
        // it on this selected page session within the same bounded setup.
        // Do not disable it afterwards: existing observation bindings may
        // already use this connection's Runtime event stream.
        self.rpc("Runtime.enable", "{}", cancellation)?;
        let frames = self.rpc("Page.getFrameTree", "{}", cancellation)?;
        let frame = field_string(&frames, &["frameTree", "frame", "id"])?;
        let world = format!("{}.control_keys", self.browser.observation_binding);
        let created = self.rpc(
            "Page.createIsolatedWorld",
            &format!(
                r#"{{"frameId":{},"worldName":{},"grantUniveralAccess":false}}"#,
                quote(&frame),
                quote(&world)
            ),
            cancellation,
        )?;
        self.context = Some(with_json(&created, |root| {
            let context = root
                .get("executionContextId")
                .ok_or("control_keys received no isolated context")?
                .integer()
                .map_err(|_| "control_keys received an invalid isolated context")?;
            if context <= 0 {
                return Err("control_keys received an invalid isolated context".into());
            }
            Ok(context)
        })?);
        self.marker_attempted = true;
        if self.evaluate(&target_install(&self.marker), true, false, cancellation)? != "true" {
            return Err(
                "stale_target: control_keys requires a visible, focused browser document".into(),
            );
        }
        // Syntax is compiled at execution time, before any key. Offline DSL
        // check validates arguments/permissions; it does not parse JavaScript.
        let expression =
            format!("(function(state,memory,control){{'use strict';return (\n{policy}\n);}})");
        let compiled = self.rpc("Runtime.compileScript", &format!(r#"{{"expression":{},"sourceURL":"zuse-control-keys-policy","persistScript":false,"executionContextId":{}}}"#, quote(&expression), self.context.unwrap()), cancellation)?;
        with_json(&compiled, |root| {
            if let Some(exception) = root.get("exceptionDetails") {
                return Err(format!(
                    "control_keys policy syntax failed: {}",
                    exception.raw()
                ));
            }
            Ok(())
        })?;
        self.boundary(cancellation)?;
        self.group_attempted = true;
        let create_timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(50);
        if create_timeout == 0 {
            return Err(TIMEOUT_ERROR.into());
        }
        let created = self.rpc("Runtime.evaluate", &format!(r#"{{"expression":{},"contextId":{},"objectGroup":{},"returnByValue":false,"awaitPromise":false,"userGesture":false,"throwOnSideEffect":true,"timeout":{create_timeout}}}"#,
            quote(&expression), self.context.unwrap(), quote(&self.marker)), cancellation)?;
        let function = with_json(&created, |root| {
            if let Some(exception) = root.get("exceptionDetails") {
                return Err(format!(
                    "control_keys policy creation failed: {}",
                    exception.raw()
                ));
            }
            let result = root
                .get("result")
                .ok_or("control_keys received no policy function")?;
            if !result
                .get("type")
                .and_then(Value::string)
                .is_some_and(|kind| kind.matches("function"))
            {
                return Err("control_keys policy must compile to a function".into());
            }
            result
                .get("objectId")
                .and_then(Value::string)
                .map(|value| value.chars().collect::<String>())
                .ok_or_else(|| "control_keys policy function has no object identity".into())
        })?;
        // This fixed, unchecked function only retains the returned function
        // reference. User policy text is never executed outside side-effect
        // checking; direct eval inside checked code is rejected by V8.
        let bind = format!(
            r#"function(){{const s=globalThis[{}];if(!s||s.document!==document)throw Error('stale_target: control_keys document changed');s.policy=this;return true}}"#,
            quote(&self.marker)
        );
        let bound = self.rpc("Runtime.callFunctionOn", &format!(r#"{{"objectId":{},"functionDeclaration":{},"objectGroup":{},"returnByValue":true,"awaitPromise":false,"userGesture":false}}"#, quote(&function), quote(&bind), quote(&self.marker)), cancellation)?;
        if evaluation_value(&bound)? != "true" {
            return Err("control_keys could not retain its policy".into());
        }
        let canvas_expression = canvas
            .map(|name| self.browser.canvas_condition(name, "state"))
            .transpose()?;
        loop {
            self.boundary(cancellation)?;
            // Check the embedding's revocable input permit even on empty-key
            // ticks. A permit is never held while waiting for a CDP reply.
            cancellation.dispatch(|| Ok(()))?;
            self.boundary(cancellation)?;
            for index in 0..self.keys.len() {
                self.ack_down(index, self.deadline, Some(cancellation))?;
            }
            self.boundary(cancellation)?;
            if self.ticks == 512 {
                return Err("control_keys tick budget exceeded".into());
            }
            let sampled = Instant::now();
            let state = if let Some(expression) = &canvas_expression {
                let state = self.evaluate(expression, false, false, cancellation)?;
                if state.len() > super::canvas::MAX_BYTES {
                    return Err("control_keys canvas observation exceeded 256 KiB".into());
                }
                state
            } else {
                "null".into()
            };
            let control = format!(
                r#"{{"tick":{},"elapsed_ms":{},"keys":{},"sample_age_ms":{}}}"#,
                self.ticks,
                self.started.elapsed().as_millis(),
                keys_json(
                    self.keys
                        .iter()
                        .filter(|key| key.owned)
                        .map(|key| key.direction)
                ),
                sampled.elapsed().as_millis()
            );
            // These detached observation objects exist before checked policy
            // evaluation begins. V8 therefore rejects writes to them, while
            // allowing the policy's freshly parsed private memory to change.
            // Object.freeze itself is rejected by some V8 side-effect checks.
            let prepare = format!(
                r#"(()=>{{if(!({}))return false;const s=globalThis[{}];s.state=JSON.parse({});s.control=JSON.parse({});return true}})()"#,
                target_check(&self.marker),
                quote(&self.marker),
                quote(&state),
                quote(&control)
            );
            if self.evaluate(&prepare, true, false, cancellation)? != "true" {
                return Err(
                    "stale_target: control_keys target changed before policy evaluation".into(),
                );
            }
            let wrapper = policy_expression(&self.marker, &self.memory);
            let value = self.evaluate(&wrapper, true, true, cancellation)?;
            let selected = decision(&value, allowed)?;
            self.boundary(cancellation)?;
            self.ticks += 1;
            self.memory = selected.memory;
            if selected.done {
                return Ok(());
            }
            self.change(&selected.keys, cancellation)?;
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            cancellation.sleep(remaining.min(TICK))?;
        }
    }

    fn ack_down(
        &mut self,
        index: usize,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<()> {
        if let Some(id) = self.keys[index].down_pending {
            let result =
                self.browser
                    .response_until(id, "Input.dispatchKeyEvent", deadline, cancellation);
            if consumed(&result, "Input.dispatchKeyEvent") {
                self.keys[index].down_pending = None;
            }
            result?;
        }
        Ok(())
    }

    fn send_up(&mut self, index: usize, deadline: Instant) -> Result<()> {
        let slot = &mut self.keys[index];
        if !slot.owned || slot.up_attempted {
            return Ok(());
        }
        slot.up_attempted = true;
        let key = slot.direction.browser();
        let vk = slot.direction.virtual_key();
        match self.browser.send_request_until("Input.dispatchKeyEvent", &format!(r#"{{"type":"keyUp","key":"{key}","code":"{key}","windowsVirtualKeyCode":{vk},"modifiers":0}}"#), true, deadline) {
            Ok(id) => { slot.up_pending = Some(id); self.ups += 1; Ok(()) }
            Err(error) => {
                // A pre-dispatch failure sent no frame; cleanup may make the
                // first release attempt under its separate deadline. A failed
                // write is ambiguous and must never replay that input frame.
                if self.browser.socket.failed.is_none() { slot.up_attempted = false; }
                else { slot.release_error = Some(error.clone()); }
                Err(error)
            }
        }
    }

    fn ack_up(
        &mut self,
        index: usize,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
    ) -> Result<()> {
        if let Some(id) = self.keys[index].up_pending {
            let result =
                self.browser
                    .response_until(id, "Input.dispatchKeyEvent", deadline, cancellation);
            if consumed(&result, "Input.dispatchKeyEvent") {
                self.keys[index].up_pending = None;
                match &result {
                    Ok(_) => {
                        self.keys[index].owned = false;
                        self.keys[index].up_attempted = false;
                    }
                    Err(error) => self.keys[index].release_error = Some(error.clone()),
                }
            }
            result?;
        }
        Ok(())
    }

    fn change(&mut self, selected: &[Direction], cancellation: &Cancellation) -> Result<()> {
        self.boundary(cancellation)?;
        let removed: Vec<usize> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.owned && !selected.contains(&key.direction))
            .map(|(index, _)| index)
            .collect();
        // Write all old keyups before waiting for any release acknowledgement.
        // New keydowns require confirmed old releases, including opposites.
        let mut errors = Vec::new();
        for index in &removed {
            if let Err(error) = self.send_up(*index, self.deadline) {
                errors.push(error);
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        for index in &removed {
            self.ack_up(*index, self.deadline, Some(cancellation))?;
        }
        self.boundary(cancellation)?;
        let added: Vec<usize> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| !key.owned && selected.contains(&key.direction))
            .map(|(index, _)| index)
            .collect();
        if !removed.is_empty()
            && !added.is_empty()
            && self.evaluate(&target_check(&self.marker), true, true, cancellation)? != "true"
        {
            return Err(
                "stale_target: control_keys target changed during the key transition".into(),
            );
        }
        cancellation.dispatch(|| {
            for index in added {
                self.boundary(cancellation)?;
                let slot = &mut self.keys[index];
                let key = slot.direction.browser();
                let vk = slot.direction.virtual_key();
                slot.owned = true; // Arm cleanup before a potentially partial write.
                slot.up_attempted = false;
                slot.release_error = None;
                match self.browser.send_request_until("Input.dispatchKeyEvent", &format!(r#"{{"type":"rawKeyDown","key":"{key}","code":"{key}","windowsVirtualKeyCode":{vk},"modifiers":0}}"#), true, self.deadline) {
                    Ok(id) => { slot.down_pending = Some(id); self.downs += 1; }
                    Err(error) => {
                        if self.browser.socket.failed.is_none() { slot.owned = false; }
                        return Err(error);
                    }
                }
            }
            Ok(())
        })
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut errors = Vec::new();
        // Release every owned key first. Never wait for a slow policy, keydown
        // or sibling release before submitting the remaining releases.
        for index in 0..self.keys.len() {
            if let Err(error) = self.send_up(index, deadline) {
                errors.push(format!(
                    "{} release unconfirmed: {error}",
                    self.keys[index].direction.browser()
                ));
            }
        }
        for index in 0..self.keys.len() {
            if let Err(error) = self.ack_down(index, deadline, None) {
                errors.push(format!("keydown: {error}"));
            }
        }
        for index in 0..self.keys.len() {
            if let Err(error) = self.ack_up(index, deadline, None) {
                errors.push(format!(
                    "{} release unconfirmed: {error}",
                    self.keys[index].direction.browser()
                ));
            }
            if self.keys[index].owned {
                errors.push(format!(
                    "{} release unconfirmed: {}",
                    self.keys[index].direction.browser(),
                    self.keys[index]
                        .release_error
                        .as_deref()
                        .unwrap_or("no successful keyup acknowledgement")
                ));
            }
        }
        if let Some((id, method)) = self.pending.take() {
            let result = self.browser.response_until(id, method, deadline, None);
            match result {
                Ok(result) => {
                    // A late successful decision never changes a failed run
                    // into success. A real late error must still be reported.
                    if let Err(error) = with_json(&result, |root| {
                        if let Some(exception) = root.get("exceptionDetails") {
                            return Err(exception.raw().to_owned());
                        }
                        Ok(())
                    }) {
                        errors.push(error);
                    }
                }
                Err(error) => errors.push(error),
            }
        }
        if self.marker_attempted && self.browser.socket.failed.is_none() {
            let cleanup_deadline = Instant::now() + Duration::from_millis(500);
            let cleanup = format!("delete globalThis[{}]", quote(&self.marker));
            let params = format!(
                r#"{{"expression":{},"contextId":{},"objectGroup":{},"returnByValue":true,"awaitPromise":false,"userGesture":false,"timeout":500}}"#,
                quote(&cleanup),
                self.context.unwrap(),
                quote(&self.marker)
            );
            // Destroyed execution contexts contain no retained marker. Cleanup
            // failure is still exposed; no claim of successful cleanup follows.
            if let Err(error) = self
                .browser
                .request_until("Runtime.evaluate", &params, true, cleanup_deadline, None)
                .and_then(|result| evaluation_value(&result))
                .and_then(|value| {
                    if value == "true" {
                        Ok(())
                    } else {
                        Err("target marker deletion was not confirmed".into())
                    }
                })
            {
                errors.push(format!("target marker cleanup: {error}"));
            }
        }
        if self.group_attempted && self.browser.socket.failed.is_none() {
            let deadline = Instant::now() + Duration::from_millis(500);
            if let Err(error) = self.browser.request_until(
                "Runtime.releaseObjectGroup",
                &format!(r#"{{"objectGroup":{}}}"#, quote(&self.marker)),
                true,
                deadline,
                None,
            ) {
                errors.push(format!("policy reference cleanup: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl Drop for Controller<'_> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn policy_expression(marker: &str, memory: &str) -> String {
    // A retained function is called as a value. Policy source cannot splice a
    // return into this fixed wrapper to bypass its guards or output validation.
    format!(
        r#"(function(memoryText){{
      'use strict';
      const valid=()=>({});
      if(!valid())return {{status:'stale'}};
      const observation=globalThis[{}];
      const state=observation.state, control=observation.control;
      {}
    }})({})"#,
        target_check(marker),
        quote(marker),
        POLICY_BODY,
        quote(memory)
    )
}

const POLICY_BODY: &str = r#"
const memory=JSON.parse(memoryText);
const policy=observation.policy;
const output=policy(state,memory,control);
if(!valid())return {status:'stale'};
const plain=value=>value!==null && typeof value==='object' &&
  (Object.getPrototypeOf(value)===Object.prototype || Object.getPrototypeOf(value)===null);
const fail=message=>{throw Error('control_keys policy: '+message);};
if(!plain(output))fail('return a plain {keys,done} object');
const fields=Reflect.ownKeys(output);
if(fields.length!==2 || !fields.includes('keys') || !fields.includes('done'))fail('only keys and done fields are allowed');
const descriptors=Object.getOwnPropertyDescriptors(output);
if(!('value' in descriptors.keys)||!('value' in descriptors.done))fail('decision getters are forbidden');
const requested=descriptors.keys.value, done=descriptors.done.value;
if(typeof done!=='boolean' || !Array.isArray(requested) || Object.getPrototypeOf(requested)!==Array.prototype || requested.length>2)fail('expected at most two keys and a boolean done');
if(Reflect.ownKeys(requested).length!==requested.length+1)fail('keys must be a dense plain array');
for(let i=0;i<requested.length;i++) {
  const entry=Object.getOwnPropertyDescriptor(requested,String(i));
  if(!entry || !('value' in entry) || typeof entry.value!=='string' || entry.value.length>16)fail('keys must contain bounded strings without getters');
}
let nodes=0;
const checked=(value,depth,parents)=>{
  if(++nodes>512 || depth>8)fail('memory exceeds depth 8 or 512 values');
  if(value===null || typeof value==='boolean')return;
  if(typeof value==='string'){if(value.length>4096)fail('memory exceeds 4096 bytes');return;}
  if(typeof value==='number'){if(!Number.isFinite(value))fail('memory numbers must be finite');return;}
  if(typeof value!=='object' || (!Array.isArray(value) && !plain(value)))fail('memory must contain only JSON values');
  if(parents.includes(value))fail('memory must not be cyclic');
  const names=Reflect.ownKeys(value);
  if(names.length>513)fail('memory exceeds 512 values');
  const branch=parents.concat([value]);
  if(Array.isArray(value)) {
    if(Object.getPrototypeOf(value)!==Array.prototype || names.length!==value.length+1 || value.length>512)fail('memory arrays must be plain, dense and bounded');
    for(let i=0;i<value.length;i++) {
      const d=Object.getOwnPropertyDescriptor(value,String(i));
      if(!d || !('value' in d))fail('memory getters are forbidden');
      checked(d.value,depth+1,branch);
    }
  } else for(const name of names) {
    if(typeof name!=='string' || name.length>4096)fail('memory fields must be bounded strings');
    const d=Object.getOwnPropertyDescriptor(value,name);
    if(!d || !('value' in d) || !d.enumerable)fail('memory getters and hidden fields are forbidden');
    checked(d.value,depth+1,branch);
  }
};
if(!plain(memory))fail('memory must remain a plain object');
checked(memory,0,[]);
const serialized=JSON.stringify(memory);
if(serialized.length>4096)fail('memory exceeds 4096 bytes');
return {status:'decision',decision:{keys:requested,done},memory:JSON.parse(serialized)};
"#;

#[cfg(test)]
mod tests {
    use super::*;
    include!("../../tests/support/browser_control_keys.rs");
}
