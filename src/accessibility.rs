//! Bounded native Linux AT-SPI observation and background actions.
//! Paths are child-index paths from desktop 0. Re-observe after UI changes.
#[cfg(target_os = "linux")]
use crate::browser::quote;
type Result<T> = std::result::Result<T, String>;

/// Read roles, names, text, screen bounds and supported actions.
/// Password text is redacted. Partial observations carry truncated/errors.
pub fn snapshot(max_nodes: usize, max_depth: usize) -> Result<String> {
    validate_limits(max_nodes, max_depth)?;
    native::snapshot(max_nodes, max_depth)
}
/// Validate a desktop observation scope without opening the accessibility bus.
pub fn validate_scope(path: &str) -> Result<()> {
    validate_path(path)
}

/// Read only one already selected accessibility subtree.
pub fn snapshot_scope(path: &str, max_nodes: usize, max_depth: usize) -> Result<String> {
    validate_path(path)?;
    validate_limits(max_nodes, max_depth)?;
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

fn validate_limits(max_nodes: usize, max_depth: usize) -> Result<()> {
    if !(1..=4096).contains(&max_nodes) || max_depth > 32 {
        return Err("accessibility limits: nodes 1..4096, depth 0..32".into());
    }
    Ok(())
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

/// Read accessibility names and text as plain text.
#[cfg(target_os = "linux")]
pub fn text(max_nodes: usize, max_depth: usize) -> Result<String> {
    validate_limits(max_nodes, max_depth)?;
    native::text(max_nodes, max_depth)
}

#[cfg(not(target_os = "linux"))]
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
    validate_path(path)?;
    if index > 63 {
        return Err("accessibility action index exceeds 63".into());
    }
    native::action(path, index)
}
/// Request accessibility focus without moving the pointer.
pub fn focus(path: &str) -> Result<String> {
    validate_path(path)?;
    native::focus(path)
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
        || path.split('/').count() > 33
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
        desktop: unsafe extern "C" fn(c_int) -> Obj,
        name: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
        role: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
        role_id: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
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
        unref: unsafe extern "C" fn(Obj),
        error_free: unsafe extern "C" fn(ErrPtr),
    }
    fn load() -> Result<&'static Api> {
        LIBRARY
            .get_or_init(Api::open)
            .as_ref()
            .map_err(Clone::clone)
    }
    impl Api {
        fn open() -> Result<Self> {
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
                    desktop: library.symbol(c"atspi_get_desktop")?,
                    name: library.symbol(c"atspi_accessible_get_name")?,
                    role: library.symbol(c"atspi_accessible_get_role_name")?,
                    role_id: library.symbol(c"atspi_accessible_get_role")?,
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
                    unref: library.symbol(c"g_object_unref")?,
                    error_free: library.symbol(c"g_error_free")?,
                };
                // atspi_init can create process-global state even on failure.
                // Pin before invoking it so callbacks never outlive the image.
                library.pin();
                let status = init();
                if status != 0 && status != 1 {
                    return Err(format!("AT-SPI initialization failed ({status})"));
                }
                set_timeout(500, 1000);
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
        fn password_role(&self, object: Obj) -> Result<bool> {
            const INVALID: c_int = 0;
            const PASSWORD_TEXT: c_int = 40;
            const UNKNOWN: c_int = 67;
            let mut error = core::ptr::null_mut();
            // SAFETY: the accessible is live and calls are serialized.
            let role = unsafe { (self.role_id)(object, &mut error) };
            self.error(error)?;
            match role {
                PASSWORD_TEXT => Ok(true),
                INVALID | UNKNOWN => Err("AT-SPI could not determine the text role".into()),
                _ => Ok(false),
            }
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
            for index in path.split('/').skip(1) {
                let index = index.parse::<usize>().map_err(|_| "invalid child index")?;
                if index >= self.count(object.ptr)? {
                    return Err("accessibility path is stale; take a fresh snapshot".into());
                }
                object = self.child(object.ptr, index)?;
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
    #[derive(Clone, Copy)]
    enum Projection {
        Tree,
        Text,
    }

    struct Scan<'a> {
        projection: Projection,
        api: &'a Api,
        nodes: Vec<String>,
        errors: usize,
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
                || self.nodes.len() >= self.max_nodes
                || Instant::now() >= self.deadline
            {
                self.truncated = true;
                return Ok(());
            }
            let pid = inherited_pid.or_else(|| {
                if matches!(self.projection, Projection::Text) {
                    return None;
                }
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
            let count = self.api.count(object.ptr).unwrap_or_else(|_| {
                self.errors += 1;
                0
            });
            let text = if let Ok(interface) = self
                .api
                .object(unsafe { (self.api.text_iface)(object.ptr) })
            {
                match self.api.password_role(object.ptr) {
                    Ok(false) => self.read_text(interface.ptr).unwrap_or_else(|_| {
                        self.errors += 1;
                        String::new()
                    }),
                    Ok(true) => "[redacted]".into(),
                    Err(_) => {
                        self.errors += 1;
                        "[redacted]".into()
                    }
                }
            } else {
                String::new()
            };
            let node = match self.projection {
                Projection::Text => {
                    let mut line = format!("{path} [{role}] {name}");
                    if !text.is_empty() && text != name {
                        line.push_str(": ");
                        line.push_str(&text);
                    }
                    line.push('\n');
                    line
                }
                Projection::Tree => {
                    let pid_json = pid.map_or_else(|| "null".to_owned(), |value| value.to_string());
                    let (visible, showing) = match self
                        .api
                        .object(unsafe { (self.api.state_set)(object.ptr) })
                    {
                        Ok(states) => {
                            // AtspiStateType: VISIBLE == 30, SHOWING == 25.
                            let visible = unsafe { (self.api.state_contains)(states.ptr, 30) != 0 };
                            let showing = unsafe { (self.api.state_contains)(states.ptr, 25) != 0 };
                            (visible.to_string(), showing.to_string())
                        }
                        Err(_) => ("null".into(), "null".into()),
                    };
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
                                let string = unsafe {
                                    (self.api.action_name)(interface.ptr, index, &mut error)
                                };
                                if let Ok(name) = self.api.string(string, error) {
                                    actions.push(format!(
                                        r#"{{"index":{index},"name":{}}}"#,
                                        quote(&name.chars().take(256).collect::<String>())
                                    ));
                                }
                            }
                        }
                    }
                    format!(
                        r#"{{"path":{},"pid":{pid_json},"role":{},"name":{},"text":{},"children":{count},"bounds":{bounds},"visible":{visible},"showing":{showing},"actions":[{}]}}"#,
                        quote(&path),
                        quote(&role),
                        quote(&name),
                        quote(&text),
                        actions.join(",")
                    )
                }
            };
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
        fn read_text(&self, interface: Obj) -> Result<String> {
            let mut error = core::ptr::null_mut();
            // SAFETY: the caller owns the interface and holds the AT-SPI lock.
            let length = unsafe { (self.api.text_count)(interface, &mut error) };
            self.api.error(error)?;
            if length <= 0 {
                return Ok(String::new());
            }
            let mut error = core::ptr::null_mut();
            let pointer = unsafe { (self.api.text)(interface, 0, length.min(4096), &mut error) };
            self.api.string(pointer, error)
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

    pub(super) fn text(max_nodes: usize, max_depth: usize) -> Result<String> {
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let mut scan = scan(
            api,
            api.resolve("0")?,
            "0",
            max_nodes,
            max_depth,
            Projection::Text,
        )?;
        if scan.truncated {
            scan.nodes.push("[observation truncated]\n".into());
        }
        Ok(scan.nodes.concat())
    }

    fn scan_root(
        api: &Api,
        root: Object<'_>,
        path: &str,
        max_nodes: usize,
        max_depth: usize,
    ) -> Result<String> {
        let scan = scan(api, root, path, max_nodes, max_depth, Projection::Tree)?;
        Ok(format!(
            r#"{{"backend":"atspi","nodes":[{}],"truncated":{},"errors":{}}}"#,
            scan.nodes.join(","),
            scan.truncated,
            scan.errors
        ))
    }
    fn scan<'a>(
        api: &'a Api,
        root: Object<'_>,
        path: &str,
        max_nodes: usize,
        max_depth: usize,
        projection: Projection,
    ) -> Result<Scan<'a>> {
        let mut scan = Scan {
            projection,
            api,
            nodes: Vec::new(),
            errors: 0,
            truncated: false,
            deadline: Instant::now() + Duration::from_secs(5),
            max_nodes,
            max_depth,
            bytes: 0,
            exhausted: false,
        };
        scan.visit(root, path.into(), 0, None)?;
        Ok(scan)
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
            for _ in 0..34 {
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

    pub(super) fn action(path: &str, index: usize) -> Result<String> {
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
        let mut error = core::ptr::null_mut();
        let success = unsafe { (api.do_action)(interface.ptr, index as c_int, &mut error) };
        api.error(error)?;
        if success == 0 {
            return Err("application rejected the accessibility action".into());
        }
        Ok(format!(
            r#"{{"path":{},"action":{index},"completed":true}}"#,
            quote(path)
        ))
    }
    pub(super) fn focus(path: &str) -> Result<String> {
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let object = api.resolve(path)?;
        let interface = api.object(unsafe { (api.component_iface)(object.ptr) })?;
        let mut error = core::ptr::null_mut();
        let success = unsafe { (api.grab_focus)(interface.ptr, &mut error) };
        api.error(error)?;
        if success == 0 {
            return Err("application rejected accessibility focus".into());
        }
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
        if api.password_role(object.ptr)? {
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
        assert!(snapshot(5, 33).is_err());
        assert!(action("0", 64).is_err());
    }
}
