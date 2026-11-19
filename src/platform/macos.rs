use super::{Result, image_metadata, quote, split_chord, validate_text};
use std::{
    ffi::{c_char, c_void},
    path::Path,
    ptr,
};
type Ref = *const c_void;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    origin: Point,
    size: Size,
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFDictionaryGetValue(dict: Ref, key: Ref) -> Ref;
    fn CFNumberGetValue(number: Ref, kind: i32, value: *mut c_void) -> bool;
    fn CFStringGetLength(string: Ref) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: bool,
    ) -> Ref;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: Ref,
        bytes: *const u8,
        length: isize,
        is_dir: bool,
    ) -> Ref;
    static kCFBooleanTrue: Ref;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementSetAttributeValue(element: Ref, attribute: Ref, value: Ref) -> i32;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    fn AXUIElementPerformAction(element: Ref, action: Ref) -> i32;
    fn CGEventCreate(source: Ref) -> Ref;
    fn CGEventGetLocation(event: Ref) -> Point;
    fn CGEventCreateMouseEvent(source: Ref, kind: u32, point: Point, button: u32) -> Ref;
    fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
    fn CGEventKeyboardSetUnicodeString(event: Ref, length: usize, text: *const u16);
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventPost(tap: u32, event: Ref);
    fn CGEventPostToPid(pid: i32, event: Ref);
    fn CGEventCreateScrollWheelEvent(source: Ref, units: u32, wheels: u32, ...) -> Ref;
    fn CGWindowListCopyWindowInfo(options: u32, relative: u32) -> Ref;
    fn CGRectMakeWithDictionaryRepresentation(dict: Ref, rect: *mut Rect) -> bool;
    fn CGWindowListCreateImage(bounds: Rect, options: u32, window: u32, image_options: u32) -> Ref;
    fn CGImageGetWidth(image: Ref) -> usize;
    fn CGImageGetHeight(image: Ref) -> usize;
    fn CGPreflightScreenCaptureAccess() -> bool;
    static CGRectInfinite: Rect;
    static kCGWindowNumber: Ref;
    static kCGWindowName: Ref;
    static kCGWindowOwnerName: Ref;
    static kCGWindowOwnerPID: Ref;
    static kCGWindowBounds: Ref;
    static kCGWindowLayer: Ref;
}
#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageDestinationCreateWithURL(url: Ref, kind: Ref, count: usize, options: Ref) -> Ref;
    fn CGImageDestinationAddImage(destination: Ref, image: Ref, properties: Ref);
    fn CGImageDestinationFinalize(destination: Ref) -> bool;
}
struct Owned(Ref);
impl Owned {
    fn new(value: Ref, what: &str) -> Result<Self> {
        if value.is_null() {
            Err(format!(
                "{what} returned no object; check macOS permissions"
            ))
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}
fn cf_string(s: &str) -> Result<Owned> {
    Owned::new(
        unsafe {
            CFStringCreateWithBytes(ptr::null(), s.as_ptr(), s.len() as isize, 0x08000100, false)
        },
        "CFString",
    )
}
fn string(value: Ref) -> String {
    if value.is_null() {
        return String::new();
    }
    unsafe {
        let len = CFStringGetLength(value);
        if !(0..=65_536).contains(&len) {
            return String::new();
        }
        let capacity = CFStringGetMaximumSizeForEncoding(len, 0x08000100) + 1;
        if capacity <= 0 || capacity > 262_145 {
            return String::new();
        }
        let mut bytes = vec![0; capacity as usize];
        if !CFStringGetCString(value, bytes.as_mut_ptr().cast(), capacity, 0x08000100) {
            return String::new();
        }
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }
}
fn number(value: Ref) -> i64 {
    let mut n = 0i64;
    if !value.is_null() {
        unsafe {
            CFNumberGetValue(value, 4, (&mut n as *mut i64).cast());
        }
    }
    n
}
struct Window {
    id: u32,
    pid: i32,
    title: String,
    owner: String,
    bounds: Rect,
}
fn window_list() -> Result<Vec<Window>> {
    let array = Owned::new(
        unsafe { CGWindowListCopyWindowInfo(1 | 16, 0) },
        "CGWindowListCopyWindowInfo",
    )?;
    let count = unsafe { CFArrayGetCount(array.0) }.clamp(0, 4096);
    let mut rows = Vec::new();
    for index in 0..count {
        unsafe {
            let dict = CFArrayGetValueAtIndex(array.0, index);
            if number(CFDictionaryGetValue(dict, kCGWindowLayer)) != 0 {
                continue;
            }
            let id = number(CFDictionaryGetValue(dict, kCGWindowNumber)) as u32;
            let pid = number(CFDictionaryGetValue(dict, kCGWindowOwnerPID)) as i32;
            let title = string(CFDictionaryGetValue(dict, kCGWindowName));
            let owner = string(CFDictionaryGetValue(dict, kCGWindowOwnerName));
            let mut bounds = Rect::default();
            if !CGRectMakeWithDictionaryRepresentation(
                CFDictionaryGetValue(dict, kCGWindowBounds),
                &mut bounds,
            ) {
                continue;
            }
            rows.push(Window {
                id,
                pid,
                title,
                owner,
                bounds,
            });
        }
    }
    Ok(rows)
}
fn window(id: &str) -> Result<Window> {
    let rows = window_list()?;
    if id == "active" {
        return rows
            .into_iter()
            .next()
            .ok_or("no visible application window".into());
    }
    let n = if let Some(hex) = id.strip_prefix("0x") {
        u32::from_str_radix(hex, 16)
    } else {
        id.parse::<u32>()
    }
    .map_err(|_| "window ID must be decimal, 0x hexadecimal or active")?;
    rows.into_iter()
        .find(|w| w.id == n)
        .ok_or("window is unavailable".into())
}
fn key_code(name: &str) -> Result<u16> {
    let n = name.to_ascii_lowercase();
    Ok(match n.as_str() {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "equal" | "plus" => 24,
        "9" => 25,
        "7" => 26,
        "minus" => 27,
        "8" => 28,
        "0" => 29,
        "bracketright" => 30,
        "o" => 31,
        "u" => 32,
        "bracketleft" => 33,
        "i" => 34,
        "p" => 35,
        "return" | "enter" => 36,
        "l" => 37,
        "j" => 38,
        "apostrophe" => 39,
        "k" => 40,
        "semicolon" => 41,
        "backslash" => 42,
        "comma" => 43,
        "slash" => 44,
        "n" => 45,
        "m" => 46,
        "period" => 47,
        "tab" => 48,
        "space" => 49,
        "grave" => 50,
        "backspace" => 51,
        "escape" | "esc" => 53,
        "cmd" | "command" | "meta" | "super" | "win" => 55,
        "shift" => 56,
        "capslock" => 57,
        "alt" | "option" => 58,
        "ctrl" | "control" => 59,
        "f17" => 64,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f3" => 99,
        "f8" => 100,
        "f9" => 101,
        "f11" => 103,
        "f13" => 105,
        "f16" => 106,
        "f14" => 107,
        "f10" => 109,
        "f12" => 111,
        "f15" => 113,
        "home" => 115,
        "pageup" => 116,
        "delete" | "del" => 117,
        "f4" => 118,
        "end" => 119,
        "f2" => 120,
        "pagedown" => 121,
        "f1" => 122,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => {
            return Err(format!(
                "unknown macOS virtual key {name}; text input supports Unicode independently of layout"
            ));
        }
    })
}
pub struct Desktop {
    pressed: u8,
}
impl Desktop {
    pub fn connect() -> Result<Self> {
        Ok(Self { pressed: 0 })
    }
    pub fn backend(&self) -> &'static str {
        "quartz"
    }
    pub fn capabilities(&self) -> String {
        format!(
            "{{\"backend\":\"quartz\",\"native\":true,\"pointer\":{},\"keyboard\":{},\"windows\":true,\"capture\":{},\"backgroundInput\":\"CGEventPostToPid; application dependent\",\"permission\":\"grant Accessibility for input and Screen Recording for capture\"}}",
            unsafe { AXIsProcessTrusted() },
            unsafe { AXIsProcessTrusted() },
            unsafe { CGPreflightScreenCaptureAccess() }
        )
    }
    fn input_permission(&self) -> Result<()> {
        if unsafe { AXIsProcessTrusted() } {
            Ok(())
        } else {
            Err("macOS Accessibility permission is required; enable the calling application in System Settings > Privacy & Security > Accessibility".into())
        }
    }
    pub fn position(&self) -> Result<(i32, i32)> {
        let event = Owned::new(unsafe { CGEventCreate(ptr::null()) }, "CGEventCreate")?;
        let p = unsafe { CGEventGetLocation(event.0) };
        Ok((p.x as i32, p.y as i32))
    }
    fn post(&self, event: Ref, pid: Option<i32>) -> Result<()> {
        let event = Owned::new(event, "CGEvent")?;
        unsafe {
            if let Some(pid) = pid {
                CGEventPostToPid(pid, event.0)
            } else {
                CGEventPost(0, event.0)
            }
        }
        Ok(())
    }
    pub fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        self.input_permission()?;
        let (kind, button) = if self.pressed & 1 != 0 {
            (6, 0)
        } else if self.pressed & 2 != 0 {
            (7, 1)
        } else if self.pressed & 4 != 0 {
            (27, 2)
        } else {
            (5, 0)
        };
        self.post(
            unsafe {
                CGEventCreateMouseEvent(
                    ptr::null(),
                    kind,
                    Point {
                        x: x as f64,
                        y: y as f64,
                    },
                    button,
                )
            },
            None,
        )
    }
    pub fn button(&mut self, button: &str, action: &str) -> Result<()> {
        self.input_permission()?;
        let (b, down, up, bit) = match button {
            "left" => (0, 1, 2, 1),
            "right" => (1, 3, 4, 2),
            "middle" => (2, 25, 26, 4),
            "back" => (3, 25, 26, 8),
            "forward" => (4, 25, 26, 16),
            _ => return Err("button must be left, middle, right, back or forward".into()),
        };
        if !matches!(action, "down" | "up" | "click") {
            return Err("button action must be down, up or click".into());
        }
        let (x, y) = self.position()?;
        let p = Point {
            x: x as f64,
            y: y as f64,
        };
        if action == "down" || action == "click" {
            self.post(
                unsafe { CGEventCreateMouseEvent(ptr::null(), down, p, b) },
                None,
            )?;
            self.pressed |= bit;
        }
        if action == "up" || action == "click" {
            self.post(
                unsafe { CGEventCreateMouseEvent(ptr::null(), up, p, b) },
                None,
            )?;
            self.pressed &= !bit;
        }
        Ok(())
    }
    pub fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        self.input_permission()?;
        if dx.unsigned_abs() > 1000 || dy.unsigned_abs() > 1000 {
            return Err("scroll is limited to 1000 notches per axis".into());
        }
        self.post(
            unsafe { CGEventCreateScrollWheelEvent(ptr::null(), 1, 2, -dy, -dx) },
            None,
        )
    }
    pub fn key(&mut self, chord: &str) -> Result<()> {
        self.key_target(None, chord)
    }
    pub fn key_into(&mut self, id: &str, chord: &str) -> Result<()> {
        self.key_target(Some(window(id)?.pid), chord)
    }
    fn key_target(&self, pid: Option<i32>, chord: &str) -> Result<()> {
        self.input_permission()?;
        let parts = split_chord(chord)?;
        let keys = parts
            .iter()
            .map(|s| key_code(s))
            .collect::<Result<Vec<_>>>()?;
        let modifier = |code: u16| match code {
            56 => 1u64 << 17,
            59 => 1u64 << 18,
            58 => 1u64 << 19,
            55 => 1u64 << 20,
            _ => 0,
        };
        let mut flags = 0u64;
        for &code in &keys {
            flags |= modifier(code);
            let e = Owned::new(
                unsafe { CGEventCreateKeyboardEvent(ptr::null(), code, true) },
                "keyboard event",
            )?;
            unsafe {
                CGEventSetFlags(e.0, flags);
                if let Some(pid) = pid {
                    CGEventPostToPid(pid, e.0)
                } else {
                    CGEventPost(0, e.0)
                }
            }
        }
        for &code in keys.iter().rev() {
            flags &= !modifier(code);
            let e = Owned::new(
                unsafe { CGEventCreateKeyboardEvent(ptr::null(), code, false) },
                "keyboard event",
            )?;
            unsafe {
                CGEventSetFlags(e.0, flags);
                if let Some(pid) = pid {
                    CGEventPostToPid(pid, e.0)
                } else {
                    CGEventPost(0, e.0)
                }
            }
        }
        Ok(())
    }
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        self.type_target(None, text)
    }
    pub fn type_into(&mut self, id: &str, text: &str) -> Result<()> {
        self.type_target(Some(window(id)?.pid), text)
    }
    fn type_target(&self, pid: Option<i32>, text: &str) -> Result<()> {
        self.input_permission()?;
        validate_text(text)?;
        for ch in text.chars() {
            let mut buffer = [0u16; 2];
            let encoded = ch.encode_utf16(&mut buffer);
            for down in [true, false] {
                let e = Owned::new(
                    unsafe { CGEventCreateKeyboardEvent(ptr::null(), 0, down) },
                    "Unicode event",
                )?;
                unsafe {
                    CGEventKeyboardSetUnicodeString(e.0, encoded.len(), encoded.as_ptr());
                    if let Some(pid) = pid {
                        CGEventPostToPid(pid, e.0)
                    } else {
                        CGEventPost(0, e.0)
                    }
                }
            }
        }
        Ok(())
    }
    pub fn windows(&self) -> Result<String> {
        let rows=window_list()?.into_iter().map(|w|format!("{{\"id\":{},\"pid\":{},\"title\":{},\"class\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}",
            quote(&w.id.to_string()),w.pid,quote(&w.title),quote(&w.owner),w.bounds.origin.x,w.bounds.origin.y,w.bounds.size.width,w.bounds.size.height)).collect::<Vec<_>>();
        Ok(format!("[{}]", rows.join(",")))
    }
    pub fn focus(&mut self, id: &str) -> Result<()> {
        self.input_permission()?;
        let w = window(id)?;
        let app = Owned::new(
            unsafe { AXUIElementCreateApplication(w.pid) },
            "AXUIElementCreateApplication",
        )?;
        let front = cf_string("AXFrontmost")?;
        let status = unsafe { AXUIElementSetAttributeValue(app.0, front.0, kCFBooleanTrue) };
        if status != 0 {
            return Err(format!("AXFrontmost failed with error {status}"));
        }
        let attr = cf_string("AXWindows")?;
        let mut value = ptr::null();
        if unsafe { AXUIElementCopyAttributeValue(app.0, attr.0, &mut value) } == 0
            && !value.is_null()
        {
            let array = Owned(value);
            let title = cf_string("AXTitle")?;
            let raise = cf_string("AXRaise")?;
            for index in 0..unsafe { CFArrayGetCount(array.0) }.clamp(0, 4096) {
                let win = unsafe { CFArrayGetValueAtIndex(array.0, index) };
                let mut value = ptr::null();
                if unsafe { AXUIElementCopyAttributeValue(win, title.0, &mut value) } == 0
                    && !value.is_null()
                {
                    let t = Owned(value);
                    if string(t.0) == w.title {
                        unsafe {
                            AXUIElementPerformAction(win, raise.0);
                        }
                        break;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn screenshot(&self, path: &Path, id: Option<&str>) -> Result<String> {
        if !unsafe { CGPreflightScreenCaptureAccess() } {
            return Err("macOS Screen Recording permission is required; enable it in System Settings > Privacy & Security".into());
        }
        let id = id.map(window).transpose()?.map(|w| w.id);
        let image = Owned::new(
            unsafe {
                CGWindowListCreateImage(
                    CGRectInfinite,
                    if id.is_some() { 8 } else { 1 },
                    id.unwrap_or(0),
                    1,
                )
            },
            "CGWindowListCreateImage",
        )?;
        let (width, height) = unsafe { (CGImageGetWidth(image.0), CGImageGetHeight(image.0)) };
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        use std::os::unix::ffi::OsStrExt;
        let bytes = absolute.as_os_str().as_bytes();
        let url = Owned::new(
            unsafe {
                CFURLCreateFromFileSystemRepresentation(
                    ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as isize,
                    false,
                )
            },
            "file URL",
        )?;
        let png = cf_string("public.png")?;
        let destination = Owned::new(
            unsafe { CGImageDestinationCreateWithURL(url.0, png.0, 1, ptr::null()) },
            "image destination",
        )?;
        unsafe {
            CGImageDestinationAddImage(destination.0, image.0, ptr::null());
        }
        if !unsafe { CGImageDestinationFinalize(destination.0) } {
            return Err("macOS PNG encoding failed".into());
        }
        Ok(image_metadata(path, width as u32, height as u32, "quartz"))
    }
}
