//! Bounded native Linux AT-SPI observation and background actions.
//! Paths are child-index paths from desktop 0. Re-observe after UI changes.
#[cfg(target_os = "linux")]
use crate::browser::quote;
type Result<T> = std::result::Result<T, String>;
pub mod controls;

/// Maximum child depth below desktop 0 for native accessibility paths.
/// Modern Electron editors can sit beyond 32 ancestors in their real UI tree.
pub(crate) const MAX_DEPTH: usize = 64;

fn validate_snapshot_limits(max_nodes: usize, max_depth: usize) -> Result<()> {
    if !(1..=4096).contains(&max_nodes) || max_depth > MAX_DEPTH {
        return Err(format!(
            "accessibility limits: nodes 1..4096, depth 0..{MAX_DEPTH}"
        ));
    }
    Ok(())
}

/// Read roles, names, text, screen bounds and supported actions.
/// Password text is redacted. Partial observations carry truncated/errors.
pub fn snapshot(max_nodes: usize, max_depth: usize) -> Result<String> {
    validate_snapshot_limits(max_nodes, max_depth)?;
    native::snapshot(max_nodes, max_depth)
}
/// Discover one process's directly exported window/popup scopes. Other
/// applications are inspected only for process identity, never traversed.
/// Returned child-index paths are observations, not persistent identifiers.
/// The adapter checks its deadline between native calls. libatspi's first PID
/// lookup uses the D-Bus default timeout, rather than atspi_set_timeout.
pub fn windows(
    expected_pid: u32,
    cancellation: &crate::observation::Cancellation,
) -> Result<String> {
    if expected_pid == 0 {
        return Err("accessibility.windows expected_pid must be a positive u32".into());
    }
    cancellation.check()?;
    #[cfg(target_os = "linux")]
    {
        native::controls::enumerate_windows(expected_pid, cancellation)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("unsupported_state: accessibility.windows requires Linux AT-SPI".into())
    }
}
/// Validate a desktop observation scope without opening the accessibility bus.
pub fn validate_scope(path: &str) -> Result<()> {
    validate_path(path)
}

/// Read only one already selected accessibility subtree.
pub fn snapshot_scope(path: &str, max_nodes: usize, max_depth: usize) -> Result<String> {
    validate_path(path)?;
    validate_snapshot_limits(max_nodes, max_depth)?;
    #[cfg(target_os = "linux")]
    {
        native::snapshot_scope(path, max_nodes, max_depth)
    }
    #[cfg(not(target_os = "linux"))]
    {
        if path != "0" {
            return Err("unsupported: scoped accessibility observation is currently available on Linux AT-SPI".into());
        }
        snapshot(max_nodes, max_depth)
    }
}

/// Re-read this node only, including its current bounds and process identity.
pub fn node(path: &str) -> Result<String> {
    validate_path(path)?;
    #[cfg(target_os = "linux")]
    {
        native::node(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("unsupported: single-node accessibility observation is currently available on Linux AT-SPI".into())
    }
}

/// Check the selected node's branch at a screen point using its window and
/// ancestors. This does not inspect other applications or establish window
/// stacking; callers must separately verify the selected native window.
pub fn hit_test(path: &str, x: i32, y: i32) -> Result<bool> {
    validate_path(path)?;
    #[cfg(target_os = "linux")]
    {
        native::hit_test(path, x, y)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, x, y);
        Err("unsupported: lightweight accessibility hit testing is available only through Linux AT-SPI".into())
    }
}

/// An event subscription bound to one native accessibility object. A changed
/// child index never silently retargets this watch to another application.
pub struct Watch {
    #[cfg(target_os = "linux")]
    inner: native::EventWatch,
}
impl Watch {
    pub fn new(scope: &str) -> Result<Self> {
        validate_path(scope)?;
        if scope.split('/').count() < 3 {
            return Err("accessibility watch requires an explicit window or subtree scope".into());
        }
        #[cfg(target_os = "linux")]
        {
            Ok(Self {
                inner: native::EventWatch::new(scope)?,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("unsupported: native accessibility event watches are currently available on Linux AT-SPI".into())
        }
    }
    pub fn snapshot(&mut self) -> Result<String> {
        #[cfg(target_os = "linux")]
        {
            self.inner.snapshot()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("unsupported: native accessibility event watches are unavailable".into())
        }
    }
    pub fn wait_event(
        &mut self,
        deadline: std::time::Instant,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<bool> {
        #[cfg(target_os = "linux")]
        {
            self.inner.wait_event(deadline, cancellation)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (deadline, cancellation);
            Err("unsupported: native accessibility event watches are unavailable".into())
        }
    }
}

/// Read an accessibility snapshot as plain text.
pub fn text(max_nodes: usize, max_depth: usize) -> Result<String> {
    let snapshot = snapshot(max_nodes, max_depth)?;
    crate::browser::with_json(&snapshot, |root| {
        let mut output = String::new();
        if let Some(nodes) = root.get("nodes") {
            for node in nodes.elements() {
                let field = |name| {
                    node.get(name)
                        .and_then(|v| v.string())
                        .map(|v| v.chars().collect::<String>())
                        .unwrap_or_default()
                };
                let name = field("name");
                let value = field("text");
                output.push_str(&format!("{} [{}] {}", field("path"), field("role"), name));
                if !value.is_empty() && value != name {
                    output.push_str(": ");
                    output.push_str(&value);
                }
                output.push('\n');
            }
        }
        if root.get("truncated").and_then(|v| v.boolean()) == Some(true) {
            output.push_str("[observation truncated]\n");
        }
        Ok(output)
    })
}
/// Invoke an action advertised in a fresh snapshot.
pub fn action(path: &str, index: usize) -> Result<String> {
    action_checked(path, index, None, None, None)
}

/// Verify the selected native object's identity before invoking its action.
/// The same owned object reference is used for checks and dispatch; the child
/// path is not resolved again. These application calls are not an atomic UI
/// transaction. Omitted checks preserve the existing action behavior.
pub fn action_checked(
    path: &str,
    index: usize,
    expected_pid: Option<u32>,
    expected_role: Option<&str>,
    expected_name: Option<&str>,
) -> Result<String> {
    action_checked_with_cancel(
        path,
        index,
        expected_pid,
        expected_role,
        expected_name,
        &crate::observation::Cancellation::default(),
    )
}

pub(crate) fn action_checked_with_cancel(
    path: &str,
    index: usize,
    expected_pid: Option<u32>,
    expected_role: Option<&str>,
    expected_name: Option<&str>,
    cancellation: &crate::observation::Cancellation,
) -> Result<String> {
    validate_identity_target(path, expected_pid, expected_role, expected_name)?;
    if index > 63 {
        return Err("accessibility action index exceeds 63".into());
    }
    let checks = IdentityChecks {
        pid: expected_pid,
        role: expected_role,
        name: expected_name,
    };
    #[cfg(target_os = "linux")]
    {
        native::action(path, index, checks, cancellation)
    }
    #[cfg(not(target_os = "linux"))]
    {
        if !checks.is_empty() {
            return Err(
                "unsupported_state: guarded accessibility actions currently require Linux AT-SPI"
                    .into(),
            );
        }
        cancellation.dispatch(|| native::action(path, index))
    }
}
/// Request accessibility focus without moving the pointer.
pub fn focus(path: &str) -> Result<String> {
    focus_checked(path, None, None, None)
}

/// Verify and focus the same native object, without a second path lookup.
pub fn focus_checked(
    path: &str,
    expected_pid: Option<u32>,
    expected_role: Option<&str>,
    expected_name: Option<&str>,
) -> Result<String> {
    focus_checked_with_cancel(
        path,
        expected_pid,
        expected_role,
        expected_name,
        &crate::observation::Cancellation::default(),
    )
}

pub(crate) fn focus_checked_with_cancel(
    path: &str,
    expected_pid: Option<u32>,
    expected_role: Option<&str>,
    expected_name: Option<&str>,
    cancellation: &crate::observation::Cancellation,
) -> Result<String> {
    validate_identity_target(path, expected_pid, expected_role, expected_name)?;
    let checks = IdentityChecks {
        pid: expected_pid,
        role: expected_role,
        name: expected_name,
    };
    #[cfg(target_os = "linux")]
    {
        native::focus(path, checks, cancellation)
    }
    #[cfg(not(target_os = "linux"))]
    {
        if !checks.is_empty() {
            return Err(
                "unsupported_state: guarded accessibility focus currently requires Linux AT-SPI"
                    .into(),
            );
        }
        cancellation.dispatch(|| native::focus(path))
    }
}

#[derive(Clone, Copy)]
struct IdentityChecks<'a> {
    pid: Option<u32>,
    role: Option<&'a str>,
    name: Option<&'a str>,
}
impl IdentityChecks<'_> {
    fn is_empty(self) -> bool {
        self.pid.is_none() && self.role.is_none() && self.name.is_none()
    }
}

pub(crate) fn validate_identity_target(
    path: &str,
    expected_pid: Option<u32>,
    expected_role: Option<&str>,
    expected_name: Option<&str>,
) -> Result<()> {
    validate_path(path)?;
    if expected_pid == Some(0) {
        return Err("expected_pid must be a positive native process ID".into());
    }
    if expected_role
        .is_some_and(|value| value.is_empty() || value.len() > 4096 || value.contains('\0'))
    {
        return Err("expected_role must contain 1..4096 NUL-free bytes".into());
    }
    if expected_name.is_some_and(|value| value.len() > 65536 || value.contains('\0')) {
        return Err("expected_name must be NUL-free and at most 64 KiB".into());
    }
    Ok(())
}
/// Replace an editable node's contents. Password nodes require human assistance.
pub fn set_text(path: &str, text: &str) -> Result<String> {
    validate_text_edit(path, text, None, None)?;
    native::set_text(path, text)
}

/// Conditionally replace and read back the same native editable object.
/// An empty expected_text requires an exactly empty editor. Checks and edits
/// are separate native application calls, not an atomic compare-and-set.
/// Omitted checks preserve the existing unguarded set_text behavior.
pub fn set_text_checked(
    path: &str,
    text: &str,
    expected_text: Option<&str>,
    expected_pid: Option<u32>,
) -> Result<String> {
    set_text_checked_with_cancel(
        path,
        text,
        expected_text,
        expected_pid,
        &crate::observation::Cancellation::default(),
    )
}

pub(crate) fn set_text_checked_with_cancel(
    path: &str,
    text: &str,
    expected_text: Option<&str>,
    expected_pid: Option<u32>,
    cancellation: &crate::observation::Cancellation,
) -> Result<String> {
    validate_text_edit(path, text, expected_text, expected_pid)?;
    native::set_text_checked(path, text, expected_text, expected_pid, cancellation)
}

pub(crate) fn validate_text_edit(
    path: &str,
    text: &str,
    expected_text: Option<&str>,
    expected_pid: Option<u32>,
) -> Result<()> {
    validate_path(path)?;
    for (name, value) in [("text", Some(text)), ("expected_text", expected_text)] {
        if value.is_some_and(|value| value.len() > 65536 || value.contains('\0')) {
            return Err(format!("{name} must be NUL-free and at most 64 KiB"));
        }
    }
    if expected_pid == Some(0) {
        return Err("expected_pid must be a positive native process ID".into());
    }
    Ok(())
}
fn validate_path(path: &str) -> Result<()> {
    if path.len() > 1024
        || path.split('/').count() > MAX_DEPTH + 1
        || path.split('/').next() != Some("0")
        || path.split('/').any(|s| {
            s.is_empty()
                || !s.bytes().all(|b| b.is_ascii_digit())
                || s.parse::<u32>().map_or(true, |i| i > 1000000)
        })
    {
        return Err("invalid accessibility path; expected 0/child/child".into());
    }
    Ok(())
}

/// Internal X11 fallback. The caller supplies a locally owned window's process
/// and geometry and rechecks X focus immediately before any text mutation.
#[cfg(target_os = "linux")]
pub(crate) fn insert_focused_x11(
    pid: u32,
    bounds: (i32, i32, u32, u32),
    frame_bounds: Option<(i32, i32, u32, u32)>,
    text: &str,
    still_focused: impl Fn() -> Result<()>,
) -> Result<()> {
    native::x11_input::insert(pid, bounds, frame_bounds, text, still_focused)
}

#[cfg(target_os = "linux")]
mod native {
    pub(super) mod controls;
    mod events;
    mod text_edit;
    pub(super) mod x11_input;
    use super::*;
    use crate::platform::dylib::Library;
    use core::ffi::{c_char, c_int, c_void};
    pub(super) use events::EventWatch;
    use std::{
        ffi::{CStr, CString},
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    type Obj = *mut c_void;
    type ErrPtr = *mut GError;
    #[repr(C)]
    struct GError {
        domain: u32,
        code: c_int,
        message: *mut c_char,
    }
    #[repr(C)]
    struct Rect {
        x: c_int,
        y: c_int,
        width: c_int,
        height: c_int,
    }
    #[repr(C)]
    struct TextRange {
        start: c_int,
        end: c_int,
    }
    // AT-SPI owns process-global D-Bus state; its library stays loaded.
    // Every call from this module is serialized by ACCESS.
    static LIBRARY: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    static ACCESS: Mutex<()> = Mutex::new(());
    struct Api {
        set_timeout: unsafe extern "C" fn(c_int, c_int),
        desktop: unsafe extern "C" fn(c_int) -> Obj,
        name: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
        role: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
        count: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        child: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> Obj,
        parent: unsafe extern "C" fn(Obj, *mut ErrPtr) -> Obj,
        text_iface: unsafe extern "C" fn(Obj) -> Obj,
        text_count: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        text: unsafe extern "C" fn(Obj, c_int, c_int, *mut ErrPtr) -> *mut c_char,
        action_iface: unsafe extern "C" fn(Obj) -> Obj,
        actions_count: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        action_name: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> *mut c_char,
        do_action: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> c_int,
        component_iface: unsafe extern "C" fn(Obj) -> Obj,
        at_point: unsafe extern "C" fn(Obj, c_int, c_int, c_int, *mut ErrPtr) -> Obj,
        extents: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> *mut Rect,
        grab_focus: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        scroll_to: Option<unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> c_int>,
        editable_iface: unsafe extern "C" fn(Obj) -> Obj,
        process_id: unsafe extern "C" fn(Obj, *mut ErrPtr) -> u32,
        clear_cache: unsafe extern "C" fn(Obj),
        state_set: unsafe extern "C" fn(Obj) -> Obj,
        state_contains: unsafe extern "C" fn(Obj, c_int) -> c_int,
        caret: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        set_caret: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> c_int,
        selections: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        selection: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> *mut TextRange,
        remove_selection: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> c_int,
        insert: unsafe extern "C" fn(Obj, c_int, *const c_char, c_int, *mut ErrPtr) -> c_int,
        delete: unsafe extern "C" fn(Obj, c_int, c_int, *mut ErrPtr) -> c_int,
        free: unsafe extern "C" fn(Obj),
        ref_object: unsafe extern "C" fn(Obj) -> Obj,
        unref: unsafe extern "C" fn(Obj),
        error_free: unsafe extern "C" fn(ErrPtr),
    }
    fn load() -> Result<&'static Api> {
        LIBRARY
            .get_or_init(Api::open)
            .as_ref()
            .map_err(Clone::clone)
    }
    fn load_bounded(startup_timeout_ms: c_int) -> Result<&'static Api> {
        LIBRARY
            .get_or_init(|| Api::open_with_timeout(startup_timeout_ms))
            .as_ref()
            .map_err(Clone::clone)
    }
    impl Api {
        fn open() -> Result<Self> {
            Self::open_with_timeout(1000)
        }
        fn open_with_timeout(startup_timeout_ms: c_int) -> Result<Self> {
            let library = Library::open(c"libatspi.so.0").map_err(|error| {
                format!("AT-SPI unavailable; install at-spi2-core and enable the session accessibility bus: {error}")
            })?;
            // SAFETY: signatures follow the libatspi public C headers. Resolve
            // every export before initializing its process-global D-Bus state.
            unsafe {
                let init: unsafe extern "C" fn() -> c_int = library.symbol(c"atspi_init")?;
                let set_timeout: unsafe extern "C" fn(c_int, c_int) =
                    library.symbol(c"atspi_set_timeout")?;
                let api = Self {
                    set_timeout,
                    desktop: library.symbol(c"atspi_get_desktop")?,
                    name: library.symbol(c"atspi_accessible_get_name")?,
                    role: library.symbol(c"atspi_accessible_get_role_name")?,
                    count: library.symbol(c"atspi_accessible_get_child_count")?,
                    child: library.symbol(c"atspi_accessible_get_child_at_index")?,
                    parent: library.symbol(c"atspi_accessible_get_parent")?,
                    text_iface: library.symbol(c"atspi_accessible_get_text_iface")?,
                    text_count: library.symbol(c"atspi_text_get_character_count")?,
                    text: library.symbol(c"atspi_text_get_text")?,
                    action_iface: library.symbol(c"atspi_accessible_get_action_iface")?,
                    actions_count: library.symbol(c"atspi_action_get_n_actions")?,
                    action_name: library.symbol(c"atspi_action_get_action_name")?,
                    do_action: library.symbol(c"atspi_action_do_action")?,
                    component_iface: library.symbol(c"atspi_accessible_get_component_iface")?,
                    at_point: library.symbol(c"atspi_component_get_accessible_at_point")?,
                    extents: library.symbol(c"atspi_component_get_extents")?,
                    grab_focus: library.symbol(c"atspi_component_grab_focus")?,
                    // Older AT-SPI libraries can still enumerate and activate
                    // controls; reveal reports its own missing capability.
                    scroll_to: library.symbol(c"atspi_component_scroll_to").ok(),
                    editable_iface: library.symbol(c"atspi_accessible_get_editable_text_iface")?,
                    process_id: library.symbol(c"atspi_accessible_get_process_id")?,
                    clear_cache: library
                        .symbol(c"atspi_accessible_clear_cache_single")
                        .or_else(|_| library.symbol(c"atspi_accessible_clear_cache"))?,
                    state_set: library.symbol(c"atspi_accessible_get_state_set")?,
                    state_contains: library.symbol(c"atspi_state_set_contains")?,
                    caret: library.symbol(c"atspi_text_get_caret_offset")?,
                    set_caret: library.symbol(c"atspi_text_set_caret_offset")?,
                    selections: library.symbol(c"atspi_text_get_n_selections")?,
                    selection: library.symbol(c"atspi_text_get_selection")?,
                    remove_selection: library.symbol(c"atspi_text_remove_selection")?,
                    insert: library.symbol(c"atspi_editable_text_insert_text")?,
                    delete: library.symbol(c"atspi_editable_text_delete_text")?,
                    free: library.symbol(c"g_free")?,
                    ref_object: library.symbol(c"g_object_ref")?,
                    unref: library.symbol(c"g_object_unref")?,
                    error_free: library.symbol(c"g_error_free")?,
                };
                // atspi_init can create process-global state even on failure.
                // Pin before invoking it so callbacks never outlive the image.
                library.pin();
                set_timeout(
                    startup_timeout_ms.clamp(1, 500),
                    startup_timeout_ms.clamp(1, 1000),
                );
                let status = init();
                set_timeout(500, 1000);
                if status != 0 && status != 1 {
                    return Err(format!("AT-SPI initialization failed ({status})"));
                }
                Ok(api)
            }
        }
        fn error(&self, error: ErrPtr) -> Result<()> {
            if error.is_null() {
                return Ok(());
            }
            // SAFETY: GLib returned an owned GError, freed exactly once after reading.
            unsafe {
                let message = if (*error).message.is_null() {
                    "AT-SPI operation failed".into()
                } else {
                    CStr::from_ptr((*error).message)
                        .to_string_lossy()
                        .chars()
                        .take(4096)
                        .collect()
                };
                (self.error_free)(error);
                Err(message)
            }
        }
        fn string(&self, pointer: *mut c_char, error: ErrPtr) -> Result<String> {
            // SAFETY: libatspi getters return owned NUL-terminated gchar strings.
            let value = if pointer.is_null() {
                String::new()
            } else {
                unsafe {
                    let value = CStr::from_ptr(pointer)
                        .to_string_lossy()
                        .chars()
                        .take(4096)
                        .collect();
                    (self.free)(pointer.cast());
                    value
                }
            };
            self.error(error)?;
            Ok(value)
        }
        fn get_string(
            &self,
            object: Obj,
            getter: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
        ) -> Result<String> {
            let mut error = core::ptr::null_mut();
            // SAFETY: a live Object is held and access is serialized.
            let string = unsafe { getter(object, &mut error) };
            self.string(string, error)
        }
        fn identity_string(
            &self,
            object: Obj,
            getter: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
            max_bytes: usize,
        ) -> Result<String> {
            let mut error = core::ptr::null_mut();
            // Identity checks must not compare the clipped snapshot preview.
            // SAFETY: the caller holds the native object; getters return an
            // owned C string which is freed even when decoding fails.
            let pointer = unsafe { getter(object, &mut error) };
            let value = if pointer.is_null() {
                Err("accessibility identity is unavailable".to_owned())
            } else {
                unsafe {
                    let value = CStr::from_ptr(pointer).to_bytes();
                    let result = if value.len() > max_bytes {
                        Err("accessibility identity exceeds its check bound".to_owned())
                    } else {
                        std::str::from_utf8(value)
                            .map(str::to_owned)
                            .map_err(|_| "accessibility identity is not UTF-8".to_owned())
                    };
                    (self.free)(pointer.cast());
                    result
                }
            };
            self.error(error)?;
            value
        }
        fn count(&self, object: Obj) -> Result<usize> {
            let mut error = core::ptr::null_mut();
            // SAFETY: object is live and error output is initialized.
            let value = unsafe { (self.count)(object, &mut error) };
            self.error(error)?;
            if value < 0 {
                return Err("AT-SPI returned a negative child count".into());
            }
            Ok(value as usize)
        }
        fn count_at(&self, object: Obj, path: &str, phase: &str) -> Result<usize> {
            self.count(object).map_err(|error| {
                format!("accessibility query failed: phase={phase} path={path} operation=ChildCount: {error}")
            })
        }
        fn object(&self, ptr: Obj) -> Result<Object<'_>> {
            if ptr.is_null() {
                Err("accessibility node/interface is unavailable".into())
            } else {
                Ok(Object { ptr, api: self })
            }
        }
        fn child(&self, object: Obj, index: usize) -> Result<Object<'_>> {
            let mut error = core::ptr::null_mut();
            // SAFETY: bounded child index, live object, initialized error output.
            let ptr = unsafe { (self.child)(object, index as c_int, &mut error) };
            if let Err(error) = self.error(error) {
                if !ptr.is_null() {
                    unsafe { (self.unref)(ptr) };
                }
                return Err(error);
            }
            self.object(ptr)
        }
        fn resolve(&self, path: &str) -> Result<Object<'_>> {
            // SAFETY: desktop 0 is supported and returns an owned reference.
            let mut object = self.object(unsafe { (self.desktop)(0) })?;
            let mut current_path = "0".to_owned();
            for index in path.split('/').skip(1) {
                // Match semantic resolution: refresh properties of this exact
                // ancestor before using its child list. No action is replayed.
                unsafe { (self.clear_cache)(object.ptr) };
                let index = index.parse::<usize>().map_err(|_| "invalid child index")?;
                if index >= self.count_at(object.ptr, &current_path, "resolve")? {
                    return Err(format!(
                        "accessibility path is stale: phase=resolve path={current_path}/{index}; take a fresh snapshot"
                    ));
                }
                object = self.child(object.ptr, index)?;
                current_path.push_str(&format!("/{index}"));
            }
            Ok(object)
        }
    }
    struct Object<'a> {
        ptr: Obj,
        api: &'a Api,
    }
    impl Drop for Object<'_> {
        fn drop(&mut self) {
            // SAFETY: this wrapper owns exactly one GLib reference.
            unsafe { (self.api.unref)(self.ptr) };
        }
    }
    struct Scan<'a> {
        api: &'a Api,
        nodes: Vec<String>,
        errors: usize,
        diagnostics: Vec<String>,
        truncated: bool,
        deadline: Instant,
        max_nodes: usize,
        max_depth: usize,
        bytes: usize,
        exhausted: bool,
    }
    impl Scan<'_> {
        fn visit(
            &mut self,
            object: Object<'_>,
            path: String,
            depth: usize,
            inherited_pid: Option<u32>,
        ) -> Result<()> {
            if self.exhausted
                || path.len() > 1024
                || self.nodes.len() >= self.max_nodes
                || Instant::now() >= self.deadline
            {
                self.truncated = true;
                return Ok(());
            }
            // Refresh each descendant, not only the selected scope root.
            // SAFETY: this visit owns the node reference and holds ACCESS.
            unsafe { (self.api.clear_cache)(object.ptr) };
            let pid = inherited_pid.or_else(|| {
                if path == "0" {
                    return None;
                }
                let mut error = core::ptr::null_mut();
                let value = unsafe { (self.api.process_id)(object.ptr, &mut error) };
                if self.api.error(error).is_ok() && value > 0 {
                    Some(value)
                } else {
                    self.errors += 1;
                    None
                }
            });
            let pid_json = pid.map_or_else(|| "null".to_owned(), |value| value.to_string());
            let name = self
                .api
                .get_string(object.ptr, self.api.name)
                .unwrap_or_else(|_| {
                    self.errors += 1;
                    String::new()
                });
            let role = self
                .api
                .get_string(object.ptr, self.api.role)
                .unwrap_or_else(|_| {
                    self.errors += 1;
                    String::new()
                });
            let count = self
                .api
                .count_at(object.ptr, &path, "scan")
                .unwrap_or_else(|error| {
                    self.errors += 1;
                    // Preserve partial tree observations, but retain bounded
                    // context instead of hiding which descendant failed.
                    let detail = quote(&error.chars().take(1024).collect::<String>());
                    if self.diagnostics.len() < 32 && self.bytes + detail.len() <= 2 << 20 {
                        self.bytes += detail.len();
                        self.diagnostics.push(detail);
                    }
                    0
                });
            let (visible, showing, enabled, focusable, focused) =
                match self.api.object(unsafe { (self.api.state_set)(object.ptr) }) {
                    Ok(states) => {
                        // AtspiStateType: ENABLED == 8, FOCUSABLE == 11,
                        // FOCUSED == 12, SHOWING == 25, VISIBLE == 30.
                        let visible = unsafe { (self.api.state_contains)(states.ptr, 30) != 0 };
                        let showing = unsafe { (self.api.state_contains)(states.ptr, 25) != 0 };
                        let enabled = unsafe { (self.api.state_contains)(states.ptr, 8) != 0 };
                        let focusable = unsafe { (self.api.state_contains)(states.ptr, 11) != 0 };
                        let focused = unsafe { (self.api.state_contains)(states.ptr, 12) != 0 };
                        (
                            visible.to_string(),
                            showing.to_string(),
                            enabled.to_string(),
                            focusable.to_string(),
                            focused.to_string(),
                        )
                    }
                    Err(_) => (
                        "null".into(),
                        "null".into(),
                        "null".into(),
                        "null".into(),
                        "null".into(),
                    ),
                };
            let password = role.to_ascii_lowercase().contains("password");
            let mut text = if password {
                "[redacted]".into()
            } else {
                String::new()
            };
            // All calls below use live owned references and serialized access.
            if !password
                && let Ok(interface) = self
                    .api
                    .object(unsafe { (self.api.text_iface)(object.ptr) })
            {
                let mut error = core::ptr::null_mut();
                let length = unsafe { (self.api.text_count)(interface.ptr, &mut error) };
                if self.api.error(error).is_ok() && length > 0 {
                    let mut error = core::ptr::null_mut();
                    let pointer =
                        unsafe { (self.api.text)(interface.ptr, 0, length.min(4096), &mut error) };
                    match self.api.string(pointer, error) {
                        Ok(value) => text = value,
                        Err(_) => self.errors += 1,
                    }
                }
            }
            let mut bounds = "null".to_owned();
            if let Ok(interface) = self
                .api
                .object(unsafe { (self.api.component_iface)(object.ptr) })
            {
                let mut error = core::ptr::null_mut();
                // ATSPI_COORD_TYPE_SCREEN == 0.
                let rect = unsafe { (self.api.extents)(interface.ptr, 0, &mut error) };
                let valid = self.api.error(error).is_ok();
                if !rect.is_null() {
                    // SAFETY: owned rectangle stays valid until g_free.
                    unsafe {
                        if valid {
                            bounds = format!(
                                r#"{{"x":{},"y":{},"width":{},"height":{}}}"#,
                                (*rect).x,
                                (*rect).y,
                                (*rect).width,
                                (*rect).height
                            );
                        }
                        (self.api.free)(rect.cast());
                    }
                }
            }
            let mut actions = Vec::new();
            if let Ok(interface) = self
                .api
                .object(unsafe { (self.api.action_iface)(object.ptr) })
            {
                let mut error = core::ptr::null_mut();
                let count = unsafe { (self.api.actions_count)(interface.ptr, &mut error) };
                if self.api.error(error).is_ok() {
                    for index in 0..count.clamp(0, 64) {
                        if Instant::now() >= self.deadline {
                            self.truncated = true;
                            break;
                        }
                        let mut error = core::ptr::null_mut();
                        let string =
                            unsafe { (self.api.action_name)(interface.ptr, index, &mut error) };
                        if let Ok(name) = self.api.string(string, error) {
                            actions.push(format!(
                                r#"{{"index":{index},"name":{}}}"#,
                                quote(&name.chars().take(256).collect::<String>())
                            ));
                        }
                    }
                }
            }
            let node = format!(
                r#"{{"path":{},"pid":{pid_json},"role":{},"name":{},"text":{},"children":{count},"bounds":{bounds},"visible":{visible},"showing":{showing},"enabled":{enabled},"focusable":{focusable},"focused":{focused},"actions":[{}]}}"#,
                quote(&path),
                quote(&role),
                quote(&name),
                quote(&text),
                actions.join(",")
            );
            if self.bytes + node.len() > 2 << 20 {
                self.exhausted = true;
                self.truncated = true;
                return Ok(());
            }
            self.bytes += node.len();
            self.nodes.push(node);
            if depth >= self.max_depth {
                if count > 0 {
                    self.truncated = true;
                }
                return Ok(());
            }
            for index in 0..count.min(1000000) {
                if self.exhausted
                    || self.nodes.len() >= self.max_nodes
                    || Instant::now() >= self.deadline
                {
                    self.truncated = true;
                    break;
                }
                match self.api.child(object.ptr, index) {
                    Ok(child) => self.visit(child, format!("{path}/{index}"), depth + 1, pid)?,
                    Err(_) => self.errors += 1,
                }
            }
            Ok(())
        }
    }
    pub(super) fn snapshot(max_nodes: usize, max_depth: usize) -> Result<String> {
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let root = api.resolve("0")?;
        scan_root(api, root, "0", max_nodes, max_depth)
    }

    fn scan_root(
        api: &Api,
        root: Object<'_>,
        path: &str,
        max_nodes: usize,
        max_depth: usize,
    ) -> Result<String> {
        let mut scan = Scan {
            api,
            nodes: Vec::new(),
            errors: 0,
            diagnostics: Vec::new(),
            truncated: false,
            deadline: Instant::now() + Duration::from_secs(5),
            max_nodes,
            // Depth is relative to this scope, but every emitted child path
            // must remain actionable within the global path depth bound.
            max_depth: max_depth.min(MAX_DEPTH.saturating_sub(path.split('/').count() - 1)),
            bytes: 0,
            exhausted: false,
        };
        scan.visit(root, path.into(), 0, None)?;
        Ok(format!(
            r#"{{"backend":"atspi","nodes":[{}],"truncated":{},"errors":{},"diagnostics":[{}]}}"#,
            scan.nodes.join(","),
            scan.truncated,
            scan.errors,
            scan.diagnostics.join(",")
        ))
    }
    pub(super) fn snapshot_scope(path: &str, max_nodes: usize, max_depth: usize) -> Result<String> {
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let root = api.resolve(path)?;
        scan_root(api, root, path, max_nodes, max_depth)
    }

    pub(super) fn node(path: &str) -> Result<String> {
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let root = api.resolve(path)?;
        unsafe {
            (api.clear_cache)(root.ptr);
        }
        let snapshot = scan_root(api, root, path, 1, 0)?;
        crate::browser::with_json(&snapshot, |value| {
            if value.get("errors").and_then(|n| n.integer().ok()) != Some(0) {
                return Err("accessibility node is unavailable or incomplete".into());
            }
            value
                .get("nodes")
                .and_then(|n| n.elements().next())
                .map(|node| node.raw().to_owned())
                .ok_or_else(|| "accessibility node is unavailable".into())
        })
    }

    pub(super) fn hit_test(path: &str, x: i32, y: i32) -> Result<bool> {
        let depth = path.split('/').count();
        if depth < 4 {
            return Err(
                "unsupported: accessibility hit testing requires a node inside an explicit window"
                    .into(),
            );
        }
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let parent = |object: Obj| -> Result<Option<Object<'static>>> {
            let mut error = core::ptr::null_mut();
            let pointer = unsafe { (api.parent)(object, &mut error) };
            if let Err(error) = api.error(error) {
                if !pointer.is_null() {
                    unsafe {
                        (api.unref)(pointer);
                    }
                }
                return Err(format!(
                    "unsupported: accessibility parent could not be verified: {error}"
                ));
            }
            if pointer.is_null() {
                Ok(None)
            } else {
                api.object(pointer).map(Some)
            }
        };
        let mut branch = vec![api.resolve(path)?];
        // The path begins desktop/application/window. Walk only that original
        // branch, stopping at its window; never enumerate a subtree or sibling.
        for _ in 3..depth {
            let next = parent(branch.last().unwrap().ptr)?
                .ok_or("unsupported: accessibility branch no longer reaches its window")?;
            branch.push(next);
        }
        for index in 1..branch.len() {
            let component = unsafe { (api.component_iface)(branch[index].ptr) };
            if component.is_null() {
                return Err(
                    "unsupported: an accessibility ancestor does not provide point hit testing"
                        .into(),
                );
            }
            let component = api.object(component)?;
            let mut error = core::ptr::null_mut();
            // ATSPI_COORD_TYPE_SCREEN == 0; returned accessibles are owned.
            let pointer = unsafe { (api.at_point)(component.ptr, x, y, 0, &mut error) };
            if let Err(error) = api.error(error) {
                if !pointer.is_null() {
                    unsafe {
                        (api.unref)(pointer);
                    }
                }
                return Err(format!(
                    "unsupported: accessibility point hit testing failed: {error}"
                ));
            }
            if pointer.is_null() {
                return Ok(false);
            }
            let mut hit = api.object(pointer)?;
            let mut on_branch = false;
            for _ in 0..MAX_DEPTH + 2 {
                if branch[..index].iter().any(|node| node.ptr == hit.ptr) {
                    on_branch = true;
                    break;
                }
                if hit.ptr == branch[index].ptr {
                    break;
                }
                let Some(next) = parent(hit.ptr)? else {
                    break;
                };
                hit = next;
            }
            if !on_branch {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn check_identity(object: &Object<'_>, checks: IdentityChecks<'_>) -> Result<()> {
        check_identity_with_check(object, checks, || Ok(()))
    }

    fn check_identity_with_check(
        object: &Object<'_>,
        checks: IdentityChecks<'_>,
        mut before: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        if checks.is_empty() {
            return Ok(());
        }
        before()?;
        let api = object.api;
        // Hold this exact native object until dispatch. Clearing its cache
        // refreshes properties without resolving the child-index path again.
        unsafe { (api.clear_cache)(object.ptr) };
        if let Some(expected) = checks.pid {
            before()?;
            let mut error = core::ptr::null_mut();
            let actual = unsafe { (api.process_id)(object.ptr, &mut error) };
            api.error(error).map_err(|error| {
                format!("stale_target: accessibility process could not be verified: {error}")
            })?;
            if actual != expected {
                return Err(
                    "stale_target: accessibility process does not match expected_pid".into(),
                );
            }
        }
        for (field, expected, getter, limit) in [
            ("role", checks.role, api.role, 4096),
            ("name", checks.name, api.name, 65536),
        ] {
            if let Some(expected) = expected {
                before()?;
                let actual = api
                    .identity_string(object.ptr, getter, limit)
                    .map_err(|error| {
                        format!(
                            "stale_target: accessibility {field} could not be verified: {error}"
                        )
                    })?;
                if actual != expected {
                    return Err(format!(
                        "stale_target: accessibility {field} does not match expected_{field}"
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn action(
        path: &str,
        index: usize,
        checks: IdentityChecks<'_>,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        cancellation.check()?;
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let object = api.resolve(path)?;
        // SAFETY: each interface is a separately owned reference from a live object.
        let interface = api.object(unsafe { (api.action_iface)(object.ptr) })?;
        let mut error = core::ptr::null_mut();
        let count = unsafe { (api.actions_count)(interface.ptr, &mut error) };
        api.error(error)?;
        if count < 0 || index >= count as usize {
            return Err("accessibility action does not exist".into());
        }
        check_identity(&object, checks)?;
        cancellation.dispatch(|| {
            let mut error = core::ptr::null_mut();
            let success = unsafe { (api.do_action)(interface.ptr, index as c_int, &mut error) };
            api.error(error)?;
            if success == 0 {
                return Err("application rejected the accessibility action".into());
            }
            Ok(())
        })?;
        Ok(format!(
            r#"{{"path":{},"action":{index},"completed":true}}"#,
            quote(path)
        ))
    }
    pub(super) fn focus(
        path: &str,
        checks: IdentityChecks<'_>,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        cancellation.check()?;
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let object = api.resolve(path)?;
        let interface = api.object(unsafe { (api.component_iface)(object.ptr) })?;
        check_identity(&object, checks)?;
        cancellation.dispatch(|| {
            let mut error = core::ptr::null_mut();
            let success = unsafe { (api.grab_focus)(interface.ptr, &mut error) };
            api.error(error)?;
            if success == 0 {
                return Err("application rejected accessibility focus".into());
            }
            Ok(())
        })?;
        Ok(format!(r#"{{"path":{},"focused":true}}"#, quote(path)))
    }
    pub(super) fn set_text(path: &str, text: &str) -> Result<String> {
        edit_text(
            path,
            text,
            None,
            None,
            &crate::observation::Cancellation::default(),
        )
    }

    pub(super) fn set_text_checked(
        path: &str,
        text: &str,
        expected_text: Option<&str>,
        expected_pid: Option<u32>,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        edit_text(path, text, expected_text, expected_pid, cancellation)
    }

    fn editable_text(api: &Api, object: &Object<'_>) -> Result<String> {
        let interface = api.object(unsafe { (api.text_iface)(object.ptr) })?;
        let mut error = core::ptr::null_mut();
        let count = unsafe { (api.text_count)(interface.ptr, &mut error) };
        api.error(error)?;
        if !(0..=65536).contains(&count) {
            return Err("editable text exceeds the 64 KiB check bound".into());
        }
        let mut error = core::ptr::null_mut();
        let pointer = unsafe { (api.text)(interface.ptr, 0, count, &mut error) };
        // Snapshot strings are clipped to 4096 characters. An edit precondition
        // must instead read the full bounded value and reject missing text.
        let value = if pointer.is_null() {
            Err("editable text is unavailable".to_owned())
        } else {
            // SAFETY: AT-SPI returned an owned NUL-terminated UTF-8 string.
            unsafe {
                let value = CStr::from_ptr(pointer)
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|_| "editable text is not UTF-8".to_owned());
                (api.free)(pointer.cast());
                value
            }
        };
        api.error(error)?;
        let value = value?;
        if value.len() > 65536 {
            return Err("editable text exceeds the 64 KiB check bound".into());
        }
        if value.chars().count() != count as usize {
            return Err("editable text changed while it was being read".into());
        }
        Ok(value)
    }

    fn edit_text(
        path: &str,
        text: &str,
        expected_text: Option<&str>,
        expected_pid: Option<u32>,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        cancellation.check()?;
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let object = api.resolve(path)?;
        let checked = expected_text.is_some() || expected_pid.is_some();
        if checked {
            unsafe { (api.clear_cache)(object.ptr) };
        }
        if api
            .get_string(object.ptr, api.role)?
            .to_ascii_lowercase()
            .contains("password")
        {
            return Err("password input requires human assistance".into());
        }
        let pid = if let Some(expected) = expected_pid {
            let mut error = core::ptr::null_mut();
            let actual = unsafe { (api.process_id)(object.ptr, &mut error) };
            api.error(error)?;
            if actual != expected {
                return Err(
                    "stale_target: accessibility process does not match expected_pid".into(),
                );
            }
            Some(actual)
        } else {
            None
        };
        if checked {
            let current = editable_text(api, &object)?;
            if expected_text.is_some_and(|expected| current != expected) {
                return Err("accessibility text precondition failed; no text was written".into());
            }
        }
        let _interface = api.object(unsafe { (api.editable_iface)(object.ptr) })?;
        let string = CString::new(text).map_err(|_| "text contains NUL")?;
        text_edit::set_contents(&object, &string, cancellation)?;
        text_edit::check_submitted(cancellation)?;
        if checked {
            unsafe { (api.clear_cache)(object.ptr) };
            let actual = editable_text(api, &object);
            text_edit::check_submitted(cancellation)?;
            let actual = actual.map_err(|error| {
                format!("accessibility text was submitted; readback failed: {error}")
            })?;
            if actual != text {
                return Err(format!(
                    "accessibility text was submitted; readback differs: {}",
                    quote(&actual)
                ));
            }
            return Ok(format!(
                r#"{{"path":{},"pid":{},"characters":{},"text":{},"verified":true}}"#,
                quote(path),
                pid.map_or_else(|| "null".into(), |pid| pid.to_string()),
                actual.chars().count(),
                quote(&actual),
            ));
        }
        Ok(format!(
            r#"{{"path":{},"characters":{}}}"#,
            quote(path),
            text.chars().count()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
#[path = "accessibility_portable.rs"]
mod native;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_bounds_are_checked_before_native_calls() {
        for path in ["0", "0/1/22", "0/0"] {
            assert!(validate_path(path).is_ok());
        }
        for path in ["", "1", "0/", "0/-1", "0/abc", "0/1000001", "0//2"] {
            assert!(validate_path(path).is_err());
        }
        assert!(snapshot(0, 4).is_err());
        assert!(snapshot(5, MAX_DEPTH + 1).is_err());
        assert!(action("0", 64).is_err());
    }

    #[test]
    fn deeper_paths_retain_depth_byte_and_node_bounds() {
        for depth in [33, 35, 64] {
            let path = format!("0{}", "/0".repeat(depth));
            assert!(validate_scope(&path).is_ok(), "depth {depth}");
        }
        let too_deep = format!("0{}", "/0".repeat(65));
        assert!(validate_scope(&too_deep).is_err());
        let largest_path = format!("0/{}", "0".repeat(1022));
        assert!(validate_scope(&largest_path).is_ok());
        assert!(validate_scope(&format!("{largest_path}0")).is_err());
        assert!(validate_snapshot_limits(4096, 64).is_ok());
        assert!(validate_snapshot_limits(4097, 64).is_err());
        assert!(validate_snapshot_limits(4096, 65).is_err());
        assert!(snapshot_scope("0/1", 4096, 65).is_err());
    }
}
