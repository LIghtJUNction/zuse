//! Bounded native Linux AT-SPI observation and background actions.
//! Paths are child-index paths from desktop 0. Re-observe after UI changes.
#[cfg(target_os = "linux")]
use crate::browser::quote;
type Result<T> = std::result::Result<T, String>;

/// Read roles, names, text, screen bounds and supported actions.
/// Password text is redacted. Partial observations carry truncated/errors.
pub fn snapshot(max_nodes: usize, max_depth: usize) -> Result<String> {
    if !(1..=4096).contains(&max_nodes) || max_depth > 32 {
        return Err("accessibility limits: nodes 1..4096, depth 0..32".into());
    }
    native::snapshot(max_nodes, max_depth)
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
    validate_path(path)?;
    if text.len() > 65536 || text.contains('\0') {
        return Err("text must be NUL-free and at most 64 KiB".into());
    }
    native::set_text(path, text)
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

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use core::ffi::{c_char, c_int, c_void};
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
    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlopen(name: *const c_char, flags: c_int) -> Obj;
        fn dlsym(handle: Obj, name: *const c_char) -> Obj;
    }
    // AT-SPI owns process-global D-Bus state; its library stays loaded.
    // Every call from this module is serialized by ACCESS.
    static LIBRARY: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    static ACCESS: Mutex<()> = Mutex::new(());
    struct Api {
        desktop: unsafe extern "C" fn(c_int) -> Obj,
        name: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
        role: unsafe extern "C" fn(Obj, *mut ErrPtr) -> *mut c_char,
        count: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        child: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> Obj,
        text_iface: unsafe extern "C" fn(Obj) -> Obj,
        text_count: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        text: unsafe extern "C" fn(Obj, c_int, c_int, *mut ErrPtr) -> *mut c_char,
        action_iface: unsafe extern "C" fn(Obj) -> Obj,
        actions_count: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        action_name: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> *mut c_char,
        do_action: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> c_int,
        component_iface: unsafe extern "C" fn(Obj) -> Obj,
        extents: unsafe extern "C" fn(Obj, c_int, *mut ErrPtr) -> *mut Rect,
        grab_focus: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
        editable_iface: unsafe extern "C" fn(Obj) -> Obj,
        set_contents: unsafe extern "C" fn(Obj, *const c_char, *mut ErrPtr) -> c_int,
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
            // SAFETY: static NUL-terminated soname, RTLD_NOW. Keep the handle loaded.
            let handle = unsafe { dlopen(c"libatspi.so.0".as_ptr(), 2) };
            if handle.is_null() {
                return Err("AT-SPI unavailable; install at-spi2-core and enable the session accessibility bus".into());
            }
            unsafe fn symbol<T: Copy>(handle: Obj, name: &CStr) -> Result<T> {
                // SAFETY: every T used below matches this symbol's public C ABI.
                let pointer = unsafe { dlsym(handle, name.as_ptr()) };
                if pointer.is_null() {
                    return Err(format!("AT-SPI symbol missing: {}", name.to_string_lossy()));
                }
                if core::mem::size_of::<T>() != core::mem::size_of::<Obj>() {
                    return Err("unsupported function pointer ABI".into());
                }
                Ok(unsafe { core::mem::transmute_copy(&pointer) })
            }
            // SAFETY: signatures follow the libatspi public C headers.
            unsafe {
                let init: unsafe extern "C" fn() -> c_int = symbol(handle, c"atspi_init")?;
                let set_timeout: unsafe extern "C" fn(c_int, c_int) =
                    symbol(handle, c"atspi_set_timeout")?;
                let status = init();
                if status != 0 && status != 1 {
                    return Err(format!("AT-SPI initialization failed ({status})"));
                }
                set_timeout(500, 1000);
                Ok(Self {
                    desktop: symbol(handle, c"atspi_get_desktop")?,
                    name: symbol(handle, c"atspi_accessible_get_name")?,
                    role: symbol(handle, c"atspi_accessible_get_role_name")?,
                    count: symbol(handle, c"atspi_accessible_get_child_count")?,
                    child: symbol(handle, c"atspi_accessible_get_child_at_index")?,
                    text_iface: symbol(handle, c"atspi_accessible_get_text_iface")?,
                    text_count: symbol(handle, c"atspi_text_get_character_count")?,
                    text: symbol(handle, c"atspi_text_get_text")?,
                    action_iface: symbol(handle, c"atspi_accessible_get_action_iface")?,
                    actions_count: symbol(handle, c"atspi_action_get_n_actions")?,
                    action_name: symbol(handle, c"atspi_action_get_action_name")?,
                    do_action: symbol(handle, c"atspi_action_do_action")?,
                    component_iface: symbol(handle, c"atspi_accessible_get_component_iface")?,
                    extents: symbol(handle, c"atspi_component_get_extents")?,
                    grab_focus: symbol(handle, c"atspi_component_grab_focus")?,
                    editable_iface: symbol(handle, c"atspi_accessible_get_editable_text_iface")?,
                    set_contents: symbol(handle, c"atspi_editable_text_set_text_contents")?,
                    free: symbol(handle, c"g_free")?,
                    unref: symbol(handle, c"g_object_unref")?,
                    error_free: symbol(handle, c"g_error_free")?,
                })
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
    struct Scan<'a> {
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
        fn visit(&mut self, object: Object<'_>, path: String, depth: usize) -> Result<()> {
            if self.exhausted
                || self.nodes.len() >= self.max_nodes
                || Instant::now() >= self.deadline
            {
                self.truncated = true;
                return Ok(());
            }
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
                r#"{{"path":{},"role":{},"name":{},"text":{},"children":{count},"bounds":{bounds},"actions":[{}]}}"#,
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
                    Ok(child) => self.visit(child, format!("{path}/{index}"), depth + 1)?,
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
        let mut scan = Scan {
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
        scan.visit(root, "0".into(), 0)?;
        Ok(format!(
            r#"{{"backend":"atspi","nodes":[{}],"truncated":{},"errors":{}}}"#,
            scan.nodes.join(","),
            scan.truncated,
            scan.errors
        ))
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
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let object = api.resolve(path)?;
        if api
            .get_string(object.ptr, api.role)?
            .to_ascii_lowercase()
            .contains("password")
        {
            return Err("password input requires human assistance".into());
        }
        let interface = api.object(unsafe { (api.editable_iface)(object.ptr) })?;
        let string = CString::new(text).map_err(|_| "text contains NUL")?;
        let mut error = core::ptr::null_mut();
        let success = unsafe { (api.set_contents)(interface.ptr, string.as_ptr(), &mut error) };
        api.error(error)?;
        if success == 0 {
            return Err("application rejected accessibility text input".into());
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
