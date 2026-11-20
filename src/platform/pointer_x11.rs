//! Scoped, independently visible XInput2 master pointers.
//!
//! Each handle owns a private Xlib connection. XTEST core button requests use
//! that connection's client pointer, whose XTEST slave belongs to the new
//! master. XIWarpPointer addresses the master explicitly for motion. No real
//! input device is reattached, and the user's client pointer is not changed.
//!
//! Handles are thread-affine; the script runner interleaves their timelines on
//! one thread. Drop is best-effort: callers that need cleanup errors use close.
//! XInput master devices are server-global, not client-owned resources, so an
//! uncatchable process death cannot run this cleanup.

use std::{
    collections::BTreeSet,
    ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void},
    marker::PhantomData,
    ptr,
    rc::Rc,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::cursor::CursorImage;

use super::{Result, dylib::Library};

type Display = *mut c_void;
type Window = c_ulong;
type Cursor = c_ulong;
type ErrorHandler = Option<unsafe extern "C" fn(Display, *mut XErrorEvent) -> c_int>;

const XI_MASTER_POINTER: c_int = 1;
const XI_ADD_MASTER: c_int = 1;
const XI_REMOVE_MASTER: c_int = 2;
const XI_FLOATING: c_int = 2;
const MAX_DEVICES: c_int = 4096;
const MAX_WINDOW_DEPTH: usize = 64;

#[repr(C)]
struct XIAddMasterInfo {
    type_: c_int,
    name: *mut c_char,
    send_core: c_int,
    enable: c_int,
}

#[repr(C)]
struct XIRemoveMasterInfo {
    type_: c_int,
    deviceid: c_int,
    return_mode: c_int,
    return_pointer: c_int,
    return_keyboard: c_int,
}

#[repr(C)]
struct XIDeviceInfo {
    deviceid: c_int,
    name: *mut c_char,
    use_: c_int,
    attachment: c_int,
    enabled: c_int,
    num_classes: c_int,
    classes: *mut *mut c_void,
}

#[repr(C)]
#[derive(Default)]
struct XIButtonState {
    mask_len: c_int,
    mask: *mut u8,
}

#[repr(C)]
#[derive(Default)]
struct XIState {
    base: c_int,
    latched: c_int,
    locked: c_int,
    effective: c_int,
}

#[repr(C)]
struct XcursorImage {
    version: c_uint,
    size: c_uint,
    width: c_uint,
    height: c_uint,
    xhot: c_uint,
    yhot: c_uint,
    delay: c_uint,
    pixels: *mut u32,
}

#[repr(C)]
struct XErrorEvent {
    type_: c_int,
    display: Display,
    resourceid: c_ulong,
    serial: c_ulong,
    error_code: u8,
    request_code: u8,
    minor_code: u8,
}

macro_rules! api {
    ($name:ident { $($function:ident($($arg:ty),*) -> $ret:ty;)+ }) => {
        #[allow(non_snake_case)]
        struct $name { $($function: unsafe extern "C" fn($($arg),*) -> $ret,)+ }
        impl $name {
            unsafe fn load(lib: &Library) -> Result<Self> {
                Ok(Self {
                    $($function: unsafe {
                        lib.symbol(CStr::from_bytes_with_nul_unchecked(
                            concat!(stringify!($function), "\0").as_bytes()
                        ))?
                    },)+
                })
            }
        }
    };
}

api!(Xlib {
    XOpenDisplay(*const c_char) -> Display;
    XCloseDisplay(Display) -> c_int;
    XDefaultRootWindow(Display) -> Window;
    XQueryPointer(Display, Window, *mut Window, *mut Window, *mut c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_uint) -> c_int;
    XSync(Display, c_int) -> c_int;
    XFlush(Display) -> c_int;
    XFree(*mut c_void) -> c_int;
    XFreeCursor(Display, Cursor) -> c_int;
    XSetErrorHandler(ErrorHandler) -> ErrorHandler;
    XGetErrorText(Display, c_int, *mut c_char, c_int) -> c_int;
    XGrabServer(Display) -> c_int;
    XUngrabServer(Display) -> c_int;
});

api!(Xi {
    XIQueryVersion(Display, *mut c_int, *mut c_int) -> c_int;
    XIQueryDevice(Display, c_int, *mut c_int) -> *mut XIDeviceInfo;
    XIFreeDeviceInfo(*mut XIDeviceInfo) -> ();
    XIChangeHierarchy(Display, *mut c_void, c_int) -> c_int;
    XISetClientPointer(Display, Window, c_int) -> c_int;
    XIGetClientPointer(Display, Window, *mut c_int) -> c_int;
    XIQueryPointer(Display, c_int, Window, *mut Window, *mut Window, *mut f64, *mut f64, *mut f64, *mut f64, *mut XIButtonState, *mut XIState, *mut XIState) -> c_int;
    XIWarpPointer(Display, c_int, Window, Window, f64, f64, c_uint, c_uint, f64, f64) -> c_int;
    XIDefineCursor(Display, c_int, Window, Cursor) -> c_int;
    XIUndefineCursor(Display, c_int, Window) -> c_int;
});

api!(Xtest {
    XTestQueryExtension(Display, *mut c_int, *mut c_int, *mut c_int, *mut c_int) -> c_int;
    XTestFakeButtonEvent(Display, c_uint, c_int, c_ulong) -> c_int;
});

api!(Xcursor {
    XcursorSupportsARGB(Display) -> c_int;
    XcursorImageCreate(c_int, c_int) -> *mut XcursorImage;
    XcursorImageDestroy(*mut XcursorImage) -> ();
    XcursorImageLoadCursor(Display, *const XcursorImage) -> Cursor;
});

struct Native {
    x: Xlib,
    xi: Xi,
    test: Xtest,
    cursor: Xcursor,
    _x: Library,
    _xi: Library,
    _test: Library,
    _cursor: Library,
}

impl Native {
    fn load() -> Result<Self> {
        let x = Library::open(c"libX11.so.6")?;
        let xi = Library::open(c"libXi.so.6")?;
        let test = Library::open(c"libXtst.so.6")?;
        let cursor = Library::open(c"libXcursor.so.1")?;
        // SAFETY: all symbols below have their documented Xlib/XI/XTEST/
        // Xcursor ABI, and these libraries outlive the display and its objects.
        unsafe {
            Ok(Self {
                x: Xlib::load(&x)?,
                xi: Xi::load(&xi)?,
                test: Xtest::load(&test)?,
                cursor: Xcursor::load(&cursor)?,
                _x: x,
                _xi: xi,
                _test: test,
                _cursor: cursor,
            })
        }
    }
}

// Xlib's error handler is process-global. Keep each native transaction and its
// scoped handler together; handlers and display owners never cross await points.
// Other desktop adapters are used by the same single-thread script scheduler.
static XLIB_TRANSACTION: Mutex<()> = Mutex::new(());
static WATCHED_DISPLAY: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_HANDLER: AtomicUsize = AtomicUsize::new(0);
static LAST_ERROR: AtomicU64 = AtomicU64::new(0);
static ERROR_RESOURCE: AtomicU64 = AtomicU64::new(0);
static IGNORE_BAD_WINDOW: AtomicBool = AtomicBool::new(false);
static NEXT_NAME: AtomicU64 = AtomicU64::new(1);

unsafe extern "C" fn capture_error(display: Display, event: *mut XErrorEvent) -> c_int {
    if display as usize == WATCHED_DISPLAY.load(Ordering::SeqCst) {
        if !event.is_null() {
            // SAFETY: Xlib passes a valid XErrorEvent for this callback.
            let event = unsafe { &*event };
            if event.error_code == 3 && IGNORE_BAD_WINDOW.load(Ordering::SeqCst) {
                return 0;
            }
            let code = u64::from(event.error_code)
                | (u64::from(event.request_code) << 8)
                | (u64::from(event.minor_code) << 16);
            if LAST_ERROR
                .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                // XID uses c_ulong, which is 32 bits on 32-bit Xlib targets.
                #[allow(clippy::unnecessary_cast)]
                let resource = event.resourceid as u64;
                ERROR_RESOURCE.store(resource, Ordering::SeqCst);
            }
        }
        return 0;
    }
    let previous = PREVIOUS_HANDLER.load(Ordering::SeqCst);
    if previous != 0 && previous != capture_error as *const () as usize {
        // SAFETY: this address was returned by XSetErrorHandler with this ABI.
        let previous: unsafe extern "C" fn(Display, *mut XErrorEvent) -> c_int =
            unsafe { std::mem::transmute(previous) };
        unsafe { previous(display, event) }
    } else {
        0
    }
}

struct ErrorTrap<'a> {
    native: &'a Native,
    display: Display,
    previous: ErrorHandler,
}

impl<'a> ErrorTrap<'a> {
    fn begin(native: &'a Native, display: Display, tolerate_bad_window: bool) -> Self {
        WATCHED_DISPLAY.store(display as usize, Ordering::SeqCst);
        LAST_ERROR.store(0, Ordering::SeqCst);
        ERROR_RESOURCE.store(0, Ordering::SeqCst);
        IGNORE_BAD_WINDOW.store(tolerate_bad_window, Ordering::SeqCst);
        let previous = unsafe { (native.x.XSetErrorHandler)(Some(capture_error)) };
        PREVIOUS_HANDLER.store(
            previous.map_or(0, |f| f as *const () as usize),
            Ordering::SeqCst,
        );
        Self {
            native,
            display,
            previous,
        }
    }

    fn finish(self, context: &str) -> Result<()> {
        unsafe { (self.native.x.XSync)(self.display, 0) };
        let error = LAST_ERROR.load(Ordering::SeqCst);
        let code = (error & 255) as c_int;
        if code == 0 {
            return Ok(());
        }
        let mut buffer = [0 as c_char; 256];
        unsafe {
            (self.native.x.XGetErrorText)(
                self.display,
                code,
                buffer.as_mut_ptr(),
                buffer.len() as c_int,
            );
        }
        let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
        Err(format!(
            "X11 {context}: {text} (request {}, minor {}, resource {})",
            (error >> 8) & 255,
            (error >> 16) & 255,
            ERROR_RESOURCE.load(Ordering::SeqCst),
        ))
    }
}

impl Drop for ErrorTrap<'_> {
    fn drop(&mut self) {
        // Drain requests even if a Rust panic unwinds the operation.
        unsafe {
            (self.native.x.XSync)(self.display, 0);
            (self.native.x.XSetErrorHandler)(self.previous);
        }
        WATCHED_DISPLAY.store(0, Ordering::SeqCst);
        PREVIOUS_HANDLER.store(0, Ordering::SeqCst);
        IGNORE_BAD_WINDOW.store(false, Ordering::SeqCst);
    }
}

struct ServerGrab<'a> {
    native: &'a Native,
    display: Display,
}

impl Drop for ServerGrab<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.native.x.XUngrabServer)(self.display);
            (self.native.x.XFlush)(self.display);
        }
    }
}

pub(crate) struct X11Pointer {
    native: Native,
    display: Display,
    root: Window,
    device_id: c_int,
    master_name: String,
    independent: bool,
    cursor: Cursor,
    cursor_windows: BTreeSet<Window>,
    held_buttons: BTreeSet<c_uint>,
    closed: bool,
    _thread_affine: PhantomData<Rc<()>>,
}

impl X11Pointer {
    /// Create a real master pointer and its paired master keyboard. The new
    /// pointer starts at the user's current position without moving that user.
    pub(crate) fn create(name: &str, image: &CursorImage) -> Result<Self> {
        image.validate()?;
        if name.is_empty() || name.len() > 128 || name.contains('\0') {
            return Err("mouse name must contain 1..128 non-NUL bytes".into());
        }
        let (mut pointer, position) = Self::open()?;
        let serial = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = CString::new(format!(
            "zuse-{}-{time:x}-{serial}-{name}",
            std::process::id()
        ))
        .map_err(|_| "mouse name contains a NUL byte")?;
        pointer.master_name = format!("{} pointer", name.to_string_lossy());
        pointer.independent = true;
        pointer.device_id = 0;
        let mut add = XIAddMasterInfo {
            type_: XI_ADD_MASTER,
            name: name.as_ptr().cast_mut(),
            send_core: 1,
            enable: 1,
        };
        // Store the exact unique name before changing hierarchy. Drop can find
        // and remove the new pair even if a later query or cursor load fails.
        let status = pointer.checked("create independent pointer", false, |n, d| unsafe {
            (n.xi.XIChangeHierarchy)(d, ptr::from_mut(&mut add).cast(), 1)
        })?;
        if status != 0 {
            return Err(format!(
                "XInput refused independent pointer creation ({status})"
            ));
        }
        pointer.device_id = pointer
            .find_named_master()?
            .ok_or("new XInput master pointer was not found")?;
        pointer.select_client_pointer()?;
        pointer.move_to(position.0, position.1)?;
        pointer.set_cursor(image)?;
        Ok(pointer)
    }

    /// Explicitly share the user's master pointer and temporarily style it.
    /// Normal application cursors resume after XIUndefineCursor at close.
    /// XInput cannot query an existing per-device override, so a pre-existing
    /// third-party per-device override cannot be restored by this API.
    pub(crate) fn shared(image: &CursorImage) -> Result<Self> {
        image.validate()?;
        let (mut pointer, _) = Self::open()?;
        pointer.set_cursor(image)?;
        Ok(pointer)
    }

    fn open() -> Result<(Self, (i32, i32))> {
        let native = Native::load()?;
        let display = unsafe { (native.x.XOpenDisplay)(ptr::null()) };
        if display.is_null() {
            return Err(
                "independent/custom mice require an accessible X11 DISPLAY and XInput2".into(),
            );
        }
        let root = unsafe { (native.x.XDefaultRootWindow)(display) };
        let mut pointer = Self {
            native,
            display,
            root,
            device_id: 0,
            master_name: String::new(),
            independent: false,
            cursor: 0,
            cursor_windows: BTreeSet::new(),
            held_buttons: BTreeSet::new(),
            closed: false,
            _thread_affine: PhantomData,
        };
        pointer.checked("query pointer extensions", false, |n, d| unsafe {
            let (mut major, mut minor) = (2, 0);
            let status = (n.xi.XIQueryVersion)(d, &mut major, &mut minor);
            if status != 0 || major < 2 {
                return Err("independent/custom mice require XInput 2.0 or later".to_string());
            }
            let (mut event, mut error, mut major, mut minor) = (0, 0, 0, 0);
            if (n.test.XTestQueryExtension)(d, &mut event, &mut error, &mut major, &mut minor) == 0
            {
                return Err("independent/custom mice require the XTEST extension".to_string());
            }
            if (n.cursor.XcursorSupportsARGB)(d) == 0 {
                return Err("this X11 server does not support ARGB mouse cursors".to_string());
            }
            Ok(())
        })??;
        // A fresh connection may have no client pointer yet. XQueryPointer
        // asks the server to choose the normal default before XIGetClientPointer.
        let (root, device_id, position) =
            pointer.checked("query user pointer", false, |n, d| unsafe {
                let (mut root, mut child, mut x, mut y, mut wx, mut wy, mut mask) =
                    (0, 0, 0, 0, 0, 0, 0);
                let same_screen = (n.x.XQueryPointer)(
                    d,
                    pointer.root,
                    &mut root,
                    &mut child,
                    &mut x,
                    &mut y,
                    &mut wx,
                    &mut wy,
                    &mut mask,
                );
                if same_screen == 0 && root != 0 {
                    (n.x.XQueryPointer)(
                        d, root, &mut root, &mut child, &mut x, &mut y, &mut wx, &mut wy, &mut mask,
                    );
                }
                let mut device = 0;
                if root == 0 || (n.xi.XIGetClientPointer)(d, 0, &mut device) == 0 || device <= 0 {
                    return Err("could not identify the user's XInput master pointer".to_string());
                }
                Ok((root, device, (x, y)))
            })??;
        pointer.root = root;
        pointer.device_id = device_id;
        pointer.master_name = pointer
            .checked("read user pointer identity", false, |n, d| {
                Self::master_name_raw(n, d, device_id)
            })??
            .ok_or("user master pointer disappeared")?;
        Ok((pointer, position))
    }

    pub(crate) fn device_id(&self) -> i32 {
        self.device_id
    }
    pub(crate) fn is_independent(&self) -> bool {
        self.independent
    }

    pub(crate) fn position(&self) -> Result<(i32, i32)> {
        self.ensure_open()?;
        self.device_checked("query mouse position", false, |n, d| {
            Self::query_raw(n, d, self.device_id, self.root).map(|q| q.0)
        })
    }

    pub(crate) fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        self.ensure_open()?;
        self.device_checked("move mouse", false, |n, d| unsafe {
            (n.xi.XIWarpPointer)(
                d,
                self.device_id,
                0,
                self.root,
                0.0,
                0.0,
                0,
                0,
                f64::from(x),
                f64::from(y),
            );
            Ok(())
        })?;
        self.refresh_cursor(false)
    }

    pub(crate) fn button(&mut self, name: &str, action: &str) -> Result<()> {
        let code = match name {
            "left" => 1,
            "middle" => 2,
            "right" => 3,
            "back" => 8,
            "forward" => 9,
            _ => return Err("button must be left, middle, right, back or forward".into()),
        };
        match action {
            "down" => self.emit_button(code, true),
            "up" => self.emit_button(code, false),
            "click" => {
                self.emit_button(code, true)?;
                self.emit_button(code, false)
            }
            _ => Err("button action must be down, up or click".into()),
        }
    }

    pub(crate) fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        if dx.unsigned_abs() > 1000 || dy.unsigned_abs() > 1000 {
            return Err("scroll is limited to 1000 notches per axis".into());
        }
        self.ensure_open()?;
        for (amount, positive, negative) in [(dy, 5, 4), (dx, 7, 6)] {
            let button = if amount < 0 { negative } else { positive };
            for _ in 0..amount.unsigned_abs() {
                self.emit_button(button, true)?;
                self.emit_button(button, false)?;
            }
        }
        Ok(())
    }

    pub(crate) fn set_cursor(&mut self, image: &CursorImage) -> Result<()> {
        self.ensure_open()?;
        image.validate()?;
        let cursor = self.checked("load mouse appearance", false, |n, d| unsafe {
            let native_image =
                (n.cursor.XcursorImageCreate)(image.width as c_int, image.height as c_int);
            if native_image.is_null() {
                return Err("could not allocate Xcursor image".to_string());
            }
            (*native_image).xhot = image.hotspot_x;
            (*native_image).yhot = image.hotspot_y;
            ptr::copy_nonoverlapping(
                image.pixels.as_ptr(),
                (*native_image).pixels,
                image.pixels.len(),
            );
            let cursor = (n.cursor.XcursorImageLoadCursor)(d, native_image);
            (n.cursor.XcursorImageDestroy)(native_image);
            if cursor == 0 {
                Err("could not load X11 mouse cursor".to_string())
            } else {
                Ok(cursor)
            }
        })??;
        let previous = std::mem::replace(&mut self.cursor, cursor);
        let result = self.refresh_cursor(true);
        if previous != 0 {
            let free = self.checked("free previous mouse appearance", false, |n, d| unsafe {
                (n.x.XFreeCursor)(d, previous);
            });
            if result.is_ok() {
                free?;
            }
        }
        result
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed || self.display.is_null() || self.device_id <= 0 {
            Err("mouse is closed or its native device is unavailable".into())
        } else {
            Ok(())
        }
    }

    fn checked<T>(
        &self,
        context: &str,
        tolerate_bad_window: bool,
        run: impl FnOnce(&Native, Display) -> T,
    ) -> Result<T> {
        if self.display.is_null() {
            return Err("X11 mouse connection is closed".into());
        }
        let _lock = XLIB_TRANSACTION
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let trap = ErrorTrap::begin(&self.native, self.display, tolerate_bad_window);
        let result = run(&self.native, self.display);
        trap.finish(context)?;
        Ok(result)
    }

    fn device_checked<T>(
        &self,
        context: &str,
        tolerate_bad_window: bool,
        run: impl FnOnce(&Native, Display) -> Result<T>,
    ) -> Result<T> {
        self.checked(context, tolerate_bad_window, |n, d| unsafe {
            // Device IDs can be reused after removal. Keep name validation
            // and the operation atomic with respect to other X11 clients.
            (n.x.XGrabServer)(d);
            let _grab = ServerGrab {
                native: n,
                display: d,
            };
            if Self::master_name_raw(n, d, self.device_id)?.as_deref()
                != Some(self.master_name.as_str())
            {
                return Err(
                    "the mouse's XInput master has disappeared; input was not redirected"
                        .to_string(),
                );
            }
            run(n, d)
        })?
    }

    fn select_client_pointer(&self) -> Result<()> {
        self.device_checked("select independent client pointer", false, |n, d| unsafe {
            (n.xi.XISetClientPointer)(d, 0, self.device_id);
            let mut actual = 0;
            if (n.xi.XIGetClientPointer)(d, 0, &mut actual) == 0 || actual != self.device_id {
                return Err("X11 client pointer did not select the requested mouse".to_string());
            }
            Ok(())
        })
    }

    fn emit_button(&mut self, code: c_uint, down: bool) -> Result<()> {
        self.ensure_open()?;
        if down {
            self.held_buttons.insert(code);
        }
        self.device_checked("send mouse button", false, |n, d| unsafe {
            (n.xi.XISetClientPointer)(d, 0, self.device_id);
            let mut actual = 0;
            if (n.xi.XIGetClientPointer)(d, 0, &mut actual) == 0 || actual != self.device_id {
                return Err(
                    "the mouse's client pointer is unavailable; input was not redirected"
                        .to_string(),
                );
            }
            if (n.test.XTestFakeButtonEvent)(d, code, c_int::from(down), 0) == 0 {
                return Err("XTEST refused the mouse button event".to_string());
            }
            Ok(())
        })?;
        if !down {
            self.held_buttons.remove(&code);
        }
        Ok(())
    }

    fn master_name_raw(n: &Native, display: Display, device_id: c_int) -> Result<Option<String>> {
        let mut count = 0;
        let devices = unsafe { (n.xi.XIQueryDevice)(display, device_id, &mut count) };
        if devices.is_null() {
            return Ok(None);
        }
        let result = if !(0..=MAX_DEVICES).contains(&count) {
            Err("invalid XInput device count".into())
        } else {
            let devices_slice = unsafe { std::slice::from_raw_parts(devices, count as usize) };
            Ok(devices_slice
                .iter()
                .find(|d| {
                    d.deviceid == device_id && d.use_ == XI_MASTER_POINTER && !d.name.is_null()
                })
                .map(|d| {
                    unsafe { CStr::from_ptr(d.name) }
                        .to_string_lossy()
                        .into_owned()
                }))
        };
        unsafe { (n.xi.XIFreeDeviceInfo)(devices) };
        result
    }

    fn find_named_master(&self) -> Result<Option<c_int>> {
        self.checked("find owned master pointer", false, |n, d| {
            let mut count = 0;
            let devices = unsafe { (n.xi.XIQueryDevice)(d, 0, &mut count) };
            if devices.is_null() {
                return Err("could not list XInput devices".to_string());
            }
            let result = if !(0..=MAX_DEVICES).contains(&count) {
                Err("invalid XInput device count".into())
            } else {
                let slice = unsafe { std::slice::from_raw_parts(devices, count as usize) };
                Ok(slice
                    .iter()
                    .find(|device| {
                        device.use_ == XI_MASTER_POINTER
                            && !device.name.is_null()
                            && unsafe { CStr::from_ptr(device.name) }.to_bytes()
                                == self.master_name.as_bytes()
                    })
                    .map(|device| device.deviceid))
            };
            unsafe { (n.xi.XIFreeDeviceInfo)(devices) };
            result
        })?
    }

    fn query_raw(
        n: &Native,
        display: Display,
        device: c_int,
        window: Window,
    ) -> Result<((i32, i32), Window)> {
        let (mut root, mut child, mut x, mut y, mut wx, mut wy) = (0, 0, 0.0, 0.0, 0.0, 0.0);
        let mut buttons = XIButtonState::default();
        let mut modifiers = XIState::default();
        let mut group = XIState::default();
        let ok = unsafe {
            (n.xi.XIQueryPointer)(
                display,
                device,
                window,
                &mut root,
                &mut child,
                &mut x,
                &mut y,
                &mut wx,
                &mut wy,
                &mut buttons,
                &mut modifiers,
                &mut group,
            )
        };
        if !buttons.mask.is_null() {
            unsafe {
                (n.x.XFree)(buttons.mask.cast());
            }
        }
        if ok == 0 {
            return Err("mouse is not on this X11 screen or the window is unavailable".into());
        }
        if !x.is_finite()
            || !y.is_finite()
            || x < f64::from(i32::MIN)
            || x > f64::from(i32::MAX)
            || y < f64::from(i32::MIN)
            || y > f64::from(i32::MAX)
        {
            return Err("XInput returned an invalid mouse position".into());
        }
        Ok(((x.round() as i32, y.round() as i32), child))
    }

    fn refresh_cursor(&mut self, force: bool) -> Result<()> {
        if self.cursor == 0 {
            return Ok(());
        }
        let next = self.device_checked("query mouse window path", true, |n, d| {
            let mut windows = BTreeSet::new();
            let mut window = self.root;
            for _ in 0..MAX_WINDOW_DEPTH {
                if window == 0 {
                    return Ok(windows);
                }
                if !windows.insert(window) {
                    return Err("cyclic X11 window tree".to_string());
                }
                match Self::query_raw(n, d, self.device_id, window) {
                    Ok((_, child)) => window = child,
                    // Windows can disappear as pointer motion closes menus.
                    // Retain the valid prefix and refresh on the next move.
                    Err(_) => return Ok(windows),
                }
            }
            Err("X11 mouse window path exceeds 64 levels".to_string())
        })?;
        let old = self.cursor_windows.clone();
        // Retain both sets until success so any partially applied overrides are
        // still removed when a fallible cursor update is unwound.
        self.cursor_windows.extend(next.iter().copied());
        self.device_checked("apply mouse appearance", true, |n, d| unsafe {
            for &window in old.difference(&next) {
                (n.xi.XIUndefineCursor)(d, self.device_id, window);
            }
            for &window in &next {
                if force || !old.contains(&window) {
                    (n.xi.XIDefineCursor)(d, self.device_id, window, self.cursor);
                }
            }
            Ok(())
        })?;
        self.cursor_windows = next;
        Ok(())
    }

    /// Release held synthetic buttons, remove this handle's cursor overrides,
    /// and remove only its exact owned master pair. Repeated close is harmless.
    pub(crate) fn close(&mut self) -> Result<()> {
        if self.closed || self.display.is_null() {
            return Ok(());
        }
        let mut errors = Vec::new();
        let found = if self.master_name.is_empty() {
            Ok(None)
        } else {
            self.find_named_master()
        };
        let mut removed = false;
        match found {
            Ok(Some(device)) => {
                self.device_id = device;
                let buttons: Vec<_> = self.held_buttons.iter().copied().collect();
                for button in buttons {
                    if let Err(error) = self.emit_button(button, false) {
                        errors.push(error);
                    }
                }
                if let Err(error) =
                    self.device_checked("restore mouse appearance", true, |n, d| unsafe {
                        for &window in &self.cursor_windows {
                            (n.xi.XIUndefineCursor)(d, device, window);
                        }
                        Ok(())
                    })
                {
                    errors.push(error);
                } else {
                    self.cursor_windows.clear();
                }
                if self.independent {
                    let mut remove = XIRemoveMasterInfo {
                        type_: XI_REMOVE_MASTER,
                        deviceid: device,
                        return_mode: XI_FLOATING,
                        return_pointer: 0,
                        return_keyboard: 0,
                    };
                    match self.device_checked("remove independent pointer", false, |n, d| unsafe {
                        Ok((n.xi.XIChangeHierarchy)(
                            d,
                            ptr::from_mut(&mut remove).cast(),
                            1,
                        ))
                    }) {
                        Ok(0) => match self.find_named_master() {
                            Ok(None) => removed = true,
                            Ok(Some(_)) => {
                                errors.push("XInput master pointer remained after cleanup".into())
                            }
                            Err(error) => errors.push(error),
                        },
                        Ok(status) => {
                            errors.push(format!("XInput refused pointer cleanup ({status})"))
                        }
                        Err(error) => errors.push(error),
                    }
                } else {
                    removed = self.cursor_windows.is_empty() && self.held_buttons.is_empty();
                }
            }
            Ok(None) => removed = true,
            Err(error) => errors.push(error),
        }
        if self.cursor != 0 {
            if let Err(error) = self.checked("free mouse cursor", false, |n, d| unsafe {
                (n.x.XFreeCursor)(d, self.cursor);
            }) {
                errors.push(error);
            }
            self.cursor = 0;
        }
        if removed {
            self.closed = true;
            self.device_id = 0;
            self.held_buttons.clear();
            self.cursor_windows.clear();
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl Drop for X11Pointer {
    fn drop(&mut self) {
        let _ = self.close();
        if !self.display.is_null() {
            let _lock = XLIB_TRANSACTION
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            unsafe {
                (self.native.x.XCloseDisplay)(self.display);
            }
            self.display = ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_names(pointer: &X11Pointer) -> BTreeSet<String> {
        pointer
            .checked("test device inventory", false, |n, d| {
                let mut count = 0;
                let devices = unsafe { (n.xi.XIQueryDevice)(d, 0, &mut count) };
                assert!(!devices.is_null() && (0..=MAX_DEVICES).contains(&count));
                let names = unsafe { std::slice::from_raw_parts(devices, count as usize) }
                    .iter()
                    .map(|device| {
                        unsafe { CStr::from_ptr(device.name) }
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                unsafe { (n.xi.XIFreeDeviceInfo)(devices) };
                names
            })
            .unwrap()
    }

    fn image(color: u32) -> CursorImage {
        CursorImage {
            width: 12,
            height: 12,
            hotspot_x: 1,
            hotspot_y: 1,
            pixels: vec![color; 144],
        }
    }

    #[repr(C)]
    struct FixesCursorImage {
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        xhot: u16,
        yhot: u16,
        serial: c_ulong,
        pixels: *mut c_ulong,
        atom: c_ulong,
        name: *mut c_char,
    }

    fn cursor_pixels(pointer: &X11Pointer) -> (u16, u16, Vec<u32>) {
        let lib = Library::open(c"libXfixes.so.3").unwrap();
        let query: unsafe extern "C" fn(Display) -> *mut FixesCursorImage =
            unsafe { lib.symbol(c"XFixesGetCursorImage") }.unwrap();
        pointer
            .checked("test cursor pixels", false, |n, d| unsafe {
                let cursor = query(d);
                assert!(!cursor.is_null());
                let image = &*cursor;
                let pixels = std::slice::from_raw_parts(
                    image.pixels,
                    usize::from(image.width) * usize::from(image.height),
                )
                .iter()
                .map(|&pixel| pixel as u32)
                .collect();
                let result = (image.width, image.height, pixels);
                (n.x.XFree)(cursor.cast());
                result
            })
            .unwrap()
    }

    struct TestWindow<'a> {
        pointer: &'a X11Pointer,
        window: Window,
        cursor: Cursor,
    }
    impl TestWindow<'_> {
        fn event_types(&self) -> Vec<c_int> {
            self.pointer
                .checked("test event delivery", false, |n, d| unsafe {
                    let pending: unsafe extern "C" fn(Display) -> c_int =
                        n._x.symbol(c"XPending").unwrap();
                    let next: unsafe extern "C" fn(Display, *mut c_void) -> c_int =
                        n._x.symbol(c"XNextEvent").unwrap();
                    (n.x.XSync)(d, 0);
                    let mut result = Vec::new();
                    while pending(d) > 0 {
                        let mut event = [0 as std::ffi::c_long; 24];
                        next(d, event.as_mut_ptr().cast());
                        result.push(*event.as_ptr().cast::<c_int>());
                    }
                    result
                })
                .unwrap()
        }
    }
    impl Drop for TestWindow<'_> {
        fn drop(&mut self) {
            let _ = self
                .pointer
                .checked("destroy test window", false, |n, d| unsafe {
                    let destroy: unsafe extern "C" fn(Display, Window) -> c_int =
                        n._x.symbol(c"XDestroyWindow").unwrap();
                    destroy(d, self.window);
                    (n.x.XFreeCursor)(d, self.cursor);
                });
        }
    }

    fn test_window(pointer: &X11Pointer) -> TestWindow<'_> {
        let (window, cursor) = pointer
            .checked("create test window", false, |n, d| unsafe {
                let create: unsafe extern "C" fn(
                    Display,
                    Window,
                    c_int,
                    c_int,
                    c_uint,
                    c_uint,
                    c_uint,
                    c_ulong,
                    c_ulong,
                ) -> Window = n._x.symbol(c"XCreateSimpleWindow").unwrap();
                let select: unsafe extern "C" fn(Display, Window, std::ffi::c_long) -> c_int =
                    n._x.symbol(c"XSelectInput").unwrap();
                let map: unsafe extern "C" fn(Display, Window) -> c_int =
                    n._x.symbol(c"XMapWindow").unwrap();
                let font_cursor: unsafe extern "C" fn(Display, c_uint) -> Cursor =
                    n._x.symbol(c"XCreateFontCursor").unwrap();
                let define: unsafe extern "C" fn(Display, Window, Cursor) -> c_int =
                    n._x.symbol(c"XDefineCursor").unwrap();
                let window = create(d, pointer.root, 10, 10, 250, 200, 0, 0, 0xffffff);
                let cursor = font_cursor(d, 152); // XC_xterm: application's ordinary window cursor.
                select(d, window, (1 << 2) | (1 << 3) | (1 << 6));
                define(d, window, cursor);
                map(d, window);
                (window, cursor)
            })
            .unwrap();
        TestWindow {
            pointer,
            window,
            cursor,
        }
    }

    /// Opt-in only: launch a disposable Xvfb and set both DISPLAY and
    /// ZUSE_TEST_PRIVATE_X11=1. This test intentionally creates/moves mice.
    #[test]
    fn private_x11_pointer_lifetime_events_and_cursor_isolation() {
        if std::env::var("ZUSE_TEST_PRIVATE_X11").as_deref() != Ok("1") {
            return;
        }
        let display = std::env::var("DISPLAY").expect("private X11 test requires DISPLAY");
        assert!(
            !display.is_empty() && display != ":0" && display != ":0.0",
            "never run native pointer tests on the user's desktop"
        );
        let (base, original_position) = X11Pointer::open().unwrap();
        let original_devices = device_names(&base);
        let app = test_window(&base);
        let original_cursor = cursor_pixels(&base);
        let green = image(0xff00ff33);
        let red = image(0xffff3344);
        {
            let mut a = X11Pointer::create("native-a", &green).unwrap();
            let mut b = X11Pointer::create("native-b", &red).unwrap();
            assert!(a.is_independent() && b.is_independent());
            assert_ne!(a.device_id(), b.device_id());
            assert_ne!(a.device_id(), base.device_id());
            assert_eq!(a.position().unwrap(), original_position);
            assert_eq!(device_names(&base).len(), original_devices.len() + 8);
            a.move_to(60, 70).unwrap();
            b.move_to(310, 360).unwrap();
            assert_eq!(a.position().unwrap(), (60, 70));
            assert_eq!(b.position().unwrap(), (310, 360));
            assert_eq!(base.position().unwrap(), original_position);
            assert_eq!(cursor_pixels(&a), (12, 12, green.pixels.clone()));
            assert_eq!(cursor_pixels(&b), (12, 12, red.pixels.clone()));
            assert_eq!(cursor_pixels(&base), original_cursor);
            a.button("left", "click").unwrap();
            a.scroll(1, -1).unwrap();
            let events = app.event_types();
            assert!(
                events.contains(&4) && events.contains(&5),
                "ordinary core application did not receive click: {events:?}"
            );
            a.set_cursor(&red).unwrap();
            assert_eq!(cursor_pixels(&a), (12, 12, red.pixels.clone()));
            a.close().unwrap();
            a.close().unwrap();
            assert!(a.move_to(0, 0).is_err());
            assert_eq!(device_names(&base).len(), original_devices.len() + 4);
            assert_eq!(base.position().unwrap(), original_position);
        }
        assert_eq!(device_names(&base), original_devices);
        app.event_types();
        // Returning an error with a held button must still synthesize release
        // and remove the master pair when its local variable is dropped.
        let failure = || -> Result<()> {
            let mut pointer = X11Pointer::create("scope-error", &green)?;
            pointer.move_to(70, 80)?;
            pointer.button("left", "down")?;
            pointer.button("invalid", "click")
        };
        assert!(failure().is_err());
        let events = app.event_types();
        assert!(
            events.contains(&4) && events.contains(&5),
            "scope cleanup failed to release button: {events:?}"
        );
        assert_eq!(device_names(&base), original_devices);
        // If another client removes our master, neither movement nor buttons
        // may silently fall back to the real user's client pointer.
        {
            let mut removed = X11Pointer::create("external-removal", &green).unwrap();
            let mut change = XIRemoveMasterInfo {
                type_: XI_REMOVE_MASTER,
                deviceid: removed.device_id(),
                return_mode: XI_FLOATING,
                return_pointer: 0,
                return_keyboard: 0,
            };
            base.checked("test external removal", false, |n, d| unsafe {
                (n.xi.XIChangeHierarchy)(d, ptr::from_mut(&mut change).cast(), 1)
            })
            .unwrap();
            assert!(removed.move_to(1, 2).is_err());
            assert!(removed.button("left", "down").is_err());
            assert_eq!(base.position().unwrap(), original_position);
        }
        assert_eq!(device_names(&base), original_devices);
        {
            let mut shared = X11Pointer::shared(&red).unwrap();
            assert!(!shared.is_independent());
            assert_eq!(shared.device_id(), base.device_id());
            assert_eq!(device_names(&base), original_devices);
            assert_eq!(cursor_pixels(&base), (12, 12, red.pixels.clone()));
            shared
                .move_to(original_position.0 + 1, original_position.1 + 1)
                .unwrap();
            assert_eq!(
                base.position().unwrap(),
                (original_position.0 + 1, original_position.1 + 1)
            );
            shared
                .move_to(original_position.0, original_position.1)
                .unwrap();
            shared.button("left", "click").unwrap();
            shared.close().unwrap();
        }
        assert_eq!(device_names(&base), original_devices);
        assert_eq!(base.position().unwrap(), original_position);
        assert_eq!(cursor_pixels(&base), original_cursor);
    }

    #[cfg(feature = "std")]
    fn receipt_events(receipt: &str) -> Vec<(i64, String)> {
        crate::json::with_value(receipt, |root| {
            root.get("events")
                .unwrap()
                .elements()
                .map(|event| {
                    Ok((
                        event.get("mouse").unwrap().integer().unwrap(),
                        crate::json::text(event.get("event").unwrap())?,
                    ))
                })
                .collect()
        })
        .unwrap()
    }

    #[cfg(feature = "std")]
    fn remove_named_test_master(pointer: &X11Pointer, suffix: &str) -> bool {
        pointer
            .checked("test remove script master", false, |n, d| unsafe {
                let mut count = 0;
                let devices = (n.xi.XIQueryDevice)(d, 0, &mut count);
                assert!(!devices.is_null() && (0..=MAX_DEVICES).contains(&count));
                let found = std::slice::from_raw_parts(devices, count as usize)
                    .iter()
                    .find(|device| {
                        device.use_ == XI_MASTER_POINTER
                            && CStr::from_ptr(device.name)
                                .to_string_lossy()
                                .ends_with(suffix)
                    })
                    .map(|device| device.deviceid);
                (n.xi.XIFreeDeviceInfo)(devices);
                if let Some(device) = found {
                    let mut remove = XIRemoveMasterInfo {
                        type_: XI_REMOVE_MASTER,
                        deviceid: device,
                        return_mode: XI_FLOATING,
                        return_pointer: 0,
                        return_keyboard: 0,
                    };
                    assert_eq!(
                        (n.xi.XIChangeHierarchy)(d, ptr::from_mut(&mut remove).cast(), 1),
                        0
                    );
                    true
                } else {
                    false
                }
            })
            .unwrap()
    }

    /// Exercises the actual compiler, cooperative scheduler and native driver
    /// together, without touching any shared desktop or accessibility service.
    #[cfg(feature = "std")]
    #[test]
    fn private_x11_script_parallel_scopes_errors_and_cancellation() {
        if std::env::var("ZUSE_TEST_PRIVATE_X11").as_deref() != Ok("1") {
            return;
        }
        let display = std::env::var("DISPLAY").expect("private X11 test requires DISPLAY");
        assert!(!display.is_empty() && display != ":0" && display != ":0.0");
        let (base, original_position) = X11Pointer::open().unwrap();
        let original_devices = device_names(&base);
        let app = test_window(&base);
        let original_cursor = cursor_pixels(&base);
        let options = crate::runtime::Options::default();
        let movement = |group: &str| {
            format!(
                r#"
            let a = mouse(name="script-timing-a");
            let b = mouse(name="script-timing-b");
            {group} {{
                a.move(x=80,y=90,duration=250,steps=20);
                b.move(x=310,y=360,duration=250,steps=20);
            }}
        "#
            )
        };
        let parallel = crate::script::execute(&movement("parallel"), options).unwrap();
        let serial = crate::script::execute(&movement("sequence"), options).unwrap();
        assert_eq!(
            crate::json::field(&parallel, "status").unwrap(),
            "completed",
            "{parallel}"
        );
        assert_eq!(
            crate::json::field(&serial, "status").unwrap(),
            "completed",
            "{serial}"
        );
        let elapsed = |receipt: &str| {
            crate::json::with_value(receipt, |root| {
                Ok(root.get("elapsed_ms").unwrap().integer().unwrap())
            })
            .unwrap()
        };
        let parallel_ms = elapsed(&parallel);
        let serial_ms = elapsed(&serial);
        assert!(
            parallel_ms >= 230 && serial_ms >= 470 && serial_ms - parallel_ms >= 100,
            "expected overlapping independent motions: parallel={parallel_ms}ms serial={serial_ms}ms"
        );
        for receipt in [&parallel, &serial] {
            let events = receipt_events(receipt);
            assert_eq!(
                events.iter().filter(|(_, kind)| kind == "create").count(),
                2
            );
            assert_eq!(events.iter().filter(|(_, kind)| kind == "move").count(), 2);
            assert_eq!(events.iter().filter(|(_, kind)| kind == "close").count(), 2);
        }
        assert_eq!(device_names(&base), original_devices);
        assert_eq!(base.position().unwrap(), original_position);
        assert_eq!(cursor_pixels(&base), original_cursor);

        let scoped = r#"
            fn local(owner) {
                let inner = mouse(name="script-function");
                inner.position();
                owner.position();
                wait(ms=20);
            }
            let outer = mouse(name="script-outer");
            {
                let nested = mouse(name="script-block");
                nested.position();
                wait(ms=20);
            }
            outer.position();
            wait(ms=20);
            local(outer);
            outer.position();
            wait(ms=20);
        "#;
        let mut saw_block = false;
        let mut saw_function = false;
        let mut saw_outer_only = false;
        let scope_receipt = crate::script::execute_with_cancel(scoped, options, || {
            let names = device_names(&base);
            let has = |suffix: &str| names.iter().any(|name| name.ends_with(suffix));
            let outer = has("-script-outer pointer");
            let block = has("-script-block pointer");
            let function = has("-script-function pointer");
            saw_block |= outer && block;
            saw_function |= outer && function && !block;
            saw_outer_only |= outer && !block && !function;
            false
        })
        .unwrap();
        assert_eq!(
            crate::json::field(&scope_receipt, "status").unwrap(),
            "completed",
            "{scope_receipt}"
        );
        assert!(
            saw_block && saw_function && saw_outer_only,
            "native variable lifetimes were not observed at scope boundaries"
        );
        let events = receipt_events(&scope_receipt);
        let created: Vec<_> = events
            .iter()
            .filter(|(_, kind)| kind == "create")
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(created.len(), 3);
        let close_index = |id: i64| {
            events
                .iter()
                .position(|event| event == &(id, "close".into()))
                .unwrap()
        };
        let outer_positions: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| **event == (created[0], "position".into()))
            .map(|(index, _)| index)
            .collect();
        assert!(
            close_index(created[1]) < outer_positions[0],
            "block mouse outlived its block"
        );
        assert!(
            close_index(created[2]) < *outer_positions.last().unwrap(),
            "function mouse outlived its call"
        );
        assert_eq!(events.last().unwrap(), &(created[0], "close".into()));
        assert_eq!(device_names(&base), original_devices);

        app.event_types();
        let started = std::time::Instant::now();
        let cancelled = crate::script::execute_with_cancel(
            r#"
            let p = mouse(name="script-cancelled");
            p.move(x=70,y=80,duration=0);
            p.down();
            wait(ms=1000);
        "#,
            options,
            || started.elapsed() >= std::time::Duration::from_millis(60),
        )
        .unwrap();
        assert_eq!(
            crate::json::field(&cancelled, "status").unwrap(),
            "failed",
            "{cancelled}"
        );
        assert_eq!(
            crate::json::field(&cancelled, "error").unwrap(),
            "script cancelled"
        );
        let events = app.event_types();
        assert!(
            events.contains(&4) && events.contains(&5),
            "cancel did not release held button: {events:?}"
        );
        assert_eq!(receipt_events(&cancelled).last().unwrap().1, "close");
        assert_eq!(device_names(&base), original_devices);

        // Cause a real native error while a different script mouse holds a
        // button. The whole failed run must still close and release every mouse.
        app.event_types();
        let mut removed = false;
        let started = std::time::Instant::now();
        let failed = crate::script::execute_with_cancel(
            r#"
            let live = mouse(name="script-error-live");
            let victim = mouse(name="script-error-victim");
            live.move(x=70,y=80,duration=0);
            live.down();
            victim.move(x=310,y=360,duration=300,steps=30);
        "#,
            options,
            || {
                if !removed && started.elapsed() >= std::time::Duration::from_millis(50) {
                    removed = remove_named_test_master(&base, "-script-error-victim pointer");
                }
                false
            },
        )
        .unwrap();
        assert!(removed, "test did not reach native failure injection");
        assert_eq!(
            crate::json::field(&failed, "status").unwrap(),
            "failed",
            "{failed}"
        );
        assert!(
            crate::json::field(&failed, "error")
                .unwrap()
                .contains("movement stopped")
        );
        let events = app.event_types();
        assert!(
            events.contains(&4) && events.contains(&5),
            "runtime error did not release held button: {events:?}"
        );
        assert_eq!(
            receipt_events(&failed)
                .iter()
                .filter(|(_, kind)| kind == "close")
                .count(),
            2
        );
        assert_eq!(device_names(&base), original_devices);
        assert_eq!(base.position().unwrap(), original_position);
        assert_eq!(cursor_pixels(&base), original_cursor);
    }
}
