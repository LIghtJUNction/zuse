use super::{
    Result, image_metadata, quote, validate_text,
    windows_keyboard::{keys, window_key_messages},
};
use std::{ffi::c_void, path::Path, ptr};
type Hwnd = *mut c_void;
type Hdc = *mut c_void;
type Handle = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct MouseInput {
    dx: i32,
    dy: i32,
    data: u32,
    flags: u32,
    time: u32,
    extra: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct KeyboardInput {
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}
#[repr(C)]
union InputData {
    mouse: MouseInput,
    key: KeyboardInput,
}
#[repr(C)]
struct Input {
    kind: u32,
    data: InputData,
}
#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pels: i32,
    y_pels: i32,
    used: u32,
    important: u32,
}
#[repr(C)]
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 1],
}
#[link(name = "user32")]
unsafe extern "system" {
    fn SendInput(count: u32, input: *const Input, size: i32) -> u32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn EnumWindows(callback: unsafe extern "system" fn(Hwnd, isize) -> i32, param: isize) -> i32;
    fn IsWindowVisible(window: Hwnd) -> i32;
    fn IsWindow(window: Hwnd) -> i32;
    fn GetWindowTextW(window: Hwnd, text: *mut u16, max: i32) -> i32;
    fn GetClassNameW(window: Hwnd, text: *mut u16, max: i32) -> i32;
    fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
    fn GetWindowRect(window: Hwnd, rect: *mut Rect) -> i32;
    fn GetForegroundWindow() -> Hwnd;
    fn SetForegroundWindow(window: Hwnd) -> i32;
    fn ShowWindowAsync(window: Hwnd, command: i32) -> i32;
    fn PostMessageW(window: Hwnd, message: u32, wparam: usize, lparam: isize) -> i32;
    fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
    fn GetDC(window: Hwnd) -> Hdc;
    fn GetWindowDC(window: Hwnd) -> Hdc;
    fn ReleaseDC(window: Hwnd, dc: Hdc) -> i32;
    fn PrintWindow(window: Hwnd, dc: Hdc, flags: u32) -> i32;
    fn SetThreadDpiAwarenessContext(context: isize) -> isize;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: Hdc) -> Hdc;
    fn CreateDIBSection(
        dc: Hdc,
        info: *const BitmapInfo,
        usage: u32,
        bits: *mut *mut c_void,
        section: Handle,
        offset: u32,
    ) -> Handle;
    fn SelectObject(dc: Hdc, object: Handle) -> Handle;
    fn DeleteObject(object: Handle) -> i32;
    fn DeleteDC(dc: Hdc) -> i32;
    fn BitBlt(
        dst: Hdc,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        src: Hdc,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> i32;
}
struct DpiGuard(isize);
impl DpiGuard {
    fn new() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(-4) })
    }
}
impl Drop for DpiGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}
fn os_error(operation: &str) -> String {
    format!("{operation}: {}", std::io::Error::last_os_error())
}
fn send(inputs: &[Input]) -> Result<()> {
    // SAFETY: INPUT layout follows Win32, and every union member matches kind.
    let n = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<Input>() as i32,
        )
    };
    if n as usize == inputs.len() {
        Ok(())
    } else {
        Err(format!(
            "SendInput accepted {n}/{} events; input integrity level may differ",
            inputs.len()
        ))
    }
}
fn key_event(vk: u16, scan: u16, flags: u32) -> Input {
    Input {
        kind: 1,
        data: InputData {
            key: KeyboardInput {
                vk,
                scan,
                flags,
                time: 0,
                extra: 0,
            },
        },
    }
}
fn mouse_event(data: u32, flags: u32) -> Input {
    Input {
        kind: 0,
        data: InputData {
            mouse: MouseInput {
                dx: 0,
                dy: 0,
                data,
                flags,
                time: 0,
                extra: 0,
            },
        },
    }
}
fn window(id: &str) -> Result<Hwnd> {
    let w = if id == "active" {
        unsafe { GetForegroundWindow() }
    } else {
        let n = if let Some(s) = id.strip_prefix("0x") {
            usize::from_str_radix(s, 16)
        } else {
            id.parse::<usize>()
        }
        .map_err(|_| "invalid HWND")?;
        n as Hwnd
    };
    if w.is_null() || unsafe { IsWindow(w) } == 0 {
        Err("window is unavailable".into())
    } else {
        Ok(w)
    }
}
pub struct Desktop;
impl Desktop {
    pub fn connect() -> Result<Self> {
        let mut p = Point::default();
        if unsafe { GetCursorPos(&mut p) } == 0 {
            Err(os_error("access desktop"))
        } else {
            Ok(Self)
        }
    }
    pub fn backend(&self) -> &'static str {
        "win32"
    }
    pub fn capabilities(&self) -> String {
        String::from(
            "{\"backend\":\"win32\",\"native\":true,\"pointer\":true,\"keyboard\":true,\"windows\":true,\"capture\":true,\"backgroundInput\":\"PostMessage; application dependent\",\"windowCapture\":\"PrintWindow with GDI fallback\",\"permission\":\"current user desktop; UIPI applies\"}",
        )
    }
    pub fn position(&self) -> Result<(i32, i32)> {
        let _dpi = DpiGuard::new();
        let mut p = Point::default();
        if unsafe { GetCursorPos(&mut p) } == 0 {
            Err(os_error("GetCursorPos"))
        } else {
            Ok((p.x, p.y))
        }
    }
    pub fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        let _dpi = DpiGuard::new();
        if unsafe { SetCursorPos(x, y) } == 0 {
            Err(os_error("SetCursorPos"))
        } else {
            Ok(())
        }
    }
    pub fn button(&mut self, button: &str, action: &str) -> Result<()> {
        let (down, up, data) = match button {
            "left" => (2, 4, 0),
            "right" => (8, 16, 0),
            "middle" => (32, 64, 0),
            "back" => (0x80, 0x100, 1),
            "forward" => (0x80, 0x100, 2),
            _ => return Err("button must be left, middle, right, back or forward".into()),
        };
        match action {
            "down" => send(&[mouse_event(data, down)]),
            "up" => send(&[mouse_event(data, up)]),
            "click" => send(&[mouse_event(data, down), mouse_event(data, up)]),
            _ => Err("button action must be down, up or click".into()),
        }
    }
    pub fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        if dx.unsigned_abs() > 1000 || dy.unsigned_abs() > 1000 {
            return Err("scroll is limited to 1000 notches per axis".into());
        }
        let mut events = Vec::new();
        if dy != 0 {
            events.push(mouse_event((-dy * 120) as u32, 0x800));
        }
        if dx != 0 {
            events.push(mouse_event((dx * 120) as u32, 0x1000));
        }
        send(&events)
    }
    pub fn key(&mut self, chord: &str) -> Result<()> {
        let keys = keys(chord)?;
        let mut events = Vec::with_capacity(keys.len() * 2);
        for &vk in &keys {
            events.push(key_event(
                vk,
                0,
                if matches!(vk, 0x21..=0x28 | 0x2d | 0x2e | 0x5b) {
                    1
                } else {
                    0
                },
            ));
        }
        for &vk in keys.iter().rev() {
            events.push(key_event(
                vk,
                0,
                2 | if matches!(vk, 0x21..=0x28 | 0x2d | 0x2e | 0x5b) {
                    1
                } else {
                    0
                },
            ));
        }
        send(&events)
    }
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        validate_text(text)?;
        // A UTF-16 surrogate pair is posted as its two Unicode key units.
        for unit in text.encode_utf16() {
            send(&[key_event(0, unit, 4), key_event(0, unit, 6)])?;
        }
        Ok(())
    }
    pub fn type_into(&mut self, id: &str, text: &str) -> Result<()> {
        validate_text(text)?;
        let w = window(id)?;
        for unit in text.encode_utf16() {
            if unsafe { PostMessageW(w, 0x102, usize::from(unit), 1) } == 0 {
                return Err(os_error("PostMessageW WM_CHAR"));
            }
        }
        Ok(())
    }
    pub fn key_into(&mut self, id: &str, chord: &str) -> Result<()> {
        // Complete parsing and mapping before posting the first key-down.
        // MAPVK_VK_TO_VSC_EX preserves the E0 prefix for extended keys.
        let events = window_key_messages(chord, |vk| unsafe { MapVirtualKeyW(u32::from(vk), 4) })?;
        let w = window(id)?;
        for event in events {
            if unsafe {
                PostMessageW(
                    w,
                    event.message,
                    usize::from(event.vk),
                    event.lparam as isize,
                )
            } == 0
            {
                return Err(os_error("PostMessageW keyboard message"));
            }
        }
        Ok(())
    }
    pub fn focus(&mut self, id: &str) -> Result<()> {
        let w = window(id)?;
        unsafe {
            ShowWindowAsync(w, 9);
        }
        if unsafe { SetForegroundWindow(w) } == 0 {
            Err("Windows declined foreground activation under its focus policy".into())
        } else {
            Ok(())
        }
    }
    pub fn windows(&self) -> Result<String> {
        let _dpi = DpiGuard::new();
        let mut rows = Vec::<String>::new();
        unsafe extern "system" fn visit(w: Hwnd, param: isize) -> i32 {
            // SAFETY: EnumWindows receives the live rows Vec exclusively for the callback.
            unsafe {
                let rows = &mut *(param as *mut Vec<String>);
                if IsWindowVisible(w) == 0 {
                    return 1;
                }
                if rows.len() >= 4096 {
                    return 0;
                }
                let mut title = [0u16; 4096];
                let nt = GetWindowTextW(w, title.as_mut_ptr(), 4096).max(0) as usize;
                let mut class = [0u16; 256];
                let nc = GetClassNameW(w, class.as_mut_ptr(), 256).max(0) as usize;
                let mut pid = 0;
                GetWindowThreadProcessId(w, &mut pid);
                let mut rect = Rect::default();
                if GetWindowRect(w, &mut rect) == 0 {
                    return 1;
                }
                rows.push(format!("{{\"id\":\"0x{:x}\",\"title\":{},\"class\":{},\"pid\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}",
                    w as usize,quote(&String::from_utf16_lossy(&title[..nt])),quote(&String::from_utf16_lossy(&class[..nc])),
                    pid,rect.left,rect.top,rect.right-rect.left,rect.bottom-rect.top));
            }
            1
        }
        let ok = unsafe { EnumWindows(visit, &mut rows as *mut Vec<String> as isize) };
        if ok == 0 && rows.len() < 4096 {
            return Err(os_error("EnumWindows"));
        }
        Ok(format!("[{}]", rows.join(",")))
    }
    pub fn screenshot(&self, path: &Path, id: Option<&str>) -> Result<String> {
        let _dpi = DpiGuard::new();
        let w = id.map(window).transpose()?.unwrap_or(ptr::null_mut());
        let (x, y, width, height) = if w.is_null() {
            unsafe {
                (
                    GetSystemMetrics(76),
                    GetSystemMetrics(77),
                    GetSystemMetrics(78),
                    GetSystemMetrics(79),
                )
            }
        } else {
            let mut r = Rect::default();
            if unsafe { GetWindowRect(w, &mut r) } == 0 {
                return Err(os_error("GetWindowRect"));
            }
            (0, 0, r.right - r.left, r.bottom - r.top)
        };
        if width <= 0
            || height <= 0
            || width > 16_384
            || height > 16_384
            || (width as u64) * (height as u64) > 67_108_864
        {
            return Err("capture dimensions exceed limits".into());
        }
        // SAFETY: all native handles are checked, selected out and released below.
        unsafe {
            let source = if w.is_null() {
                GetDC(w)
            } else {
                GetWindowDC(w)
            };
            if source.is_null() {
                return Err(os_error("GetDC"));
            }
            let dc = CreateCompatibleDC(source);
            if dc.is_null() {
                ReleaseDC(w, source);
                return Err(os_error("CreateCompatibleDC"));
            }
            let info = BitmapInfo {
                header: BitmapInfoHeader {
                    size: 40,
                    width,
                    height: -height,
                    planes: 1,
                    bit_count: 32,
                    compression: 0,
                    size_image: 0,
                    x_pels: 0,
                    y_pels: 0,
                    used: 0,
                    important: 0,
                },
                colors: [0],
            };
            let mut bits = ptr::null_mut();
            let bitmap = CreateDIBSection(source, &info, 0, &mut bits, ptr::null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                ReleaseDC(w, source);
                return Err(os_error("CreateDIBSection"));
            }
            let previous = SelectObject(dc, bitmap);
            let captured = if !w.is_null() && PrintWindow(w, dc, 2) != 0 {
                true
            } else {
                BitBlt(dc, 0, 0, width, height, source, x, y, 0x40cc0020) != 0
            };
            let result = if captured {
                super::png::write_png(path, width as u32, height as u32, |px, py| {
                    let p = bits
                        .cast::<u8>()
                        .add(((py as usize) * (width as usize) + (px as usize)) * 4);
                    [*p.add(2), *p.add(1), *p]
                })
            } else {
                Err(os_error("capture GDI bitmap"))
            };
            SelectObject(dc, previous);
            DeleteObject(bitmap);
            DeleteDC(dc);
            ReleaseDC(w, source);
            result?;
        }
        Ok(image_metadata(path, width as u32, height as u32, "win32"))
    }
}
