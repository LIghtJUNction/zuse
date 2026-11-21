use super::super::dylib::Library;
use super::super::{Result, image_metadata, quote, split_chord, validate_text};
use std::{
    ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void},
    path::Path,
    ptr,
    sync::Once,
};
type Dpy = *mut c_void;
type Window = c_ulong;
type Atom = c_ulong;
macro_rules! api {
    ($($name:ident($($arg:ty),*)->$ret:ty;)+) => {
        #[allow(non_snake_case)]
        struct Api { $($name:unsafe extern "C" fn($($arg),*)->$ret,)+ }
        impl Api { unsafe fn load(lib:&Library)->Result<Self> {Ok(Self{
            $($name:unsafe{lib.symbol(CStr::from_bytes_with_nul_unchecked(concat!(stringify!($name),"\0").as_bytes()))?},)+
        })} }
    }
}
api! {
    XOpenDisplay(*const c_char)->Dpy;
    XCloseDisplay(Dpy)->c_int;
    XDefaultRootWindow(Dpy)->Window;
    XDefaultScreen(Dpy)->c_int;
    XQueryPointer(Dpy,Window,*mut Window,*mut Window,*mut c_int,*mut c_int,*mut c_int,*mut c_int,*mut c_uint)->c_int;
    XFlush(Dpy)->c_int;
    XSync(Dpy,c_int)->c_int;
    XInternAtom(Dpy,*const c_char,c_int)->Atom;
    XGetWindowProperty(Dpy,Window,Atom,std::ffi::c_long,std::ffi::c_long,c_int,Atom,*mut Atom,*mut c_int,*mut c_ulong,*mut c_ulong,*mut *mut u8)->c_int;
    XQueryTree(Dpy,Window,*mut Window,*mut Window,*mut *mut Window,*mut c_uint)->c_int;
    XGetGeometry(Dpy,Window,*mut Window,*mut c_int,*mut c_int,*mut c_uint,*mut c_uint,*mut c_uint,*mut c_uint)->c_int;
    XGetWindowAttributes(Dpy,Window,*mut WindowAttributes)->c_int;
    XTranslateCoordinates(Dpy,Window,Window,c_int,c_int,*mut c_int,*mut c_int,*mut Window)->c_int;
    XFetchName(Dpy,Window,*mut *mut c_char)->c_int;
    XGetClassHint(Dpy,Window,*mut ClassHint)->c_int;
    XGetInputFocus(Dpy,*mut Window,*mut c_int)->c_int;
    XSetInputFocus(Dpy,Window,c_int,c_ulong)->c_int;
    XRaiseWindow(Dpy,Window)->c_int;
    XGetImage(Dpy,Window,c_int,c_int,c_uint,c_uint,c_ulong,c_int)->*mut XImage;
    XDestroyImage(*mut XImage)->c_int;
    XGetPixel(*mut XImage,c_int,c_int)->c_ulong;
    XStringToKeysym(*const c_char)->c_ulong;
    XKeysymToKeycode(Dpy,c_ulong)->u8;
    XDisplayKeycodes(Dpy,*mut c_int,*mut c_int)->c_int;
    XGetKeyboardMapping(Dpy,u8,c_int,*mut c_int)->*mut c_ulong;
    XDisplayString(Dpy)->*const c_char;
    XQueryKeymap(Dpy,*mut c_char)->c_int;
    XkbGetState(Dpy,c_uint,*mut KeyboardState)->c_int;
    XkbKeysymToModifiers(Dpy,c_ulong)->c_uint;
    XkbLockModifiers(Dpy,c_uint,c_uint,c_uint)->c_int;
    XFree(*mut c_void)->c_int;
    XSendEvent(Dpy,Window,c_int,std::ffi::c_long,*mut c_void)->c_int;
    XSetErrorHandler(Option<unsafe extern "C" fn(Dpy,*mut c_void)->c_int>)->Option<unsafe extern "C" fn(Dpy,*mut c_void)->c_int>;
}
#[repr(C)]
struct ClassHint {
    name: *mut c_char,
    class: *mut c_char,
}
#[repr(C)]
#[derive(Default)]
struct WindowAttributes {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    border_width: c_int,
    depth: c_int,
    visual: *mut c_void,
    root: Window,
    class: c_int,
    bit_gravity: c_int,
    win_gravity: c_int,
    backing_store: c_int,
    backing_planes: c_ulong,
    backing_pixel: c_ulong,
    save_under: c_int,
    colormap: c_ulong,
    map_installed: c_int,
    map_state: c_int,
    all_event_masks: std::ffi::c_long,
    your_event_mask: std::ffi::c_long,
    do_not_propagate_mask: std::ffi::c_long,
    override_redirect: c_int,
    screen: *mut c_void,
}
#[repr(C)]
#[derive(Default)]
struct KeyboardState {
    group: u8,
    locked_group: u8,
    base_group: u16,
    latched_group: u16,
    mods: u8,
    base_mods: u8,
    latched_mods: u8,
    locked_mods: u8,
    compat_state: u8,
    grab_mods: u8,
    compat_grab_mods: u8,
    lookup_mods: u8,
    compat_lookup_mods: u8,
    ptr_buttons: u16,
}

/// Plan the whole string before dispatch: a missing symbol must never leave an
/// ASCII prefix in the receiver. Only the two unmodified-group levels are used.
fn key_plan(mapping: &[c_ulong], min: u8, per: usize, text: &str) -> Option<Vec<(u8, bool)>> {
    text.chars()
        .map(|ch| {
            let sym = match ch {
                '\n' | '\r' => 0xff0d,
                '\t' => 0xff09,
                '\u{8}' => 0xff08,
                c if (c as u32) <= 255 => c as c_ulong,
                c => 0x01000000 | c as c_ulong,
            };
            mapping
                .chunks_exact(per)
                .enumerate()
                .find_map(|(index, row)| {
                    if row[0] == sym {
                        Some((min + index as u8, false))
                    } else if per > 1 && row[1] == sym {
                        Some((min + index as u8, true))
                    } else {
                        None
                    }
                })
        })
        .collect()
}
#[repr(C)]
struct XImage {
    width: c_int,
    height: c_int,
    xoffset: c_int,
    format: c_int,
    data: *mut c_char,
    byte_order: c_int,
    bitmap_unit: c_int,
    bitmap_bit_order: c_int,
    bitmap_pad: c_int,
    depth: c_int,
    bytes_per_line: c_int,
    bits_per_pixel: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
}
#[repr(C)]
struct KeyEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: Dpy,
    window: Window,
    root: Window,
    subwindow: Window,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    keycode: c_uint,
    same_screen: c_int,
}
struct XTest {
    _lib: Library,
    key: unsafe extern "C" fn(Dpy, c_uint, c_int, c_ulong) -> c_int,
    button: unsafe extern "C" fn(Dpy, c_uint, c_int, c_ulong) -> c_int,
    motion: unsafe extern "C" fn(Dpy, c_int, c_int, c_int, c_ulong) -> c_int,
}
impl XTest {
    fn load(display: Dpy) -> Result<Self> {
        let lib = Library::open(c"libXtst.so.6")?;
        let query: unsafe extern "C" fn(
            Dpy,
            *mut c_int,
            *mut c_int,
            *mut c_int,
            *mut c_int,
        ) -> c_int = unsafe { lib.symbol(c"XTestQueryExtension")? };
        let (mut event, mut error, mut major, mut minor) = (0, 0, 0, 0);
        if unsafe { query(display, &mut event, &mut error, &mut major, &mut minor) } == 0 {
            return Err("X11 server does not provide the XTest extension".into());
        }
        // SAFETY: function pointer types match XTEST 2.2.
        unsafe {
            Ok(Self {
                key: lib.symbol(c"XTestFakeKeyEvent")?,
                button: lib.symbol(c"XTestFakeButtonEvent")?,
                motion: lib.symbol(c"XTestFakeMotionEvent")?,
                _lib: lib,
            })
        }
    }
}
pub(super) struct X11 {
    api: Api,
    display: Dpy,
    root: Window,
    test: Option<XTest>,
    _lib: Library,
}
impl Drop for X11 {
    fn drop(&mut self) {
        unsafe {
            (self.api.XCloseDisplay)(self.display);
        }
    }
}
unsafe extern "C" fn ignore_error(_: Dpy, _: *mut c_void) -> c_int {
    0
}
impl X11 {
    pub fn connect() -> Result<Self> {
        let lib = Library::open(c"libX11.so.6")?;
        // SAFETY: dynamically loaded Xlib functions retain their library owner.
        unsafe {
            let api = Api::load(&lib)?;
            static ERROR_HANDLER: Once = Once::new();
            ERROR_HANDLER.call_once(|| {
                (api.XSetErrorHandler)(Some(ignore_error));
            });
            let display = (api.XOpenDisplay)(ptr::null());
            if display.is_null() {
                return Err("XOpenDisplay failed; set DISPLAY to an accessible X11 display".into());
            }
            let root = (api.XDefaultRootWindow)(display);
            Ok(Self {
                api,
                display,
                root,
                test: XTest::load(display).ok(),
                _lib: lib,
            })
        }
    }
    pub fn has_input(&self) -> bool {
        self.test.is_some()
    }
    fn keyboard_state(&self) -> Result<(KeyboardState, bool, c_uint, c_uint)> {
        let mut state = KeyboardState::default();
        let mut keys = [0 as c_char; 32];
        // SAFETY: these buffers match XkbStateRec and XQueryKeymap's 32 bytes.
        unsafe {
            if (self.api.XkbGetState)(self.display, 0x100, &mut state) != 0 {
                return Err("unsupported_state: X11 cannot read the current keyboard state".into());
            }
            (self.api.XQueryKeymap)(self.display, keys.as_mut_ptr());
            Ok((
                state,
                keys.iter().any(|&key| key != 0),
                (self.api.XkbKeysymToModifiers)(self.display, 0xff7f),
                (self.api.XkbKeysymToModifiers)(self.display, 0xffe5),
            ))
        }
    }
    pub fn keyboard_status(&self) -> Result<String> {
        let (state, held, num, caps) = self.keyboard_state()?;
        let lock = |mask: c_uint| {
            if mask == 0 {
                "null"
            } else if u32::from(state.locked_mods) & mask != 0 {
                "true"
            } else {
                "false"
            }
        };
        Ok(format!(
            "{{\"backend\":\"x11\",\"scope\":\"core_keyboard\",\"current\":{{\"address\":\"core\",\"main\":true,\"numLock\":{},\"capsLock\":{},\"active_layout_index\":{}}},\"other_devices\":[],\"other_devices_observed\":false,\"keys_held\":{held}}}",
            lock(num),
            lock(caps),
            state.group
        ))
    }
    pub fn num_lock(&self, action: &str) -> Result<String> {
        if action == "read" {
            return self.keyboard_status();
        }
        if !matches!(action, "on" | "off" | "toggle") {
            return Err("Num Lock action must be read, on, off or toggle".into());
        }
        let (state, held, mask, _) = self.keyboard_state()?;
        if mask == 0 {
            return Err(
                "unsupported_state: Num Lock has no modifier in the current X11 keymap".into(),
            );
        }
        if held || state.base_mods != 0 || state.latched_mods != 0 {
            return Err("unsupported_state: release held keys before changing Num Lock".into());
        }
        let before = u32::from(state.locked_mods) & mask != 0;
        let expected = match action {
            "on" => true,
            "off" => false,
            _ => !before,
        };
        if before != expected {
            // Only the explicitly requested core-keyboard lock bit is changed.
            let accepted = unsafe {
                (self.api.XkbLockModifiers)(
                    self.display,
                    0x100,
                    mask,
                    if expected { mask } else { 0 },
                )
            };
            unsafe {
                (self.api.XSync)(self.display, 0);
            }
            if accepted == 0 {
                return Err("X11 rejected the Num Lock state change; it was not retried".into());
            }
        }
        let (after, _, after_mask, _) = self.keyboard_state()?;
        if after_mask != mask || (u32::from(after.locked_mods) & mask != 0) != expected {
            return Err(
                "Num Lock readback did not match the requested state; it was not retried".into(),
            );
        }
        self.keyboard_status()
    }
    pub fn position(&self) -> Result<(i32, i32)> {
        let (mut root, mut child, mut x, mut y, mut wx, mut wy, mut state) = (0, 0, 0, 0, 0, 0, 0);
        unsafe {
            if (self.api.XQueryPointer)(
                self.display,
                self.root,
                &mut root,
                &mut child,
                &mut x,
                &mut y,
                &mut wx,
                &mut wy,
                &mut state,
            ) == 0
            {
                return Err("pointer is on a different X11 screen".into());
            }
        }
        Ok((x, y))
    }
    fn flush(&self) {
        unsafe {
            (self.api.XFlush)(self.display);
        }
    }
    pub fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        let test = self
            .test
            .as_ref()
            .ok_or("XTest extension library unavailable")?;
        unsafe {
            (test.motion)(
                self.display,
                (self.api.XDefaultScreen)(self.display),
                x,
                y,
                0,
            );
        }
        self.flush();
        Ok(())
    }
    pub fn button(&mut self, button: &str, action: &str) -> Result<()> {
        let code = match button {
            "left" => 1,
            "middle" => 2,
            "right" => 3,
            "back" => 8,
            "forward" => 9,
            _ => return Err("button must be left, middle, right, back or forward".into()),
        };
        let test = self
            .test
            .as_ref()
            .ok_or("XTest extension library unavailable")?;
        unsafe {
            match action {
                "down" => {
                    (test.button)(self.display, code, 1, 0);
                }
                "up" => {
                    (test.button)(self.display, code, 0, 0);
                }
                "click" => {
                    (test.button)(self.display, code, 1, 0);
                    (test.button)(self.display, code, 0, 0);
                }
                _ => return Err("button action must be down, up or click".into()),
            }
        }
        self.flush();
        Ok(())
    }
    pub fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        if dx.unsigned_abs() > 1000 || dy.unsigned_abs() > 1000 {
            return Err("scroll is limited to 1000 notches per axis".into());
        }
        let test = self
            .test
            .as_ref()
            .ok_or("XTest extension library unavailable")?;
        for (amount, positive, negative) in [(dy, 5, 4), (dx, 7, 6)] {
            let button = if amount < 0 { negative } else { positive };
            for _ in 0..amount.unsigned_abs() {
                unsafe {
                    (test.button)(self.display, button, 1, 0);
                    (test.button)(self.display, button, 0, 0);
                }
            }
        }
        self.flush();
        Ok(())
    }
    fn keys(&self, chord: &str) -> Result<Vec<u8>> {
        let mut result = Vec::new();
        for name in split_chord(chord)? {
            let normalized = match name.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => "Control_L",
                "alt" => "Alt_L",
                "shift" => "Shift_L",
                "super" | "win" | "meta" | "cmd" => "Super_L",
                "enter" | "return" => "Return",
                "escape" | "esc" => "Escape",
                "backspace" => "BackSpace",
                "tab" => "Tab",
                "space" => "space",
                "delete" | "del" => "Delete",
                "insert" => "Insert",
                "home" => "Home",
                "end" => "End",
                "pageup" => "Page_Up",
                "pagedown" => "Page_Down",
                "left" => "Left",
                "right" => "Right",
                "up" => "Up",
                "down" => "Down",
                _ => name,
            };
            let c = CString::new(normalized).map_err(|_| "NUL in key")?;
            let sym = unsafe { (self.api.XStringToKeysym)(c.as_ptr()) };
            if sym == 0 {
                return Err(format!("unknown X11 keysym {name:?}"));
            }
            let code = unsafe { (self.api.XKeysymToKeycode)(self.display, sym) };
            if code == 0 {
                return Err(format!("key {name:?} is absent from the X11 keyboard map"));
            }
            result.push(code);
        }
        Ok(result)
    }
    pub fn key(&mut self, chord: &str) -> Result<()> {
        let keys = self.keys(chord)?;
        let test = self
            .test
            .as_ref()
            .ok_or("XTest extension library unavailable")?;
        unsafe {
            for &code in &keys {
                (test.key)(self.display, u32::from(code), 1, 0);
            }
            for &code in keys.iter().rev() {
                (test.key)(self.display, u32::from(code), 0, 0);
            }
        }
        self.flush();
        Ok(())
    }
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        self.type_common(None, text)
    }
    pub fn type_into(&mut self, window: &str, text: &str) -> Result<()> {
        let id = self.window_id(window)?;
        self.type_common(Some(id), text)
    }
    fn type_common(&mut self, window: Option<Window>, text: &str) -> Result<()> {
        validate_text(text)?;
        if text.is_empty() {
            return Ok(());
        }
        let (mut min, mut max, mut per) = (0, 0, 0);
        // Read the active keyboard map once; existing symbols never change the map.
        let mapping = unsafe {
            (self.api.XDisplayKeycodes)(self.display, &mut min, &mut max);
            if min < 8 || max > 255 || min > max {
                return Err("invalid X11 keyboard-map range".into());
            }
            let saved =
                (self.api.XGetKeyboardMapping)(self.display, min as u8, max - min + 1, &mut per);
            if saved.is_null() {
                return Err("could not read X11 keyboard mapping".into());
            }
            if per <= 0 || per > 64 {
                (self.api.XFree)(saved.cast());
                return Err("invalid X11 keyboard mapping width".into());
            }
            let values =
                std::slice::from_raw_parts(saved, ((max - min + 1) * per) as usize).to_vec();
            (self.api.XFree)(saved.cast());
            values
        };
        let Some(plan) = key_plan(&mapping, min as u8, per as usize, text) else {
            return self.type_utf8(window, text);
        };
        if window.is_none() {
            if self.test.is_none() {
                return Err("XTest extension library unavailable".into());
            }
            // A GUI can commit while Enter (or another user key) is still down.
            // Choose the confirmed whole-string path before emitting any key;
            // never release user keys or apply this plan through another layout.
            if self.quiescent_keyboard().is_err() {
                return self.type_utf8(window, text);
            }
        }
        let shift_code = unsafe { (self.api.XKeysymToKeycode)(self.display, 0xffe1) };
        if shift_code == 0 && plan.iter().any(|&(_, shift)| shift) {
            return Err("Shift is absent from the X11 keyboard map".into());
        }
        let emit = |code: u8, shift: bool| unsafe {
            if let Some(w) = window {
                self.send_key(w, code, true, u32::from(shift));
                self.send_key(w, code, false, u32::from(shift));
            } else if let Some(test) = self.test.as_ref() {
                if shift {
                    (test.key)(self.display, u32::from(shift_code), 1, 0);
                }
                (test.key)(self.display, u32::from(code), 1, 0);
                (test.key)(self.display, u32::from(code), 0, 0);
                if shift {
                    (test.key)(self.display, u32::from(shift_code), 0, 0);
                }
            }
            (self.api.XSync)(self.display, 0);
        };
        for (code, shift) in plan {
            emit(code, shift);
        }
        Ok(())
    }
    fn quiescent_keyboard(&self) -> Result<()> {
        let mut keys = [0 as c_char; 32];
        let mut state = KeyboardState::default();
        // SAFETY: buffers match XQueryKeymap and XkbStateRec from Xlib/XKB.
        let status = unsafe {
            (self.api.XQueryKeymap)(self.display, keys.as_mut_ptr());
            (self.api.XkbGetState)(self.display, 0x100, &mut state)
        };
        if status != 0
            || keys.iter().any(|&key| key != 0)
            || state.group != 0
            || state.base_group != 0
            || state.latched_group != 0
            || state.locked_group != 0
            || state.mods != 0
            || state.base_mods != 0
            || state.latched_mods != 0
            || state.locked_mods != 0
        {
            return Err("X11 text keys require released keys, no locked/latched modifiers, and layout group zero; no input was sent".into());
        }
        Ok(())
    }
    fn window_pid(&self, window: Window) -> Option<u32> {
        let (format, bytes) = self.property(window, c"_NET_WM_PID")?;
        if format != 32 || bytes.len() != std::mem::size_of::<c_ulong>() {
            return None;
        }
        let pid = c_ulong::from_ne_bytes(bytes.try_into().ok()?);
        u32::try_from(pid).ok().filter(|&pid| pid != 0)
    }
    fn parent(&self, window: Window) -> Result<Window> {
        let (mut root, mut parent, mut children, mut count) = (0, 0, ptr::null_mut(), 0);
        // SAFETY: initialized outputs; the returned children allocation is freed
        // even when only the parent is needed.
        let status = unsafe {
            let status = (self.api.XQueryTree)(
                self.display,
                window,
                &mut root,
                &mut parent,
                &mut children,
                &mut count,
            );
            if !children.is_null() {
                (self.api.XFree)(children.cast());
            }
            status
        };
        if status == 0 || parent == window {
            return Err("could not identify the focused X11 client window".into());
        }
        Ok(parent)
    }
    fn type_utf8(&self, window: Option<Window>, text: &str) -> Result<()> {
        // MappingNotify plus XSync only orders server requests. A paused client
        // can read a later mapping for an older key event, so no temporary row
        // (or timing delay) can safely encode arbitrary Unicode here.
        if text.len() > 65536 || text.chars().any(char::is_control) {
            return Err("Unsupported X11 Unicode text: use at most 64 KiB of printable UTF-8; send control keys separately; no input was sent".into());
        }
        // AT-SPI describes this machine's session. Never correlate remote X11
        // PIDs with local accessibility objects.
        let display = unsafe { CStr::from_ptr((self.api.XDisplayString)(self.display)) };
        let display = display.to_bytes();
        if !(display.starts_with(b":")
            || display.starts_with(b"unix:")
            || display.starts_with(b"unix/:"))
        {
            return Err(
                "Unsupported X11 Unicode text on a remote display; no input was sent".into(),
            );
        }
        let focus = self.window_id("active")?;
        if window.is_some_and(|window| window != focus) {
            return Err("Unsupported background X11 Unicode text: explicitly focus the target and use type_text; no input was sent".into());
        }
        let mut client = focus;
        let mut pid = None;
        for _ in 0..32 {
            if client <= 1 || client == self.root {
                break;
            }
            pid = self.window_pid(client);
            if pid.is_some() {
                break;
            }
            client = self.parent(client)?;
        }
        let pid = pid.ok_or("Unsupported X11 Unicode text: focused window has no local process identity; no input was sent")?;
        // A local display can still have clients connected from another host.
        // EWMH requires WM_CLIENT_MACHINE alongside _NET_WM_PID; do not compare
        // a remote client's numeric PID with this session's AT-SPI processes.
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map_err(|_| "Unsupported X11 Unicode text: cannot establish the local host identity; no input was sent")?;
        let hostname = hostname.trim_end().as_bytes();
        let local_client = || {
            self.property(client, c"WM_CLIENT_MACHINE")
                .is_some_and(|(format, mut name)| {
                    while name.last() == Some(&0) {
                        name.pop();
                    }
                    format == 8 && !hostname.is_empty() && name == hostname
                })
        };
        if !local_client() {
            return Err("Unsupported X11 Unicode text: the focused client is not identified as local; no input was sent".into());
        }
        let bounds = self.geometry(client)?;
        let frame_bounds =
            self.property(client, c"_NET_FRAME_EXTENTS")
                .and_then(|(format, bytes)| {
                    if format != 32 || bytes.len() != 4 * std::mem::size_of::<c_ulong>() {
                        return None;
                    }
                    let extents = bytes
                        .as_chunks::<{ std::mem::size_of::<c_ulong>() }>()
                        .0
                        .iter()
                        .map(|v| u32::try_from(c_ulong::from_ne_bytes(*v)).ok())
                        .collect::<Option<Vec<_>>>()?;
                    let [left, right, top, bottom] = extents.as_slice() else {
                        return None;
                    };
                    Some((
                        bounds.0.checked_sub(i32::try_from(*left).ok()?)?,
                        bounds.1.checked_sub(i32::try_from(*top).ok()?)?,
                        bounds.2.checked_add(*left)?.checked_add(*right)?,
                        bounds.3.checked_add(*top)?.checked_add(*bottom)?,
                    ))
                });
        crate::accessibility::insert_focused_x11(pid, bounds, frame_bounds, text, || {
            if self.window_id("active")? != focus
                || self.window_pid(client) != Some(pid)
                || !local_client()
                || self.geometry(client)? != bounds
            {
                return Err("X11 focus, target identity or geometry changed".into());
            }
            Ok(())
        })
    }
    unsafe fn send_key(&self, window: Window, code: u8, down: bool, state: u32) {
        let mut event = [0 as c_ulong; 24];
        let value = KeyEvent {
            kind: if down { 2 } else { 3 },
            serial: 0,
            send_event: 1,
            display: self.display,
            window,
            root: self.root,
            subwindow: 0,
            time: 0,
            x: 1,
            y: 1,
            x_root: 1,
            y_root: 1,
            state,
            keycode: u32::from(code),
            same_screen: 1,
        };
        unsafe {
            event.as_mut_ptr().cast::<KeyEvent>().write(value);
            (self.api.XSendEvent)(
                self.display,
                window,
                0,
                if down { 1 } else { 2 },
                event.as_mut_ptr().cast(),
            );
        }
    }
    pub fn key_into(&mut self, window: &str, chord: &str) -> Result<()> {
        let w = self.window_id(window)?;
        let codes = self.keys(chord)?;
        let mut state = 0u32;
        for (name, &code) in split_chord(chord)?.into_iter().zip(&codes) {
            unsafe {
                self.send_key(w, code, true, state);
            }
            state |= match name.to_ascii_lowercase().as_str() {
                "shift" => 1,
                "ctrl" | "control" => 4,
                "alt" => 8,
                "super" | "meta" | "win" => 64,
                _ => 0,
            };
        }
        for &code in codes.iter().rev() {
            unsafe {
                self.send_key(w, code, false, state);
            }
        }
        self.flush();
        Ok(())
    }
    pub fn window_id(&self, id: &str) -> Result<Window> {
        let w = if id == "active" {
            let (mut w, mut revert) = (0, 0);
            unsafe {
                (self.api.XGetInputFocus)(self.display, &mut w, &mut revert);
            }
            w
        } else if let Some(hex) = id.strip_prefix("0x") {
            Window::from_str_radix(hex, 16).map_err(|_| "invalid hexadecimal X11 window ID")?
        } else {
            id.parse()
                .map_err(|_| "window ID must be decimal, 0x hexadecimal, or active")?
        };
        if w <= 1 {
            return Err("no active application window".into());
        }
        self.geometry(w)?;
        Ok(w)
    }
    fn geometry(&self, w: Window) -> Result<(i32, i32, u32, u32)> {
        let (mut root, mut x, mut y, mut width, mut height, mut border, mut depth) =
            (0, 0, 0, 0, 0, 0, 0);
        let status = unsafe {
            (self.api.XGetGeometry)(
                self.display,
                w,
                &mut root,
                &mut x,
                &mut y,
                &mut width,
                &mut height,
                &mut border,
                &mut depth,
            )
        };
        if status == 0 {
            return Err(format!("window 0x{w:x} does not exist"));
        }
        let mut child = 0;
        unsafe {
            (self.api.XTranslateCoordinates)(
                self.display,
                w,
                self.root,
                0,
                0,
                &mut x,
                &mut y,
                &mut child,
            );
        }
        Ok((x, y, width, height))
    }
    /// Resolve the frontmost root child at a point without changing focus or pointer.
    pub fn window_hit_test(&self, id: &str, x: i32, y: i32) -> Result<bool> {
        let target = self.window_id(id)?;
        let (mut dx, mut dy, mut hit) = (0, 0, 0);
        let ok = unsafe {
            (self.api.XTranslateCoordinates)(
                self.display,
                self.root,
                self.root,
                x,
                y,
                &mut dx,
                &mut dy,
                &mut hit,
            )
        };
        if ok == 0 || hit == 0 {
            return Ok(false);
        }
        let mut current = target;
        for _ in 0..32 {
            if current == hit {
                return Ok(true);
            }
            if current <= 1 || current == self.root {
                break;
            }
            let parent = self.parent(current)?;
            if parent == current {
                break;
            }
            current = parent;
        }
        Ok(false)
    }

    pub fn focus(&mut self, id: &str) -> Result<()> {
        let w = self.window_id(id)?;
        unsafe {
            (self.api.XRaiseWindow)(self.display, w);
            (self.api.XSetInputFocus)(self.display, w, 2, 0);
        }
        self.flush();
        Ok(())
    }
    fn property(&self, w: Window, name: &CStr) -> Option<(c_int, Vec<u8>)> {
        let atom = unsafe { (self.api.XInternAtom)(self.display, name.as_ptr(), 1) };
        if atom == 0 {
            return None;
        }
        let (mut actual, mut format, mut count, mut after, mut data) =
            (0, 0, 0, 0, ptr::null_mut());
        let status = unsafe {
            (self.api.XGetWindowProperty)(
                self.display,
                w,
                atom,
                0,
                16_384,
                0,
                0,
                &mut actual,
                &mut format,
                &mut count,
                &mut after,
                &mut data,
            )
        };
        if status != 0 || data.is_null() {
            return None;
        }
        let bytes = match format {
            8 => count as usize,
            16 => count as usize * 2,
            32 => count as usize * std::mem::size_of::<c_ulong>(),
            _ => 0,
        };
        let out = unsafe { std::slice::from_raw_parts(data, bytes.min(1_048_576)).to_vec() };
        unsafe {
            (self.api.XFree)(data.cast());
        }
        Some((format, out))
    }
    pub fn windows(&self) -> Result<String> {
        let mut focused_ancestors = Vec::new();
        let (mut focused, mut revert) = (0, 0);
        unsafe {
            (self.api.XGetInputFocus)(self.display, &mut focused, &mut revert);
        }
        for _ in 0..32 {
            if focused <= 1 || focused == self.root {
                break;
            }
            focused_ancestors.push(focused);
            match self.parent(focused) {
                Ok(parent) => focused = parent,
                Err(_) => break,
            }
        }
        let mut wins = Vec::new();
        if let Some((32, bytes)) = self.property(self.root, c"_NET_CLIENT_LIST") {
            for item in bytes.as_chunks::<{ std::mem::size_of::<c_ulong>() }>().0 {
                let mut arr = [0; std::mem::size_of::<c_ulong>()];
                arr.copy_from_slice(item);
                wins.push(c_ulong::from_ne_bytes(arr));
            }
        } else {
            let (mut root, mut parent, mut children, mut count) = (0, 0, ptr::null_mut(), 0);
            unsafe {
                if (self.api.XQueryTree)(
                    self.display,
                    self.root,
                    &mut root,
                    &mut parent,
                    &mut children,
                    &mut count,
                ) == 0
                {
                    return Err("XQueryTree failed".into());
                }
                if !children.is_null() {
                    wins.extend_from_slice(std::slice::from_raw_parts(
                        children,
                        count.min(4096) as usize,
                    ));
                    (self.api.XFree)(children.cast());
                }
            }
        }
        let mut result = String::from("[");
        for w in wins.into_iter().take(4096) {
            let mut attributes = WindowAttributes::default();
            if unsafe { (self.api.XGetWindowAttributes)(self.display, w, &mut attributes) } == 0 {
                continue;
            }
            let mapped = attributes.map_state == 2;
            let focused = focused_ancestors.contains(&w);
            let Ok((x, y, width, height)) = self.geometry(w) else {
                continue;
            };
            let title = if let Some((8, data)) = self.property(w, c"_NET_WM_NAME") {
                String::from_utf8_lossy(&data)
                    .trim_end_matches('\0')
                    .to_owned()
            } else {
                let mut name = ptr::null_mut();
                unsafe {
                    (self.api.XFetchName)(self.display, w, &mut name);
                    if name.is_null() {
                        String::new()
                    } else {
                        let s = CStr::from_ptr(name).to_string_lossy().into_owned();
                        (self.api.XFree)(name.cast());
                        s
                    }
                }
            };
            let mut hint = ClassHint {
                name: ptr::null_mut(),
                class: ptr::null_mut(),
            };
            let class = unsafe {
                (self.api.XGetClassHint)(self.display, w, &mut hint);
                let s = if hint.class.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(hint.class).to_string_lossy().into_owned()
                };
                if !hint.name.is_null() {
                    (self.api.XFree)(hint.name.cast());
                }
                if !hint.class.is_null() {
                    (self.api.XFree)(hint.class.cast());
                }
                s
            };
            let pid = self.property(w, c"_NET_WM_PID").and_then(|(f, b)| {
                if f == 32 && b.len() >= std::mem::size_of::<c_ulong>() {
                    let mut a = [0; std::mem::size_of::<c_ulong>()];
                    let n = a.len();
                    a.copy_from_slice(&b[..n]);
                    Some(c_ulong::from_ne_bytes(a))
                } else {
                    None
                }
            });
            if result.len() > 1 {
                result.push(',');
            }
            let wine = class.to_ascii_lowercase().contains("wine")
                || class.to_ascii_lowercase().ends_with(".exe");
            result.push_str(&format!("{{\"id\":\"0x{w:x}\",\"title\":{},\"class\":{},\"pid\":{},\"x\":{x},\"y\":{y},\"width\":{width},\"height\":{height},\"wine\":{wine},\"scale\":1.0,\"focused\":{focused},\"mapped\":{mapped}}}",quote(&title),quote(&class),pid.map(|p|p.to_string()).unwrap_or("null".into())));
        }
        result.push(']');
        Ok(result)
    }
    pub fn screenshot(&self, path: &Path, window: Option<&str>) -> Result<String> {
        let w = window
            .map(|w| self.window_id(w))
            .transpose()?
            .unwrap_or(self.root);
        let (_, _, width, height) = self.geometry(w)?;
        if width == 0
            || height == 0
            || width > 16_384
            || height > 16_384
            || u64::from(width) * u64::from(height) > 67_108_864
        {
            return Err("capture dimensions exceed limits".into());
        }
        let image =
            unsafe { (self.api.XGetImage)(self.display, w, 0, 0, width, height, c_ulong::MAX, 2) };
        if image.is_null() {
            return Err("XGetImage failed; the window must be mapped and drawable".into());
        }
        let channel = |pixel: c_ulong, mask: c_ulong| -> u8 {
            if mask == 0 {
                0
            } else {
                let shift = mask.trailing_zeros();
                let value = (pixel & mask) >> shift;
                let max = mask >> shift;
                ((value * 255) / max) as u8
            }
        };
        let result = super::super::png::write_png(path, width, height, |x, y| unsafe {
            let pixel = (self.api.XGetPixel)(image, x as i32, y as i32);
            [
                channel(pixel, (*image).red_mask),
                channel(pixel, (*image).green_mask),
                channel(pixel, (*image).blue_mask),
            ]
        });
        unsafe {
            (self.api.XDestroyImage)(image);
        }
        result?;
        Ok(image_metadata(path, width, height, "x11"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_plan_requires_every_character_before_dispatch() {
        let map = [
            b'a' as c_ulong,
            b'A' as c_ulong,
            b' ' as c_ulong,
            0,
            0xff0d,
            0,
            0xff09,
            0,
        ];
        assert_eq!(
            key_plan(&map, 8, 2, "aA \n\t"),
            Some(vec![
                (8, false),
                (8, true),
                (9, false),
                (10, false),
                (11, false)
            ])
        );
        assert_eq!(key_plan(&map, 8, 2, "aA 中"), None);
        assert_eq!(key_plan(&map, 8, 2, "aA 🙂"), None);
    }

    #[test]
    fn existing_unicode_symbols_need_no_mapping_change() {
        let map = [0x01004e2d, 0, 0x0101f642, 0];
        assert_eq!(
            key_plan(&map, 8, 2, "中🙂中"),
            Some(vec![(8, false), (9, false), (8, false)])
        );
    }

    #[test]
    fn xkb_state_layout_matches_the_public_header() {
        assert_eq!(std::mem::size_of::<KeyboardState>(), 18);
        assert_eq!(std::mem::align_of::<KeyboardState>(), 2);
        assert_eq!(std::mem::offset_of!(KeyboardState, ptr_buttons), 16);
    }
}
