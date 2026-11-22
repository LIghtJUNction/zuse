//! In-process AT-SPI ABI fixture. Every Object is an opaque fixture identity;
//! no library is loaded, no D-Bus connection is opened and no UI is controlled.
use super::*;
use std::cell::RefCell;

const DESKTOP: usize = 1;
const APP: usize = 2;
const SCOPE: usize = 3;
const TARGET: usize = 4;
const SIBLING: usize = 5;
const REPLACEMENT: usize = 6;
const PID: u32 = 42;

#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Rejected,
    Error,
    Cancel,
    Slow,
}

struct Fixture {
    target_parent: usize,
    target_name: String,
    actions: Vec<String>,
    showing: bool,
    enabled: bool,
    null_desktop: bool,
    null_child: Option<(usize, c_int)>,
    null_states: Option<usize>,
    null_parent: bool,
    null_ref: bool,
    null_component: bool,
    null_action_call: Option<usize>,
    action_iface_calls: usize,
    vanish_role: Option<usize>,
    role_calls: [usize; 7],
    replace_on_vanish: bool,
    replaced: bool,
    negative_count: bool,
    outcome: Outcome,
    input_cancel: Option<Cancellation>,
    inputs: [usize; 3],
    last_action: Option<c_int>,
    refs: isize,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            target_parent: SCOPE,
            target_name: "Target".into(),
            actions: vec!["press".into(), "menu".into()],
            showing: true,
            enabled: true,
            null_desktop: false,
            null_child: None,
            null_states: None,
            null_parent: false,
            null_ref: false,
            null_component: false,
            null_action_call: None,
            action_iface_calls: 0,
            vanish_role: None,
            role_calls: [0; 7],
            replace_on_vanish: false,
            replaced: false,
            negative_count: false,
            outcome: Outcome::Success,
            input_cancel: None,
            inputs: [0; 3],
            last_action: None,
            refs: 0,
        }
    }
}

thread_local! { static FIXTURE: RefCell<Fixture> = RefCell::new(Fixture::default()); }

fn edit<T>(f: impl FnOnce(&mut Fixture) -> T) -> T {
    FIXTURE.with(|fixture| f(&mut fixture.borrow_mut()))
}

fn reset() {
    edit(|fixture| {
        assert_eq!(fixture.refs, 0, "previous fixture leaked native references");
        *fixture = Fixture::default();
    });
}

fn owned(id: usize) -> Obj {
    if id != 0 {
        edit(|fixture| fixture.refs += 1);
    }
    // The fixture only compares these opaque identities; it never dereferences
    // them as Rust memory or passes them to any external library.
    id as Obj
}

fn string(value: &str) -> *mut c_char {
    CString::new(value).unwrap().into_raw()
}

unsafe fn error(out: *mut ErrPtr, message: &str) {
    if !out.is_null() {
        let value = Box::new(GError {
            domain: 1,
            code: 1,
            message: string(message),
        });
        unsafe { *out = Box::into_raw(value) };
    }
}

unsafe extern "C" fn timeout(_: c_int, _: c_int) {}
unsafe extern "C" fn desktop(_: c_int) -> Obj {
    owned(if edit(|fixture| fixture.null_desktop) {
        0
    } else {
        DESKTOP
    })
}
unsafe extern "C" fn name(object: Obj, _: *mut ErrPtr) -> *mut c_char {
    let value = edit(|fixture| match object as usize {
        TARGET => fixture.target_name.clone(),
        SCOPE => "Scope".into(),
        SIBLING => "Sibling".into(),
        REPLACEMENT => "Replacement".into(),
        _ => "Fixture".into(),
    });
    string(&value)
}
unsafe extern "C" fn role(object: Obj, out: *mut ErrPtr) -> *mut c_char {
    let id = object as usize;
    let vanished = edit(|fixture| {
        fixture.role_calls[id] += 1;
        if fixture.vanish_role == Some(id) {
            fixture.vanish_role = None;
            fixture.replaced = fixture.replace_on_vanish;
            true
        } else {
            false
        }
    });
    if vanished {
        unsafe {
            error(
                out,
                "Method \"GetRole\" with signature \"\" on interface \"org.a11y.atspi.Accessible\" could not be processed as object /org/a11y/atspi/accessible/5733 does not exist\n",
            )
        };
        return core::ptr::null_mut();
    }
    string(match id {
        TARGET => "button",
        SCOPE | SIBLING | REPLACEMENT => "frame",
        APP => "application",
        _ => "desktop frame",
    })
}
unsafe extern "C" fn count(object: Obj, _: *mut ErrPtr) -> c_int {
    edit(|fixture| match object as usize {
        DESKTOP => 1,
        APP => 2,
        SCOPE | REPLACEMENT => 1,
        TARGET if fixture.negative_count => -1,
        _ => 0,
    })
}
unsafe extern "C" fn child(object: Obj, index: c_int, _: *mut ErrPtr) -> Obj {
    let id = edit(|fixture| {
        if fixture.null_child == Some((object as usize, index)) {
            return 0;
        }
        match (object as usize, index) {
            (DESKTOP, 0) => APP,
            (APP, 0) => {
                if fixture.replaced {
                    REPLACEMENT
                } else {
                    SCOPE
                }
            }
            (APP, 1) => SIBLING,
            (SCOPE | REPLACEMENT, 0) => TARGET,
            _ => 0,
        }
    });
    owned(id)
}
unsafe extern "C" fn parent(object: Obj, _: *mut ErrPtr) -> Obj {
    let id = edit(|fixture| match object as usize {
        TARGET if fixture.null_parent => 0,
        TARGET => fixture.target_parent,
        SCOPE | SIBLING | REPLACEMENT => APP,
        APP => DESKTOP,
        _ => 0,
    });
    owned(id)
}
unsafe extern "C" fn no_interface(_: Obj) -> Obj {
    core::ptr::null_mut()
}
unsafe extern "C" fn text(_: Obj, _: c_int, _: c_int, _: *mut ErrPtr) -> *mut c_char {
    string("")
}
unsafe extern "C" fn action_interface(object: Obj) -> Obj {
    let id = edit(|fixture| {
        fixture.action_iface_calls += 1;
        if fixture.null_action_call == Some(fixture.action_iface_calls) {
            0
        } else {
            object as usize
        }
    });
    owned(id)
}
unsafe extern "C" fn action_count(_: Obj, _: *mut ErrPtr) -> c_int {
    edit(|fixture| fixture.actions.len() as c_int)
}
unsafe extern "C" fn action_name(_: Obj, index: c_int, _: *mut ErrPtr) -> *mut c_char {
    string(&edit(|fixture| fixture.actions[index as usize].clone()))
}
unsafe extern "C" fn component_interface(object: Obj) -> Obj {
    owned(if edit(|fixture| fixture.null_component) {
        0
    } else {
        object as usize
    })
}
unsafe extern "C" fn process_id(_: Obj, _: *mut ErrPtr) -> u32 {
    PID
}
unsafe extern "C" fn clear_cache(_: Obj) {}
unsafe extern "C" fn states(object: Obj) -> Obj {
    owned(
        if edit(|fixture| fixture.null_states == Some(object as usize)) {
            0
        } else {
            object as usize
        },
    )
}
unsafe extern "C" fn contains(object: Obj, state: c_int) -> c_int {
    edit(|fixture| match state {
        8 => object as usize != TARGET || fixture.enabled,
        11 | 30 => true,
        25 => object as usize != TARGET || fixture.showing,
        _ => false,
    }) as c_int
}
unsafe extern "C" fn free(pointer: Obj) {
    if !pointer.is_null() {
        drop(unsafe { CString::from_raw(pointer.cast()) });
    }
}
unsafe extern "C" fn reference(object: Obj) -> Obj {
    owned(if edit(|fixture| fixture.null_ref) {
        0
    } else {
        object as usize
    })
}
unsafe extern "C" fn unref(_: Obj) {
    edit(|fixture| fixture.refs -= 1);
}
unsafe extern "C" fn error_free(pointer: ErrPtr) {
    let value = unsafe { Box::from_raw(pointer) };
    unsafe { free(value.message.cast()) };
}

unsafe fn input(kind: usize, out: *mut ErrPtr) -> c_int {
    let (outcome, cancellation) = edit(|fixture| {
        fixture.inputs[kind] += 1;
        (fixture.outcome, fixture.input_cancel.clone())
    });
    match outcome {
        Outcome::Success => {
            if kind == 2 {
                edit(|fixture| fixture.showing = true);
            }
            1
        }
        Outcome::Rejected => 0,
        Outcome::Error => {
            unsafe { error(out, "fixture native reply failed") };
            0
        }
        Outcome::Cancel => {
            cancellation.unwrap().request_cancel();
            1
        }
        Outcome::Slow => {
            std::thread::sleep(Duration::from_millis(80));
            1
        }
    }
}
unsafe extern "C" fn action(_: Obj, index: c_int, out: *mut ErrPtr) -> c_int {
    edit(|fixture| fixture.last_action = Some(index));
    unsafe { input(0, out) }
}
unsafe extern "C" fn focus(_: Obj, out: *mut ErrPtr) -> c_int {
    unsafe { input(1, out) }
}
unsafe extern "C" fn scroll(_: Obj, _: c_int, out: *mut ErrPtr) -> c_int {
    unsafe { input(2, out) }
}
unsafe extern "C" fn at_point(_: Obj, _: c_int, _: c_int, _: c_int, _: *mut ErrPtr) -> Obj {
    core::ptr::null_mut()
}
unsafe extern "C" fn extents(_: Obj, _: c_int, _: *mut ErrPtr) -> *mut Rect {
    core::ptr::null_mut()
}
unsafe extern "C" fn selection(_: Obj, _: c_int, _: *mut ErrPtr) -> *mut TextRange {
    core::ptr::null_mut()
}
unsafe extern "C" fn zero_index(_: Obj, _: c_int, _: *mut ErrPtr) -> c_int {
    0
}
unsafe extern "C" fn insert(_: Obj, _: c_int, _: *const c_char, _: c_int, _: *mut ErrPtr) -> c_int {
    0
}
unsafe extern "C" fn delete(_: Obj, _: c_int, _: c_int, _: *mut ErrPtr) -> c_int {
    0
}

static API: Api = Api {
    set_timeout: timeout,
    desktop,
    name,
    role,
    count,
    child,
    parent,
    text_iface: no_interface,
    text_count: count,
    text,
    action_iface: action_interface,
    actions_count: action_count,
    action_name,
    do_action: action,
    component_iface: component_interface,
    at_point,
    extents,
    grab_focus: focus,
    scroll_to: Some(scroll),
    editable_iface: no_interface,
    process_id,
    clear_cache,
    state_set: states,
    state_contains: contains,
    caret: count,
    set_caret: zero_index,
    selections: count,
    selection,
    remove_selection: zero_index,
    insert,
    delete,
    free,
    ref_object: reference,
    unref,
    error_free,
};

fn query() -> Query<'static> {
    Query {
        scope: "0/0/0",
        expected_pid: PID,
        role: Some("button"),
        name: Some("Target"),
        text: None,
        exact: true,
    }
}

fn run(
    operation: Option<Operation<'_>>,
    waiting: bool,
    timeout_ms: u64,
    cancellation: &Cancellation,
) -> Result<String> {
    let start = Instant::now();
    let budget = Budget {
        api: &API,
        deadline: start + Duration::from_millis(timeout_ms),
        cancellation,
    };
    let result = run_scoped(&query(), operation, None, waiting, start, &budget);
    assert_eq!(
        edit(|fixture| fixture.refs),
        0,
        "native Object references must be released on every exit"
    );
    result
}

fn assert_no_input(error: &str, phase: &str) {
    assert!(error.contains(&format!("phase={phase}")), "{error}");
    assert!(error.contains("scope=0/0/0"), "{error}");
    assert!(error.contains("native_call_started=false"), "{error}");
    assert!(!error.contains("native_call_started=true"), "{error}");
    assert_eq!(edit(|fixture| fixture.inputs), [0; 3]);
}

#[test]
fn permission_reparent_to_same_pid_sibling_blocks_all_native_input() {
    for operation in [
        Operation::Action("press"),
        Operation::Focus,
        Operation::Reveal,
    ] {
        reset();
        edit(|fixture| fixture.showing = !matches!(operation, Operation::Reveal));
        let cancellation = Cancellation::with_dispatch_guard(|| {
            edit(|fixture| fixture.target_parent = SIBLING);
            Ok(Box::new(()))
        });
        let error = run(Some(operation), true, 1000, &cancellation).unwrap_err();
        assert_no_input(&error, "prepare");
        assert!(error.contains("GetParent"), "{error}");
    }
}

#[test]
fn permission_action_reordering_removal_or_duplicate_never_uses_the_old_index() {
    for actions in [
        vec!["menu", "press"],
        vec!["menu", "renamed"],
        vec!["press", "press"],
    ] {
        reset();
        let cancellation = Cancellation::with_dispatch_guard(move || {
            edit(|fixture| fixture.actions = actions.iter().map(|name| (*name).into()).collect());
            Ok(Box::new(()))
        });
        let error = run(Some(Operation::Action("press")), true, 1000, &cancellation).unwrap_err();
        assert_no_input(&error, "prepare");
        assert!(
            error.contains("ActionIndex") || error.contains("ActionName"),
            "{error}"
        );
        assert_eq!(edit(|fixture| fixture.last_action), None);
    }
}

#[test]
fn permission_identity_readiness_and_null_parent_fail_before_native_input() {
    for change in 0..4 {
        reset();
        let cancellation = Cancellation::with_dispatch_guard(move || {
            edit(|fixture| match change {
                0 => fixture.target_name = "Changed".into(),
                1 => fixture.enabled = false,
                2 => fixture.null_states = Some(TARGET),
                _ => fixture.null_parent = true,
            });
            Ok(Box::new(()))
        });
        let error = run(Some(Operation::Action("press")), true, 1000, &cancellation).unwrap_err();
        assert_no_input(&error, "prepare");
        assert!(error.contains("path=0/0/0/0"), "{error}");
    }
}

#[test]
fn native_false_and_error_replies_are_marked_once_and_never_retried() {
    for operation in [
        Operation::Action("press"),
        Operation::Focus,
        Operation::Reveal,
    ] {
        for outcome in [Outcome::Rejected, Outcome::Error] {
            reset();
            edit(|fixture| {
                fixture.outcome = outcome;
                fixture.showing = !matches!(operation, Operation::Reveal);
            });
            let error = run(Some(operation), true, 1000, &Cancellation::default()).unwrap_err();
            assert!(error.contains("phase=native_input"), "{error}");
            assert!(error.contains("native_call_started=true"), "{error}");
            assert!(!error.contains("native_call_started=false"), "{error}");
            assert!(error.contains("not retried"), "{error}");
            assert_eq!(edit(|fixture| fixture.inputs.iter().sum::<usize>()), 1);
            if matches!(outcome, Outcome::Error) {
                assert!(error.starts_with("fixture native reply failed"), "{error}");
            }
        }
    }
}

#[test]
fn native_reply_cancellation_or_deadline_is_marked_as_already_started() {
    for operation in [
        Operation::Action("press"),
        Operation::Focus,
        Operation::Reveal,
    ] {
        for outcome in [Outcome::Cancel, Outcome::Slow] {
            reset();
            let cancellation = Cancellation::default();
            edit(|fixture| {
                fixture.outcome = outcome;
                fixture.input_cancel = Some(cancellation.clone());
                fixture.showing = !matches!(operation, Operation::Reveal);
            });
            let timeout = if matches!(outcome, Outcome::Slow) {
                40
            } else {
                1000
            };
            let error = run(Some(operation), true, timeout, &cancellation).unwrap_err();
            let prefix = if matches!(outcome, Outcome::Slow) {
                "timeout:"
            } else {
                "cancelled:"
            };
            assert!(error.starts_with(prefix), "{error}");
            assert!(error.contains("native_call_started=true"), "{error}");
            assert!(!error.contains("native_call_started=false"), "{error}");
            assert!(error.contains("not retried"), "{error}");
            assert_eq!(edit(|fixture| fixture.inputs.iter().sum::<usize>()), 1);
        }
    }
}

#[test]
fn null_native_objects_report_the_exact_read_stage() {
    for failure in 0..8 {
        reset();
        edit(|fixture| match failure {
            0 => fixture.null_desktop = true,
            1 => fixture.null_child = Some((DESKTOP, 0)),
            2 => fixture.null_states = Some(SCOPE),
            3 => fixture.null_ref = true,
            4 => fixture.null_child = Some((SCOPE, 0)),
            5 => fixture.null_states = Some(TARGET),
            6 => fixture.null_action_call = Some(3),
            _ => fixture.null_component = true,
        });
        let operation = if failure == 7 {
            Operation::Focus
        } else {
            Operation::Action("press")
        };
        let error = run(Some(operation), true, 1000, &Cancellation::default()).unwrap_err();
        assert!(
            error.starts_with("accessibility node/interface is unavailable"),
            "{error}"
        );
        let (phase, read) = match failure {
            0 => ("resolve", "Desktop"),
            1 => ("resolve", "ChildAtIndex(0)"),
            2 => ("scope", "StateSet"),
            3 => ("scope", "RefScope"),
            4 => ("scan", "ChildAtIndex"),
            5 => ("scan", "States"),
            6 => ("pre_dispatch", "ActionIface"),
            _ => ("pre_dispatch", "ComponentIface"),
        };
        assert_no_input(&error, phase);
        assert!(error.contains(&format!("operation={read}")), "{error}");
    }
}

#[test]
fn only_read_only_wait_retries_exact_descendant_loss_and_keeps_scope_identity() {
    reset();
    edit(|fixture| fixture.vanish_role = Some(TARGET));
    let result = run(None, true, 1000, &Cancellation::default()).unwrap();
    assert!(result.contains("\"read_retries\":1"), "{result}");
    assert_eq!(edit(|fixture| fixture.inputs), [0; 3]);

    for (operation, waiting) in [(None, false), (Some(Operation::Action("press")), true)] {
        reset();
        edit(|fixture| fixture.vanish_role = Some(TARGET));
        let error = run(operation, waiting, 1000, &Cancellation::default()).unwrap_err();
        assert_no_input(&error, "scan");
        assert_eq!(edit(|fixture| fixture.role_calls[TARGET]), 1);
    }
    reset();
    edit(|fixture| fixture.vanish_role = Some(SCOPE));
    let error = run(None, true, 1000, &Cancellation::default()).unwrap_err();
    assert_no_input(&error, "scan");
    assert_eq!(edit(|fixture| fixture.role_calls[SCOPE]), 1);

    reset();
    edit(|fixture| {
        fixture.vanish_role = Some(TARGET);
        fixture.replace_on_vanish = true;
    });
    let error = run(None, true, 1000, &Cancellation::default()).unwrap_err();
    assert!(error.starts_with("stale_target:"), "{error}");
    assert_no_input(&error, "scope");
    assert!(error.contains("operation=ScopePointer"), "{error}");

    for null_state in [false, true] {
        reset();
        edit(|fixture| {
            if null_state {
                fixture.null_states = Some(TARGET);
            } else {
                fixture.negative_count = true;
            }
        });
        let error = run(None, true, 1000, &Cancellation::default()).unwrap_err();
        assert_no_input(&error, "scan");
        assert_eq!(edit(|fixture| fixture.role_calls[TARGET]), 1);
    }
}

#[test]
fn unchanged_permission_preparation_submits_one_retained_native_object() {
    for operation in [
        Operation::Action("press"),
        Operation::Focus,
        Operation::Reveal,
    ] {
        reset();
        edit(|fixture| fixture.showing = !matches!(operation, Operation::Reveal));
        let receipt = run(Some(operation), true, 1000, &Cancellation::default()).unwrap();
        assert!(
            receipt.contains("\"native_call_started\":true"),
            "{receipt}"
        );
        assert_eq!(edit(|fixture| fixture.inputs.iter().sum::<usize>()), 1);
        if matches!(operation, Operation::Action(_)) {
            assert_eq!(edit(|fixture| fixture.last_action), Some(0));
        }
    }
}
