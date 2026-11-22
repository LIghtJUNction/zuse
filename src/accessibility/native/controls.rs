//! One bounded, coordinate-free traversal retains every native object until
//! selection and dispatch finish. Child-index paths are output, never re-used
//! to resolve the selected control after matching it.
use super::*;
use crate::{
    accessibility::controls::{Operation, Query},
    observation::Cancellation,
};
use std::sync::{MutexGuard, TryLockError};

mod diagnostics;
mod reveal;
mod wait_recovery;
mod windows;

#[cfg(test)]
mod dispatch_tests;

use diagnostics::Context;
use wait_recovery::ReadFailure;

pub(in crate::accessibility) fn enumerate_windows(
    expected_pid: u32,
    cancellation: &Cancellation,
) -> Result<String> {
    windows::run(expected_pid, cancellation)
}

const MAX_NODES: usize = 4096;
const MAX_BYTES: usize = 2 << 20;

struct Budget<'a> {
    api: &'static Api,
    deadline: Instant,
    cancellation: &'a Cancellation,
}
impl Budget<'_> {
    fn check(&self) -> Result<()> {
        self.cancellation.check()?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timeout: scoped accessibility query reached its deadline".into());
        }
        // A blocked native request may consume only the remaining budget,
        // capped at 250 ms so cancellation is revisited between requests.
        let millis = remaining.as_millis().clamp(1, 250) as c_int;
        unsafe { (self.api.set_timeout)(millis, millis) };
        Ok(())
    }
    fn call<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.check()?;
        let result = f();
        self.check()?;
        result
    }
}
impl Drop for Budget<'_> {
    fn drop(&mut self) {
        // ACCESS remains held until this guard restores the ordinary adapter
        // timeouts. No other operation sees our temporary per-request limit.
        unsafe { (self.api.set_timeout)(500, 1000) };
    }
}

fn access(deadline: Instant, cancellation: &Cancellation) -> Result<MutexGuard<'static, ()>> {
    loop {
        cancellation.check()?;
        if Instant::now() >= deadline {
            return Err("timeout: waiting for native accessibility access".into());
        }
        match ACCESS.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => return Err("accessibility state is poisoned".into()),
            Err(TryLockError::WouldBlock) => {
                cancellation.sleep(
                    Duration::from_millis(5)
                        .min(deadline.saturating_duration_since(Instant::now())),
                )?;
            }
        }
    }
}

fn resolve(path: &str, budget: &Budget<'_>) -> Result<Object<'static>> {
    let api = budget.api;
    let mut object = Context::new(path, "0", "resolve").read(
        "Desktop",
        budget.call(|| api.object(unsafe { (api.desktop)(0) })),
    )?;
    let mut current_path = "0".to_owned();
    for index in path.split('/').skip(1) {
        let context = Context::new(path, &current_path, "resolve");
        context.read("Budget", budget.check())?;
        unsafe { (api.clear_cache)(object.ptr) };
        let index = context.read(
            "ChildIndex",
            index
                .parse::<usize>()
                .map_err(|_| "invalid accessibility child index".into()),
        )?;
        let count = context.read("ChildCount", budget.call(|| api.count(object.ptr)))?;
        if index >= count {
            return Err(context.error(
                &format!("ChildAtIndex({index})"),
                "stale_target: accessibility scope path no longer exists".into(),
                false,
            ));
        }
        object = context.read(
            &format!("ChildAtIndex({index})"),
            budget.call(|| api.child(object.ptr, index)),
        )?;
        current_path.push_str(&format!("/{index}"));
    }
    Ok(object)
}

fn owned_string(api: &Api, pointer: *mut c_char, error: ErrPtr, limit: usize) -> Result<String> {
    let value = if pointer.is_null() {
        Err("accessibility string is unavailable".to_owned())
    } else {
        // SAFETY: AT-SPI returned an owned C string, released exactly once.
        unsafe {
            let bytes = CStr::from_ptr(pointer).to_bytes();
            let result = if bytes.len() > limit {
                Err("unsupported_state: accessibility string exceeds query bound".to_owned())
            } else {
                std::str::from_utf8(bytes)
                    .map(str::to_owned)
                    .map_err(|_| "accessibility string is not UTF-8".to_owned())
            };
            (api.free)(pointer.cast());
            result
        }
    };
    api.error(error)?;
    value
}

fn text(object: &Object<'_>, role: &str, budget: &Budget<'_>) -> Result<String> {
    if role.to_ascii_lowercase().contains("password") {
        return Ok("[redacted]".into());
    }
    let api = object.api;
    let interface = budget.call(|| {
        let pointer = unsafe { (api.text_iface)(object.ptr) };
        if pointer.is_null() {
            Ok(None)
        } else {
            api.object(pointer).map(Some)
        }
    })?;
    let Some(interface) = interface else {
        return Ok(String::new());
    };
    let count = budget.call(|| {
        let mut error = core::ptr::null_mut();
        let count = unsafe { (api.text_count)(interface.ptr, &mut error) };
        api.error(error)?;
        Ok(count)
    })?;
    if !(0..=65536).contains(&count) {
        return Err("unsupported_state: accessible text exceeds query bound".into());
    }
    if count == 0 {
        return Ok(String::new());
    }
    budget.call(|| {
        let mut error = core::ptr::null_mut();
        let pointer = unsafe { (api.text)(interface.ptr, 0, count, &mut error) };
        owned_string(api, pointer, error, 65536)
    })
}

fn actions(object: &Object<'_>, budget: &Budget<'_>) -> Result<Vec<(usize, String)>> {
    let api = object.api;
    let interface = budget.call(|| {
        let pointer = unsafe { (api.action_iface)(object.ptr) };
        if pointer.is_null() {
            Ok(None)
        } else {
            api.object(pointer).map(Some)
        }
    })?;
    let Some(interface) = interface else {
        return Ok(Vec::new());
    };
    let count = budget.call(|| {
        let mut error = core::ptr::null_mut();
        let count = unsafe { (api.actions_count)(interface.ptr, &mut error) };
        api.error(error)?;
        Ok(count)
    })?;
    if !(0..=64).contains(&count) {
        return Err("unsupported_state: accessible actions exceed query bound".into());
    }
    let mut result = Vec::new();
    for index in 0..count {
        let name = budget.call(|| {
            let mut error = core::ptr::null_mut();
            let pointer = unsafe { (api.action_name)(interface.ptr, index, &mut error) };
            owned_string(api, pointer, error, 4096)
        })?;
        result.push((index as usize, name));
    }
    Ok(result)
}

#[derive(Clone, Copy)]
struct States {
    enabled: bool,
    focusable: bool,
    focused: bool,
    showing: bool,
    visible: bool,
    defunct: bool,
}
fn states(object: &Object<'_>, budget: &Budget<'_>) -> Result<States> {
    let api = object.api;
    let states = budget.call(|| api.object(unsafe { (api.state_set)(object.ptr) }))?;
    let has = |state| unsafe { (api.state_contains)(states.ptr, state) != 0 };
    Ok(States {
        enabled: has(8),
        focusable: has(11),
        focused: has(12),
        showing: has(25),
        visible: has(30),
        defunct: has(6),
    })
}

struct Node {
    object_index: usize,
    path: String,
    pid: u32,
    role: String,
    name: String,
    text: String,
    states: States,
    actions: Vec<(usize, String)>,
}
impl Node {
    fn json(&self) -> String {
        self.json_with_states(self.states)
    }
    fn json_with_states(&self, s: States) -> String {
        let actions = self
            .actions
            .iter()
            .map(|(index, name)| format!(r#"{{"index":{index},"name":{}}}"#, quote(name)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"path":{},"pid":{},"role":{},"name":{},"text":{},"enabled":{},"focusable":{},"focused":{},"visible":{},"showing":{},"actions":[{actions}]}}"#,
            quote(&self.path),
            self.pid,
            quote(&self.role),
            quote(&self.name),
            quote(&self.text),
            s.enabled,
            s.focusable,
            s.focused,
            s.visible,
            s.showing
        )
    }
    fn visible(&self) -> bool {
        self.states.visible && self.states.showing && !self.states.defunct
    }
    fn ready(&self, operation: Operation<'_>) -> bool {
        self.states.visible
            && !self.states.defunct
            && self.states.enabled
            && (matches!(operation, Operation::Reveal) || self.states.showing)
            && (!matches!(operation, Operation::Focus) || self.states.focusable)
    }
}

struct Scan {
    scope: String,
    objects: Vec<Object<'static>>,
    matches: Vec<Node>,
    truncated: bool,
    bytes: usize,
}
fn scan_read<T>(
    path: &str,
    operation: &'static str,
    result: Result<T>,
) -> std::result::Result<T, ReadFailure> {
    result.map_err(|message| ReadFailure::at(path, operation, message))
}
impl Scan {
    fn new(
        root: Object<'static>,
        query: &Query<'_>,
        budget: &Budget<'_>,
    ) -> std::result::Result<Self, ReadFailure> {
        let mut scan = Self {
            scope: query.scope.to_owned(),
            objects: Vec::new(),
            matches: Vec::new(),
            truncated: false,
            bytes: 0,
        };
        let mut pending = vec![(root, query.scope.to_owned())];
        let api = budget.api;
        let mut identity_query = *query;
        identity_query.text = None;
        while let Some((object, path)) = pending.pop() {
            scan_read(&path, "Budget", budget.check())?;
            unsafe { (api.clear_cache)(object.ptr) };
            let object_index = scan.objects.len();
            scan.objects.push(object);
            let object = &scan.objects[object_index];
            let role = scan_read(
                &path,
                "Role",
                budget.call(|| api.identity_string(object.ptr, api.role, 4096)),
            )?;
            let name = scan_read(
                &path,
                "Name",
                budget.call(|| api.identity_string(object.ptr, api.name, 65536)),
            )?;
            if identity_query.matches(&role, &name, "") {
                let pid = scan_read(
                    &path,
                    "ProcessId",
                    budget.call(|| {
                        let mut error = core::ptr::null_mut();
                        let pid = unsafe { (api.process_id)(object.ptr, &mut error) };
                        api.error(error)?;
                        Ok(pid)
                    }),
                )?;
                if pid == query.expected_pid {
                    let value = scan_read(&path, "Text", text(object, &role, budget))?;
                    if query.matches(&role, &name, &value) {
                        let node = Node {
                            object_index,
                            path: path.clone(),
                            pid,
                            role,
                            name,
                            text: value,
                            states: scan_read(&path, "States", states(object, budget))?,
                            actions: scan_read(&path, "Actions", actions(object, budget))?,
                        };
                        scan.bytes += node.json().len();
                        if scan.bytes > MAX_BYTES - 2048 {
                            scan.truncated = true;
                            break;
                        }
                        scan.matches.push(node);
                    }
                }
            }
            let count = scan_read(&path, "ChildCount", budget.call(|| api.count(object.ptr)))?;
            if path.split('/').count() > MAX_DEPTH {
                scan.truncated |= count != 0;
                continue;
            }
            let capacity = MAX_NODES.saturating_sub(scan.objects.len() + pending.len());
            scan.truncated |= count > capacity;
            for index in (0..count.min(capacity)).rev() {
                let child_path = format!("{path}/{index}");
                if child_path.len() > 1024 {
                    scan.truncated = true;
                    continue;
                }
                // This request is sent to the parent, not the returned child.
                // A vanished scope root must never count as a retryable child.
                let child = scan_read(
                    &path,
                    "ChildAtIndex",
                    budget.call(|| api.child(object.ptr, index)),
                )?;
                pending.push((child, child_path));
            }
        }
        Ok(scan)
    }
    fn receipt(&self, query: &Query<'_>, visible_only: bool) -> String {
        let matches = self
            .matches
            .iter()
            .filter(|node| !visible_only || node.visible())
            .collect::<Vec<_>>();
        format!(
            r#"{{"backend":"atspi","scope":{},"expected_pid":{},"matches":[{}],"count":{},"visited":{},"truncated":{},"errors":0}}"#,
            quote(query.scope),
            query.expected_pid,
            matches
                .iter()
                .map(|node| node.json())
                .collect::<Vec<_>>()
                .join(","),
            matches.len(),
            self.objects.len(),
            self.truncated
        )
    }
}

fn choose(count: usize, index: Option<usize>, complete: bool) -> Result<Option<usize>> {
    if !complete {
        return Err(
            "unsupported_state: scoped accessibility query is incomplete; no input was sent".into(),
        );
    }
    if index.is_none() && count > 1 {
        return Err(format!(
            "ambiguous_target: {count} controls match; narrow the scope/name or specify index"
        ));
    }
    Ok(index.or(Some(0)).filter(|&index| index < count))
}

fn parent(
    object: &Object<'_>,
    budget: &Budget<'_>,
    context: Context<'_>,
    step: usize,
) -> Result<Object<'static>> {
    let api = budget.api;
    context.read(
        &format!("GetParent({step})"),
        budget.call(|| {
            // Parent relations may change while the host grants an input permit.
            // Refresh this retained ancestor instead of resolving an index path.
            unsafe { (api.clear_cache)(object.ptr) };
            let mut error = core::ptr::null_mut();
            let pointer = unsafe { (api.parent)(object.ptr, &mut error) };
            if let Err(error) = api.error(error) {
                if !pointer.is_null() {
                    unsafe { (api.unref)(pointer) };
                }
                return Err(error);
            }
            api.object(pointer)
        }),
    )
}

fn check_membership(
    object: &Object<'_>,
    scope: &Object<'_>,
    budget: &Budget<'_>,
    context: Context<'_>,
) -> Result<()> {
    if object.ptr != scope.ptr {
        let mut current = parent(object, budget, context, 0)?;
        let mut inside = current.ptr == scope.ptr;
        for step in 1..=MAX_DEPTH {
            if inside {
                break;
            }
            current = parent(&current, budget, context, step)?;
            inside = current.ptr == scope.ptr;
        }
        if !inside {
            return Err(context.error(
                "Membership",
                "stale_target: matched control left the selected scope".into(),
                context.native_call_started,
            ));
        }
    }
    Ok(())
}

fn fresh_node(
    node: &Node,
    object: &Object<'_>,
    scope: &Object<'_>,
    operation: Operation<'_>,
    budget: &Budget<'_>,
    context: Context<'_>,
) -> Result<States> {
    let scope_context = Context {
        path: context.scope,
        ..context
    };
    scope_context.read(
        "ScopeIdentity",
        budget.call(|| {
            check_identity_with_check(
                scope,
                IdentityChecks {
                    pid: Some(node.pid),
                    role: None,
                    name: None,
                },
                || budget.check(),
            )
        }),
    )?;
    if scope_context
        .read("ScopeStateSet", states(scope, budget))?
        .defunct
    {
        return Err(scope_context.error(
            "ScopeStateSet",
            "stale_target: selected accessibility scope was destroyed".into(),
            context.native_call_started,
        ));
    }
    context.read(
        "Identity",
        budget.call(|| {
            check_identity_with_check(
                object,
                IdentityChecks {
                    pid: Some(node.pid),
                    role: Some(&node.role),
                    name: Some(&node.name),
                },
                || budget.check(),
            )
        }),
    )?;
    check_membership(object, scope, budget, context)?;
    let fresh = context.read("StateSet", states(object, budget))?;
    if !fresh.visible
        || !fresh.enabled
        || fresh.defunct
        || (!matches!(operation, Operation::Reveal) && !fresh.showing)
        || (matches!(operation, Operation::Focus) && !fresh.focusable)
    {
        return Err(context.error(
            "Readiness",
            "stale_target: matched control is no longer ready for input".into(),
            context.native_call_started,
        ));
    }
    Ok(fresh)
}

fn named_action(
    object: &Object<'_>,
    expected: &str,
    budget: &Budget<'_>,
    context: Context<'_>,
) -> Result<usize> {
    let current = context.read("Actions", actions(object, budget))?;
    let matching = current
        .iter()
        .filter(|(_, name)| name == expected)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(context.error(
            "ActionName",
            format!(
                "unsupported_state: control does not advertise exactly one action named {}",
                quote(expected)
            ),
            false,
        ));
    }
    Ok(matching[0].0)
}

fn invoke_node(
    scan: &Scan,
    selected: usize,
    operation: Operation<'_>,
    budget: &Budget<'_>,
) -> Result<String> {
    let node = &scan.matches[selected];
    let object = &scan.objects[node.object_index];
    let scope = &scan.objects[0];
    if matches!(operation, Operation::Reveal) {
        return reveal::invoke(scan, selected, budget);
    }
    let api = budget.api;
    let context = Context::new(&scan.scope, &node.path, "pre_dispatch");
    fresh_node(node, object, scope, operation, budget, context)?;
    let (interface, action_index, action_name) = match operation {
        Operation::Focus => (
            context.read(
                "ComponentIface",
                budget.call(|| api.object(unsafe { (api.component_iface)(object.ptr) })),
            )?,
            None,
            None,
        ),
        Operation::Reveal => {
            unreachable!("reveal dispatches through its single-submission verifier")
        }
        Operation::Action(expected) => {
            let index = named_action(object, expected, budget, context)?;
            (
                context.read(
                    "ActionIface",
                    budget.call(|| api.object(unsafe { (api.action_iface)(object.ptr) })),
                )?,
                Some(index),
                Some(expected),
            )
        }
    };
    let method = if action_index.is_some() {
        "DoAction"
    } else {
        "GrabFocus"
    };
    let mut native_call_started = false;
    let phase = std::cell::Cell::new("permission");
    // Acquiring a host permit may block. Revalidate after it returns, outside
    // the dispatch mutex; no read RPC is added to the input critical section.
    let result = budget.cancellation.dispatch_prepared(|| {
        phase.set("prepare");
        let prepare = Context { phase: "prepare", ..context };
        fresh_node(node, object, scope, operation, budget, prepare)?;
        if let (Some(expected), Some(index)) = (action_name, action_index)
            && named_action(object, expected, budget, prepare)? != index {
                return Err(prepare.error("ActionIndex",
                    "stale_target: advertised action index changed while acquiring input permission".into(), false));
        }
        phase.set("dispatch");
        Ok(())
    }, |()| {
        budget.check()?;
        let mut error = core::ptr::null_mut();
        native_call_started = true;
        let success = unsafe {
            match action_index {
                Some(index) => (api.do_action)(interface.ptr, index as c_int, &mut error),
                None => (api.grab_focus)(interface.ptr, &mut error),
            }
        };
        api.error(error)?;
        if success == 0 { return Err("application rejected the semantic accessibility operation".into()); }
        Ok(())
    });
    if let Err(error) = result {
        if !native_call_started && phase.get() == "prepare" {
            return Err(error); // The exact failed read already carries context.
        }
        let phase = if native_call_started {
            "native_input"
        } else {
            phase.get()
        };
        let error = if native_call_started {
            format!("{error}; native input method was called once; input was not retried")
        } else {
            error
        };
        return Err(Context { phase, ..context }.error(method, error, native_call_started));
    }
    budget.check().map_err(|error| {
        Context {
            phase: "verify",
            ..context
        }
        .error(
            method,
            format!("{error}; native input method was called once; input was not retried"),
            true,
        )
    })?;
    let operation = if action_index.is_some() {
        "action"
    } else {
        "focus"
    };
    Ok(format!(
        r#"{{"operation":{},"target":{},"action":{},"action_index":{},"native_call_started":true,"completed":true}}"#,
        quote(operation),
        node.json(),
        action_name.map_or_else(|| "null".into(), quote),
        action_index.map_or_else(|| "null".into(), |index| index.to_string())
    ))
}

pub(in crate::accessibility) fn run(
    query: &Query<'_>,
    operation: Option<Operation<'_>>,
    index: Option<usize>,
    timeout_ms: u64,
    waiting: bool,
    cancellation: &Cancellation,
) -> Result<String> {
    let context = Context::new(query.scope, query.scope, "initialize");
    context.read("Cancellation", cancellation.check())?;
    let start = Instant::now();
    let deadline = start + Duration::from_millis(timeout_ms);
    let _access = context.read("Access", access(deadline, cancellation))?;
    context.read("Cancellation", cancellation.check())?;
    let startup = deadline.saturating_duration_since(Instant::now());
    if startup.is_zero() {
        return Err(context.error(
            "Initialize",
            "timeout: accessibility initialization deadline expired".into(),
            false,
        ));
    }
    let api = context.read(
        "Initialize",
        load_bounded(startup.as_millis().clamp(1, 250) as c_int),
    )?;
    let budget = Budget {
        api,
        deadline,
        cancellation,
    };
    run_scoped(query, operation, index, waiting, start, &budget)
}

fn run_scoped(
    query: &Query<'_>,
    operation: Option<Operation<'_>>,
    index: Option<usize>,
    waiting: bool,
    start: Instant,
    budget: &Budget<'_>,
) -> Result<String> {
    let api = budget.api;
    let cancellation = budget.cancellation;
    let deadline = budget.deadline;
    let context = Context::new(query.scope, query.scope, "scope");
    // Retain the original scope independently of every attempted scan. A
    // disappearing descendant may release the failed scan's objects; it must
    // not release or silently rebind the selected native scope.
    let root = resolve(query.scope, budget)?;
    let scope_pointer = root.ptr;
    let mut read_retries = 0usize;
    loop {
        context.read("Budget", budget.check())?;
        context.read(
            "Identity",
            budget.call(|| {
                check_identity_with_check(
                    &root,
                    IdentityChecks {
                        pid: Some(query.expected_pid),
                        role: None,
                        name: None,
                    },
                    || budget.check(),
                )
            }),
        )?;
        if context.read("StateSet", states(&root, budget))?.defunct {
            return Err(context.error(
                "StateSet",
                "stale_target: selected accessibility scope was destroyed".into(),
                false,
            ));
        }
        let scan_root = context.read(
            "RefScope",
            budget.call(|| {
                // SAFETY: root owns a live GLib reference for this whole call.
                // The extra reference belongs to Scan and is released on failure.
                api.object(unsafe { (api.ref_object)(root.ptr) })
            }),
        )?;
        match Scan::new(scan_root, query, budget) {
            Ok(scan) => {
                if !waiting {
                    return Ok(scan.receipt(query, false));
                }
                let selected = Context {
                    phase: "select",
                    ..context
                }
                .read(
                    "Match",
                    choose(
                        scan.matches.len(),
                        if operation.is_some() { index } else { Some(0) },
                        !scan.truncated,
                    ),
                )?;
                if let Some(selected) = selected {
                    if let Some(operation) = operation {
                        if scan.matches[selected].ready(operation) {
                            let result = invoke_node(&scan, selected, operation, budget)?;
                            return Ok(format!(
                                r#"{{"status":"completed","elapsed_ms":{},{}"#,
                                start.elapsed().as_millis(),
                                &result[1..]
                            ));
                        }
                    } else if scan.matches.iter().any(Node::visible) {
                        let result = scan.receipt(query, true);
                        return Ok(format!(
                            r#"{{"status":"satisfied","elapsed_ms":{},"read_retries":{},{}"#,
                            start.elapsed().as_millis(),
                            read_retries,
                            &result[1..]
                        ));
                    }
                }
            }
            Err(error) if error.retryable(query.scope, waiting, operation) => {
                // No partial matches survive, and no action branch is retried.
                // Continue only through the original deadline/cancellation and
                // exact scope revalidation below.
                read_retries += 1;
            }
            Err(error) => return Err(diagnostics::scan_error(query.scope, error.message())),
        }
        let waiting = Context {
            phase: "wait",
            ..context
        };
        waiting.read("Budget", budget.check())?;
        waiting.read(
            "Sleep",
            cancellation.sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            ),
        )?;
        let current = resolve(query.scope, budget)?;
        if current.ptr != scope_pointer {
            return Err(context.error(
                "ScopePointer",
                "stale_target: selected accessibility scope path changed".into(),
                false,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_selection_rejects_ambiguity_and_partial_observation() {
        assert_eq!(choose(0, None, true).unwrap(), None);
        assert_eq!(choose(1, None, true).unwrap(), Some(0));
        assert!(
            choose(2, None, true)
                .unwrap_err()
                .starts_with("ambiguous_target:")
        );
        assert_eq!(choose(2, Some(1), true).unwrap(), Some(1));
        assert_eq!(choose(2, Some(2), true).unwrap(), None);
        for index in [None, Some(0)] {
            assert!(choose(1, index, false).is_err());
        }
    }

    #[test]
    fn disabled_hidden_or_defunct_controls_remain_observable_but_never_ready() {
        let mut node = Node {
            object_index: 0,
            path: "0/1/0/2".into(),
            pid: 42,
            role: "button".into(),
            name: "Send".into(),
            text: String::new(),
            actions: Vec::new(),
            states: States {
                enabled: false,
                focusable: true,
                focused: false,
                showing: true,
                visible: true,
                defunct: false,
            },
        };
        assert!(node.visible());
        assert!(!node.ready(Operation::Action("press")));
        node.states.enabled = true;
        assert!(node.ready(Operation::Action("press")));
        assert!(node.ready(Operation::Focus));
        node.states.focusable = false;
        assert!(node.ready(Operation::Action("press")));
        assert!(!node.ready(Operation::Focus));
        node.states.showing = false;
        assert!(!node.ready(Operation::Action("press")));
        assert!(!node.ready(Operation::Focus));
        assert!(node.ready(Operation::Reveal));
        node.states.enabled = false;
        assert!(!node.ready(Operation::Reveal));
        node.states.enabled = true;
        node.states.visible = false;
        assert!(!node.ready(Operation::Reveal));
        node.states.visible = true;
        node.states.showing = true;
        node.states.defunct = true;
        assert!(!node.ready(Operation::Action("press")));
        assert!(!node.ready(Operation::Reveal));
    }
}
