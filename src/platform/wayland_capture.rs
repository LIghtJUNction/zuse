//! Capture one retained Wayland toplevel, identified by its complete address.
//!
//! The mapping protocol returns both halves of Hyprland's window address.
//! Export v2 then receives that same foreign-toplevel object. No output crop,
//! title match, truncated address or external capture utility is used.
use super::{CaptureRegion, Result};
use crate::{observation::Cancellation, platform::dylib::Library};
use std::{
    cell::Cell,
    ffi::{CStr, c_char, c_int, c_void},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

type Proxy = *mut c_void;
const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TOPLEVELS: usize = 4096;
const MAX_BYTES: usize = 256 * 1024 * 1024;

#[repr(C)]
struct Message {
    name: *const c_char,
    signature: *const c_char,
    types: *const *const Interface,
}
#[repr(C)]
struct Interface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const Message,
    event_count: c_int,
    events: *const Message,
}
// SAFETY: all protocol descriptors and their pointer arrays are immutable.
unsafe impl Sync for Message {}
unsafe impl Sync for Interface {}
struct Types<const N: usize>([*const Interface; N]);
unsafe impl<const N: usize> Sync for Types<N> {}
static NO_TYPES: Types<5> = Types([ptr::null(); 5]);
macro_rules! message {
    ($name:literal, $signature:literal) => {
        Message {
            name: concat!($name, "\0").as_ptr().cast(),
            signature: concat!($signature, "\0").as_ptr().cast(),
            types: NO_TYPES.0.as_ptr(),
        }
    };
    ($name:literal, $signature:literal, $types:ident) => {
        Message {
            name: concat!($name, "\0").as_ptr().cast(),
            signature: concat!($signature, "\0").as_ptr().cast(),
            types: $types.0.as_ptr(),
        }
    };
}
static TOP_METHODS: [Message; 8] = [
    message!("set_maximized", ""),
    message!("unset_maximized", ""),
    message!("set_minimized", ""),
    message!("unset_minimized", ""),
    message!("activate", "o"),
    message!("close", ""),
    message!("set_rectangle", "oiiii"),
    message!("destroy", ""),
];
static TOP_EVENTS: [Message; 7] = [
    message!("title", "s"),
    message!("app_id", "s"),
    message!("output_enter", "o"),
    message!("output_leave", "o"),
    message!("state", "a"),
    message!("done", ""),
    message!("closed", ""),
];
static TOP_INTERFACE: Interface = Interface {
    name: c"zwlr_foreign_toplevel_handle_v1".as_ptr(),
    version: 1,
    method_count: 8,
    methods: TOP_METHODS.as_ptr(),
    event_count: 7,
    events: TOP_EVENTS.as_ptr(),
};
static FOREIGN_TOP_TYPES: Types<1> = Types([&TOP_INTERFACE]);
static FOREIGN_METHODS: [Message; 1] = [message!("stop", "")];
static FOREIGN_EVENTS: [Message; 2] = [
    message!("toplevel", "n", FOREIGN_TOP_TYPES),
    message!("finished", ""),
];
static FOREIGN_INTERFACE: Interface = Interface {
    name: c"zwlr_foreign_toplevel_manager_v1".as_ptr(),
    version: 1,
    method_count: 1,
    methods: FOREIGN_METHODS.as_ptr(),
    event_count: 2,
    events: FOREIGN_EVENTS.as_ptr(),
};
static MAP_HANDLE_METHODS: [Message; 1] = [message!("destroy", "")];
static MAP_HANDLE_EVENTS: [Message; 2] = [message!("window_address", "uu"), message!("failed", "")];
static MAP_HANDLE_INTERFACE: Interface = Interface {
    name: c"hyprland_toplevel_window_mapping_handle_v1".as_ptr(),
    version: 1,
    method_count: 1,
    methods: MAP_HANDLE_METHODS.as_ptr(),
    event_count: 2,
    events: MAP_HANDLE_EVENTS.as_ptr(),
};
static MAP_EXT_TYPES: Types<2> = Types([&MAP_HANDLE_INTERFACE, ptr::null()]);
static MAP_WLR_TYPES: Types<2> = Types([&MAP_HANDLE_INTERFACE, &TOP_INTERFACE]);
static MAP_METHODS: [Message; 3] = [
    message!("get_window_for_toplevel", "no", MAP_EXT_TYPES),
    message!("get_window_for_toplevel_wlr", "no", MAP_WLR_TYPES),
    message!("destroy", ""),
];
static MAP_INTERFACE: Interface = Interface {
    name: c"hyprland_toplevel_mapping_manager_v1".as_ptr(),
    version: 1,
    method_count: 3,
    methods: MAP_METHODS.as_ptr(),
    event_count: 0,
    events: ptr::null(),
};
static FRAME_METHODS: [Message; 2] = [message!("copy", "oi"), message!("destroy", "")];
static FRAME_EVENTS: [Message; 7] = [
    message!("buffer", "uuuu"),
    message!("damage", "uuuu"),
    message!("flags", "u"),
    message!("ready", "uuu"),
    message!("failed", ""),
    message!("linux_dmabuf", "uuu"),
    message!("buffer_done", ""),
];
static FRAME_INTERFACE: Interface = Interface {
    name: c"hyprland_toplevel_export_frame_v1".as_ptr(),
    version: 2,
    method_count: 2,
    methods: FRAME_METHODS.as_ptr(),
    event_count: 7,
    events: FRAME_EVENTS.as_ptr(),
};
static EXPORT_NUMBER_TYPES: Types<3> = Types([&FRAME_INTERFACE, ptr::null(), ptr::null()]);
static EXPORT_OBJECT_TYPES: Types<3> = Types([&FRAME_INTERFACE, ptr::null(), &TOP_INTERFACE]);
static EXPORT_METHODS: [Message; 3] = [
    message!("capture_toplevel", "niu", EXPORT_NUMBER_TYPES),
    message!("destroy", ""),
    message!(
        "capture_toplevel_with_wlr_toplevel_handle",
        "2nio",
        EXPORT_OBJECT_TYPES
    ),
];
static EXPORT_INTERFACE: Interface = Interface {
    name: c"hyprland_toplevel_export_manager_v1".as_ptr(),
    version: 2,
    method_count: 3,
    methods: EXPORT_METHODS.as_ptr(),
    event_count: 0,
    events: ptr::null(),
};

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
#[repr(C)]
struct SockaddrUn {
    family: u16,
    path: [c_char; 108],
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Peer {
    pid: c_int,
    uid: u32,
    gid: u32,
}
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn memfd_create(name: *const c_char, flags: u32) -> c_int;
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn connect(fd: c_int, address: *const c_void, length: u32) -> c_int;
    fn getsockopt(
        fd: c_int,
        level: c_int,
        option: c_int,
        value: *mut c_void,
        length: *mut u32,
    ) -> c_int;
}
#[derive(Clone, Copy)]
struct Api {
    connect_fd: unsafe extern "C" fn(c_int) -> Proxy,
    disconnect: unsafe extern "C" fn(Proxy),
    fd: unsafe extern "C" fn(Proxy) -> c_int,
    flush: unsafe extern "C" fn(Proxy) -> c_int,
    dispatch_pending: unsafe extern "C" fn(Proxy) -> c_int,
    prepare_read: unsafe extern "C" fn(Proxy) -> c_int,
    read_events: unsafe extern "C" fn(Proxy) -> c_int,
    cancel_read: unsafe extern "C" fn(Proxy),
    destroy: unsafe extern "C" fn(Proxy),
    add_listener: unsafe extern "C" fn(Proxy, *const *const c_void, Proxy) -> c_int,
    marshal: unsafe extern "C" fn(Proxy, u32, *const Interface, u32, u32, ...) -> Proxy,
    registry: *const Interface,
    callback: *const Interface,
    shm: *const Interface,
    pool: *const Interface,
    buffer: *const Interface,
}
impl Api {
    fn load(lib: &Library) -> Result<Self> {
        // SAFETY: signatures match libwayland-client's public C ABI.
        unsafe {
            Ok(Self {
                connect_fd: lib.symbol(c"wl_display_connect_to_fd")?,
                disconnect: lib.symbol(c"wl_display_disconnect")?,
                fd: lib.symbol(c"wl_display_get_fd")?,
                flush: lib.symbol(c"wl_display_flush")?,
                dispatch_pending: lib.symbol(c"wl_display_dispatch_pending")?,
                prepare_read: lib.symbol(c"wl_display_prepare_read")?,
                read_events: lib.symbol(c"wl_display_read_events")?,
                cancel_read: lib.symbol(c"wl_display_cancel_read")?,
                destroy: lib.symbol(c"wl_proxy_destroy")?,
                add_listener: lib.symbol(c"wl_proxy_add_listener")?,
                marshal: lib.symbol(c"wl_proxy_marshal_flags")?,
                registry: lib.symbol(c"wl_registry_interface")?,
                callback: lib.symbol(c"wl_callback_interface")?,
                shm: lib.symbol(c"wl_shm_interface")?,
                pool: lib.symbol(c"wl_shm_pool_interface")?,
                buffer: lib.symbol(c"wl_buffer_interface")?,
            })
        }
    }
}
struct Listener<const N: usize>([*const c_void; N]);
// SAFETY: listener arrays contain immutable C callback function addresses.
unsafe impl<const N: usize> Sync for Listener<N> {}

#[derive(Clone, Copy)]
struct Global {
    name: u32,
    version: u32,
}
struct Top {
    object: Proxy,
    mapping: Proxy,
    address: Option<u64>,
    mapping_done: bool,
    announced: bool,
    closed: bool,
}
struct State {
    api: Api,
    globals: [Option<Global>; 4],
    mapping: Proxy,
    // Listener data points into these boxes while the vector may grow.
    #[allow(clippy::vec_box)]
    tops: Vec<Box<Top>>,
    error: Option<&'static str>,
}
impl State {
    fn selected(&self, address: u64) -> Result<usize> {
        selected_toplevel(self.tops.iter().map(Box::as_ref).enumerate(), address)
    }
}
fn selected_toplevel<'a>(
    tops: impl Iterator<Item = (usize, &'a Top)>,
    address: u64,
) -> Result<usize> {
    let mut matches = tops.filter(|(_, top)| {
        !top.closed && top.announced && top.mapping_done && top.address == Some(address)
    });
    let selected = matches
        .next()
        .map(|(index, _)| index)
        .ok_or("stale_target: the full window address has no live Wayland toplevel mapping")?;
    if matches.next().is_some() {
        return Err(
            "ambiguous_target: more than one toplevel maps to the full window address".into(),
        );
    }
    Ok(selected)
}
unsafe extern "C" fn global(
    data: Proxy,
    _: Proxy,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    if interface.is_null() || version == 0 {
        return;
    }
    // SAFETY: callbacks run synchronously with a stable boxed State.
    let state = unsafe { &mut *data.cast::<State>() };
    let index = match unsafe { CStr::from_ptr(interface) }.to_bytes() {
        b"wl_shm" => 0,
        b"zwlr_foreign_toplevel_manager_v1" => 1,
        b"hyprland_toplevel_mapping_manager_v1" => 2,
        b"hyprland_toplevel_export_manager_v1" => 3,
        _ => return,
    };
    if state.globals[index].is_some() {
        state.error =
            Some("unsupported_state: multiple window capture protocol managers are ambiguous");
    } else {
        state.globals[index] = Some(Global { name, version });
    }
}
unsafe extern "C" fn global_remove(data: Proxy, _: Proxy, name: u32) {
    let state = unsafe { &mut *data.cast::<State>() };
    if state
        .globals
        .iter()
        .flatten()
        .any(|global| global.name == name)
    {
        state.error = Some("unsupported_state: a required window capture protocol was removed");
    }
}
unsafe extern "C" fn top_text(_: Proxy, _: Proxy, _: *const c_char) {}
unsafe extern "C" fn top_output(_: Proxy, _: Proxy, _: Proxy) {}
unsafe extern "C" fn top_state(_: Proxy, _: Proxy, _: *const c_void) {}
unsafe extern "C" fn top_done(data: Proxy, _: Proxy) {
    unsafe {
        (*data.cast::<Top>()).announced = true;
    }
}
unsafe extern "C" fn top_closed(data: Proxy, _: Proxy) {
    unsafe {
        (*data.cast::<Top>()).closed = true;
    }
}
unsafe extern "C" fn mapped(data: Proxy, _: Proxy, high: u32, low: u32) {
    let top = unsafe { &mut *data.cast::<Top>() };
    top.address = Some((u64::from(high) << 32) | u64::from(low));
    top.mapping_done = true;
}
unsafe extern "C" fn mapping_failed(data: Proxy, _: Proxy) {
    let top = unsafe { &mut *data.cast::<Top>() };
    top.address = None;
    top.mapping_done = true;
}
static REGISTRY_LISTENER: Listener<2> =
    Listener([global as *const c_void, global_remove as *const c_void]);
static TOP_LISTENER: Listener<7> = Listener([
    top_text as *const c_void,
    top_text as *const c_void,
    top_output as *const c_void,
    top_output as *const c_void,
    top_state as *const c_void,
    top_done as *const c_void,
    top_closed as *const c_void,
]);
static MAPPING_LISTENER: Listener<2> =
    Listener([mapped as *const c_void, mapping_failed as *const c_void]);
unsafe extern "C" fn new_top(data: Proxy, _: Proxy, object: Proxy) {
    let state = unsafe { &mut *data.cast::<State>() };
    if object.is_null() {
        state.error = Some("unsupported_state: empty foreign toplevel object");
        return;
    }
    if state.tops.len() >= MAX_TOPLEVELS {
        unsafe {
            (state.api.destroy)(object);
        }
        state.error = Some("unsupported_state: window capture enumeration exceeds 4096 toplevels");
        return;
    }
    let mut top = Box::new(Top {
        object,
        mapping: ptr::null_mut(),
        address: None,
        mapping_done: false,
        announced: false,
        closed: false,
    });
    let top_data = (&mut *top as *mut Top).cast();
    let listen = unsafe { (state.api.add_listener)(object, TOP_LISTENER.0.as_ptr(), top_data) };
    if listen != 0 {
        state.error = Some("unsupported_state: cannot listen to a foreign toplevel");
    } else {
        top.mapping = unsafe {
            (state.api.marshal)(
                state.mapping,
                1,
                &MAP_HANDLE_INTERFACE,
                1,
                0,
                ptr::null_mut::<c_void>(),
                object,
            )
        };
        if top.mapping.is_null()
            || unsafe {
                (state.api.add_listener)(top.mapping, MAPPING_LISTENER.0.as_ptr(), top_data)
            } != 0
        {
            state.error = Some("unsupported_state: cannot bind a full window address mapping");
        }
    }
    state.tops.push(top);
}
unsafe extern "C" fn foreign_finished(data: Proxy, _: Proxy) {
    unsafe {
        (*data.cast::<State>()).error =
            Some("unsupported_state: foreign toplevel enumeration stopped");
    }
}
static FOREIGN_LISTENER: Listener<2> =
    Listener([new_top as *const c_void, foreign_finished as *const c_void]);
unsafe extern "C" fn shm_format(_: Proxy, _: Proxy, _: u32) {}
static SHM_LISTENER: Listener<1> = Listener([shm_format as *const c_void]);
unsafe extern "C" fn sync_done(data: Proxy, _: Proxy, _: u32) {
    unsafe {
        (*data.cast::<Cell<bool>>()).set(true);
    }
}
static SYNC_LISTENER: Listener<1> = Listener([sync_done as *const c_void]);

#[derive(Clone, Copy, Debug, PartialEq)]
struct BufferSpec {
    format: u32,
    width: u32,
    height: u32,
    stride: u32,
}
impl BufferSpec {
    fn bytes(self) -> Result<usize> {
        if !matches!(self.format, 0 | 1) {
            return Err(
                "unsupported_state: toplevel capture requires ARGB8888 or XRGB8888 shared memory"
                    .into(),
            );
        }
        let bytes = u64::from(self.stride) * u64::from(self.height);
        if self.width == 0
            || self.height == 0
            || self.width > 16_384
            || self.height > 16_384
            || u64::from(self.width) * u64::from(self.height) > 67_108_864
            || u64::from(self.stride) < u64::from(self.width) * 4
            || !self.stride.is_multiple_of(4)
            || bytes > MAX_BYTES as u64
            || bytes > i32::MAX as u64
        {
            return Err("unsupported_state: toplevel shared-memory dimensions or stride exceed capture limits".into());
        }
        Ok(bytes as usize)
    }
    fn pixel(self, bytes: &[u8], x: u32, y: u32, inverted: bool) -> [u8; 3] {
        let row = if inverted { self.height - 1 - y } else { y };
        let start = row as usize * self.stride as usize + x as usize * 4;
        let native = u32::from_ne_bytes(
            bytes[start..start + 4]
                .try_into()
                .expect("validated pixel extent"),
        );
        [(native >> 16) as u8, (native >> 8) as u8, native as u8]
    }
}
#[derive(Default)]
struct Frame {
    object: Proxy,
    spec: Option<BufferSpec>,
    buffers_done: bool,
    copy_sent: bool,
    ready: bool,
    flags: Option<u32>,
    error: Option<&'static str>,
}
unsafe extern "C" fn frame_buffer(
    data: Proxy,
    _: Proxy,
    format: u32,
    width: u32,
    height: u32,
    stride: u32,
) {
    let frame = unsafe { &mut *data.cast::<Frame>() };
    if frame.spec.is_some() || frame.buffers_done {
        frame.error =
            Some("unsupported_state: duplicate or late toplevel shared-memory buffer description");
    } else {
        frame.spec = Some(BufferSpec {
            format,
            width,
            height,
            stride,
        });
    }
}
unsafe extern "C" fn frame_damage(_: Proxy, _: Proxy, _: u32, _: u32, _: u32, _: u32) {}
unsafe extern "C" fn frame_flags(data: Proxy, _: Proxy, flags: u32) {
    let frame = unsafe { &mut *data.cast::<Frame>() };
    if !frame.copy_sent || frame.flags.is_some() || flags & !1 != 0 {
        frame.error = Some("unsupported_state: invalid toplevel frame orientation flags");
    } else {
        frame.flags = Some(flags);
    }
}
unsafe extern "C" fn frame_ready(data: Proxy, _: Proxy, _: u32, _: u32, nanoseconds: u32) {
    let frame = unsafe { &mut *data.cast::<Frame>() };
    if !frame.copy_sent || frame.ready || frame.flags.is_none() || nanoseconds >= 1_000_000_000 {
        frame.error =
            Some("unsupported_state: invalid toplevel frame readiness or orientation flags");
    } else {
        frame.ready = true;
    }
}
unsafe extern "C" fn frame_failed(data: Proxy, _: Proxy) {
    unsafe {
        (*data.cast::<Frame>()).error =
            Some("unsupported_state: compositor refused capture of the requested toplevel");
    }
}
unsafe extern "C" fn frame_dmabuf(_: Proxy, _: Proxy, _: u32, _: u32, _: u32) {}
unsafe extern "C" fn frame_buffers_done(data: Proxy, _: Proxy) {
    let frame = unsafe { &mut *data.cast::<Frame>() };
    if frame.buffers_done {
        frame.error = Some("unsupported_state: duplicate toplevel buffer enumeration completion");
    } else {
        frame.buffers_done = true;
    }
}
static FRAME_LISTENER: Listener<7> = Listener([
    frame_buffer as *const c_void,
    frame_damage as *const c_void,
    frame_flags as *const c_void,
    frame_ready as *const c_void,
    frame_failed as *const c_void,
    frame_dmabuf as *const c_void,
    frame_buffers_done as *const c_void,
]);
unsafe extern "C" fn buffer_release(_: Proxy, _: Proxy) {}
static BUFFER_LISTENER: Listener<1> = Listener([buffer_release as *const c_void]);

struct Buffer {
    object: Proxy,
    file: File,
}
struct Connection<'a> {
    api: Api,
    display: Proxy,
    owned: Vec<Proxy>,
    registry: Proxy,
    shm: Proxy,
    export: Proxy,
    state: Box<State>,
    selected: Option<usize>,
    frame: Option<Box<Frame>>,
    buffer: Option<Buffer>,
    peer: Peer,
    deadline: Instant,
    cancellation: &'a Cancellation,
    _library: Library,
}

fn check(deadline: Instant, cancellation: &Cancellation) -> Result<()> {
    cancellation.check()?;
    if Instant::now() >= deadline {
        return Err(
            "timeout: surface-bound Wayland window capture exceeded its 5-second operation budget"
                .into(),
        );
    }
    Ok(())
}

fn wait_fd(fd: c_int, events: i16, deadline: Instant, cancellation: &Cancellation) -> Result<()> {
    loop {
        check(deadline, cancellation)?;
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 25) as c_int;
        let mut pfd = PollFd {
            fd,
            events,
            revents: 0,
        };
        let ready = unsafe { poll(&mut pfd, 1, remaining) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!(
                "unsupported_state: poll native window capture socket: {error}"
            ));
        }
        if ready > 0 {
            if pfd.revents & (8 | 32) != 0 {
                return Err("unsupported_state: native window capture socket failed".into());
            }
            // A reader must consume EOF on HUP, rather than spinning forever.
            if pfd.revents & (events | 16) != 0 {
                return Ok(());
            }
        }
    }
}

fn local_socket(path: &Path, deadline: Instant, cancellation: &Cancellation) -> Result<OwnedFd> {
    check(deadline, cancellation)?;
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.len() >= 108 || bytes.contains(&0) {
        return Err(
            "unsupported_state: native window capture socket path is invalid or too long".into(),
        );
    }
    // Linux AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC.
    let raw = unsafe { socket(1, 1 | 0o4000 | 0o2000000, 0) };
    if raw < 0 {
        return Err(format!(
            "unsupported_state: create native capture socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut address = SockaddrUn {
        family: 1,
        path: [0; 108],
    };
    for (out, byte) in address.path.iter_mut().zip(bytes) {
        *out = *byte as c_char;
    }
    if unsafe {
        connect(
            raw,
            (&address as *const SockaddrUn).cast(),
            (2 + bytes.len() + 1) as u32,
        )
    } == 0
    {
        return Ok(fd);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(115) {
        return Err(format!(
            "unsupported_state: connect native capture socket: {error}"
        ));
    }
    wait_fd(raw, 4, deadline, cancellation)?;
    let mut failure: c_int = 0;
    let mut size = std::mem::size_of::<c_int>() as u32;
    if unsafe { getsockopt(raw, 1, 4, (&mut failure as *mut c_int).cast(), &mut size) } < 0
        || failure != 0
    {
        return Err("unsupported_state: native window capture socket connection failed".into());
    }
    check(deadline, cancellation)?;
    Ok(fd)
}

fn peer_identity(fd: c_int) -> Result<Peer> {
    let mut peer = Peer::default();
    let mut size = std::mem::size_of::<Peer>() as u32;
    // Linux SOL_SOCKET / SO_PEERCRED binds IPC and Wayland to one compositor.
    if unsafe { getsockopt(fd, 1, 17, (&mut peer as *mut Peer).cast(), &mut size) } < 0
        || size != std::mem::size_of::<Peer>() as u32
        || peer.pid <= 0
    {
        return Err(
            "unsupported_state: cannot verify the native capture compositor identity".into(),
        );
    }
    Ok(peer)
}

fn same_compositor(expected: Peer, actual: Peer) -> Result<()> {
    if expected != actual {
        return Err("stale_target: Hyprland IPC and Wayland capture do not belong to the same compositor process".into());
    }
    Ok(())
}

fn observe(
    socket_path: &Path,
    id: &str,
    expected: Option<Peer>,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<(CaptureRegion, Peer)> {
    let fd = local_socket(socket_path, deadline, cancellation)?;
    let peer = peer_identity(fd.as_raw_fd())?;
    if let Some(expected) = expected {
        same_compositor(expected, peer)?;
    }
    let mut stream = UnixStream::from(fd);
    let command: &[u8] = if id == "active" {
        b"j/activewindow"
    } else {
        b"j/clients"
    };
    let mut sent = 0;
    while sent < command.len() {
        check(deadline, cancellation)?;
        match stream.write(&command[sent..]) {
            Ok(0) => {
                return Err(
                    "unsupported_state: native capture IPC disconnected during request".into(),
                );
            }
            Ok(count) => sent += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), 4, deadline, cancellation)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(format!(
                    "unsupported_state: native capture IPC write failed: {error}"
                ));
            }
        }
    }
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        check(deadline, cancellation)?;
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if bytes.len() + count > 1_048_576 {
                    return Err(
                        "unsupported_state: native capture IPC response exceeds 1 MiB".into(),
                    );
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), 1, deadline, cancellation)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(format!(
                    "unsupported_state: native capture IPC read failed: {error}"
                ));
            }
        }
    }
    let snapshot = std::str::from_utf8(&bytes)
        .map_err(|_| "unsupported_state: native capture IPC did not return UTF-8")?;
    let region = CaptureRegion::from_snapshot(snapshot, id).map_err(|error| {
        format!("stale_target: cannot observe the requested capture window: {error}")
    })?;
    Ok((region, peer))
}

impl<'a> Connection<'a> {
    fn open(
        display_path: Option<&Path>,
        deadline: Instant,
        cancellation: &'a Cancellation,
    ) -> Result<Self> {
        check(deadline, cancellation)?;
        let path = match display_path {
            Some(path) => path.to_path_buf(),
            None => {
                if std::env::var_os("WAYLAND_SOCKET").is_some() {
                    return Err("unsupported_state: surface-bound capture requires an owned Wayland connection, not an inherited WAYLAND_SOCKET fd".into());
                }
                let display = PathBuf::from(
                    std::env::var_os("WAYLAND_DISPLAY")
                        .filter(|value| !value.is_empty())
                        .ok_or("unsupported_state: WAYLAND_DISPLAY is unavailable")?,
                );
                if display.is_absolute() {
                    display
                } else {
                    PathBuf::from(
                        std::env::var_os("XDG_RUNTIME_DIR").ok_or(
                            "unsupported_state: the Wayland runtime directory is unavailable",
                        )?,
                    )
                    .join(display)
                }
            }
        };
        let fd = local_socket(&path, deadline, cancellation)?;
        let peer = peer_identity(fd.as_raw_fd())?;
        let library = Library::open(c"libwayland-client.so.0").map_err(|error| {
            format!("unsupported_state: surface-bound capture needs libwayland-client: {error}")
        })?;
        let api = Api::load(&library).map_err(|error| {
            format!("unsupported_state: native window capture ABI is unavailable: {error}")
        })?;
        // libwayland takes ownership of this descriptor, including on failure.
        let display = unsafe { (api.connect_fd)(fd.into_raw_fd()) };
        if display.is_null() {
            return Err(
                "unsupported_state: cannot connect to Wayland for surface-bound window capture"
                    .into(),
            );
        }
        let mut this = Self {
            api,
            display,
            owned: Vec::new(),
            registry: ptr::null_mut(),
            shm: ptr::null_mut(),
            export: ptr::null_mut(),
            state: Box::new(State {
                api,
                globals: [None; 4],
                mapping: ptr::null_mut(),
                tops: Vec::new(),
                error: None,
            }),
            selected: None,
            frame: None,
            buffer: None,
            peer,
            deadline,
            cancellation,
            _library: library,
        };
        this.registry = this.own(unsafe {
            (api.marshal)(display, 1, api.registry, 1, 0, ptr::null_mut::<c_void>())
        })?;
        let state_data = (&mut *this.state as *mut State).cast();
        this.listen(this.registry, &REGISTRY_LISTENER, state_data)?;
        this.roundtrip()?;
        let globals = required_globals(&this.state.globals)?;
        this.shm = this.bind(globals[0], api.shm, 1)?;
        this.listen(this.shm, &SHM_LISTENER, ptr::null_mut())?;
        this.export = this.bind(globals[3], &EXPORT_INTERFACE, 2)?;
        this.state.mapping = this.bind(globals[2], &MAP_INTERFACE, 1)?;
        let foreign = this.bind(globals[1], &FOREIGN_INTERFACE, 1)?;
        this.listen(foreign, &FOREIGN_LISTENER, state_data)?;
        // Binding sends the initial list before this display-sync callback.
        // Mapping requests made by new_top can finish after that callback.
        this.roundtrip()?;
        this.wait(|connection| {
            connection
                .state
                .tops
                .iter()
                .all(|top| top.closed || (top.announced && top.mapping_done))
        })?;
        Ok(this)
    }

    fn own(&mut self, object: Proxy) -> Result<Proxy> {
        if object.is_null() {
            Err("unsupported_state: Wayland window capture object creation failed".into())
        } else {
            self.owned.push(object);
            Ok(object)
        }
    }

    fn listen<const N: usize>(
        &self,
        object: Proxy,
        listener: &Listener<N>,
        data: Proxy,
    ) -> Result<()> {
        if object.is_null()
            || unsafe { (self.api.add_listener)(object, listener.0.as_ptr(), data) } != 0
        {
            return Err(
                "unsupported_state: Wayland window capture listener registration failed".into(),
            );
        }
        Ok(())
    }

    fn bind(&mut self, global: Global, interface: *const Interface, version: u32) -> Result<Proxy> {
        let object = unsafe {
            (self.api.marshal)(
                self.registry,
                0,
                interface,
                version,
                0,
                global.name,
                (*interface).name,
                version,
                ptr::null_mut::<c_void>(),
            )
        };
        self.own(object)
    }

    fn check_state(&self) -> Result<()> {
        check(self.deadline, self.cancellation)?;
        if let Some(error) = self.state.error {
            return Err(error.into());
        }
        if self
            .selected
            .is_some_and(|index| self.state.tops[index].closed)
        {
            return Err("stale_target: the selected Wayland toplevel closed during capture".into());
        }
        if let Some(error) = self.frame.as_ref().and_then(|frame| frame.error) {
            return Err(error.into());
        }
        Ok(())
    }

    fn wait(&mut self, done: impl Fn(&Self) -> bool) -> Result<()> {
        loop {
            self.check_state()?;
            if unsafe { (self.api.dispatch_pending)(self.display) } < 0 {
                return Err("unsupported_state: Wayland window capture connection failed".into());
            }
            self.check_state()?;
            if done(self) {
                return Ok(());
            }
            if unsafe { (self.api.prepare_read)(self.display) } != 0 {
                continue;
            }
            let flush = unsafe { (self.api.flush)(self.display) };
            let needs_write =
                flush < 0 && std::io::Error::last_os_error().raw_os_error() == Some(11);
            if flush < 0 && !needs_write {
                unsafe {
                    (self.api.cancel_read)(self.display);
                }
                return Err("unsupported_state: Wayland window capture write failed".into());
            }
            // Cancellation is checked at least every 25 ms while awaiting
            // protocol events, including an off-screen compositor stall.
            let remaining = self
                .deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 25) as c_int;
            let mut pfd = PollFd {
                fd: unsafe { (self.api.fd)(self.display) },
                events: 1 | if needs_write { 4 } else { 0 },
                revents: 0,
            };
            let ready = unsafe { poll(&mut pfd, 1, remaining) };
            if ready > 0 && pfd.revents & (8 | 16 | 32) != 0 {
                unsafe {
                    (self.api.cancel_read)(self.display);
                }
                return Err(
                    "unsupported_state: Wayland window capture display disconnected".into(),
                );
            }
            if ready > 0 && pfd.revents & 1 != 0 {
                if unsafe { (self.api.read_events)(self.display) } < 0 {
                    return Err("unsupported_state: Wayland window capture read failed".into());
                }
            } else {
                unsafe {
                    (self.api.cancel_read)(self.display);
                }
                if ready < 0 && std::io::Error::last_os_error().raw_os_error() != Some(4) {
                    return Err("unsupported_state: Wayland window capture polling failed".into());
                }
            }
        }
    }

    fn roundtrip(&mut self) -> Result<()> {
        let done = Cell::new(false);
        let callback = unsafe {
            (self.api.marshal)(
                self.display,
                0,
                self.api.callback,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if callback.is_null() {
            return Err("unsupported_state: Wayland window capture sync creation failed".into());
        }
        let result = self
            .listen(
                callback,
                &SYNC_LISTENER,
                (&done as *const Cell<bool>).cast_mut().cast(),
            )
            .and_then(|_| self.wait(|_| done.get()));
        // End the proxy before its listener's stack-owned Cell expires.
        unsafe {
            (self.api.destroy)(callback);
        }
        result
    }

    fn select(&mut self, address: u64) -> Result<()> {
        self.check_state()?;
        self.selected = Some(self.state.selected(address)?);
        Ok(())
    }

    fn verify_mapping(&mut self, expected: u64) -> Result<()> {
        self.check_state()?;
        let index = self.selected.ok_or("stale_target: no selected toplevel")?;
        let top = &mut self.state.tops[index];
        unsafe {
            (self.api.marshal)(top.mapping, 0, ptr::null(), 1, 1);
        }
        top.address = None;
        top.mapping_done = false;
        top.mapping = unsafe {
            (self.api.marshal)(
                self.state.mapping,
                1,
                &MAP_HANDLE_INTERFACE,
                1,
                0,
                ptr::null_mut::<c_void>(),
                top.object,
            )
        };
        let object = top.mapping;
        let data = (&mut **top as *mut Top).cast();
        self.listen(object, &MAPPING_LISTENER, data)?;
        self.wait(|connection| connection.state.tops[index].mapping_done)?;
        if self.state.tops[index].address != Some(expected) {
            return Err("stale_target: the retained toplevel no longer maps to the requested full window address".into());
        }
        Ok(())
    }

    fn copy(&mut self) -> Result<(BufferSpec, Vec<u8>, bool)> {
        self.check_state()?;
        let index = self.selected.ok_or("stale_target: no selected toplevel")?;
        let object = self.state.tops[index].object;
        let mut frame = Box::<Frame>::default();
        frame.object = unsafe {
            (self.api.marshal)(
                self.export,
                2,
                &FRAME_INTERFACE,
                2,
                0,
                ptr::null_mut::<c_void>(),
                0i32,
                object,
            )
        };
        let frame_data = (&mut *frame as *mut Frame).cast();
        let frame_object = frame.object;
        self.frame = Some(frame);
        self.listen(frame_object, &FRAME_LISTENER, frame_data)?;
        self.wait(|connection| {
            connection
                .frame
                .as_ref()
                .is_some_and(|frame| frame.buffers_done)
        })?;
        let spec =
            self.frame.as_ref().and_then(|frame| frame.spec).ok_or(
                "unsupported_state: compositor did not offer a shared-memory toplevel buffer",
            )?;
        let bytes = spec.bytes()?;
        self.create_buffer(spec, bytes)?;
        let buffer = self
            .buffer
            .as_ref()
            .ok_or("missing toplevel capture buffer")?
            .object;
        self.frame
            .as_mut()
            .ok_or("missing toplevel capture frame")?
            .copy_sent = true;
        unsafe {
            (self.api.marshal)(frame_object, 0, ptr::null(), 2, 0, buffer, 1i32);
        }
        self.wait(|connection| connection.frame.as_ref().is_some_and(|frame| frame.ready))?;
        self.check_state()?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(bytes)
            .map_err(|_| "cannot allocate bounded toplevel capture pixels")?;
        pixels.resize(bytes, 0);
        let file = &mut self
            .buffer
            .as_mut()
            .ok_or("missing toplevel capture buffer")?
            .file;
        if file.metadata().map_err(|e| e.to_string())?.len() != bytes as u64 {
            return Err("unsupported_state: toplevel shared-memory buffer changed size".into());
        }
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        file.read_exact(&mut pixels)
            .map_err(|e| format!("read toplevel shared memory: {e}"))?;
        self.check_state()?;
        let inverted = self
            .frame
            .as_ref()
            .and_then(|frame| frame.flags)
            .ok_or("unsupported_state: missing toplevel orientation flags")?
            & 1
            != 0;
        Ok((spec, pixels, inverted))
    }

    fn create_buffer(&mut self, spec: BufferSpec, bytes: usize) -> Result<()> {
        self.check_state()?;
        let fd = unsafe { memfd_create(c"zuse-window-capture".as_ptr(), 1) };
        if fd < 0 {
            return Err(format!(
                "create toplevel shared memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: this function takes exclusive ownership of the returned fd.
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(bytes as u64)
            .map_err(|e| format!("size toplevel shared memory: {e}"))?;
        let pool = unsafe {
            (self.api.marshal)(
                self.shm,
                0,
                self.api.pool,
                1,
                0,
                ptr::null_mut::<c_void>(),
                file.as_raw_fd(),
                bytes as i32,
            )
        };
        if pool.is_null() {
            return Err("unsupported_state: toplevel shared-memory pool creation failed".into());
        }
        let object = unsafe {
            (self.api.marshal)(
                pool,
                0,
                self.api.buffer,
                1,
                0,
                ptr::null_mut::<c_void>(),
                0i32,
                spec.width as i32,
                spec.height as i32,
                spec.stride as i32,
                spec.format,
            )
        };
        // A wl_buffer retains the pool mapping on the compositor after this.
        unsafe {
            (self.api.marshal)(pool, 1, ptr::null(), 1, 1);
        }
        self.buffer = Some(Buffer { object, file });
        self.listen(object, &BUFFER_LISTENER, ptr::null_mut())
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        // Disconnecting destroys the server resources. Destroy local proxies
        // first, without dispatching any callback after its data expires.
        unsafe {
            if let Some(buffer) = &self.buffer
                && !buffer.object.is_null()
            {
                (self.api.destroy)(buffer.object);
            }
            if let Some(frame) = &self.frame
                && !frame.object.is_null()
            {
                (self.api.destroy)(frame.object);
            }
            for top in &self.state.tops {
                if !top.mapping.is_null() {
                    (self.api.destroy)(top.mapping);
                }
                if !top.object.is_null() {
                    (self.api.destroy)(top.object);
                }
            }
            for object in self.owned.drain(..).rev() {
                (self.api.destroy)(object);
            }
            (self.api.disconnect)(self.display);
        }
    }
}

fn required_globals(globals: &[Option<Global>; 4]) -> Result<[Global; 4]> {
    let names = [
        "wl_shm",
        "zwlr_foreign_toplevel_manager_v1",
        "hyprland_toplevel_mapping_manager_v1",
        "hyprland_toplevel_export_manager_v1 version 2",
    ];
    let mut found = [Global {
        name: 0,
        version: 0,
    }; 4];
    for index in 0..4 {
        found[index] = globals[index].filter(|global| global.version >= if index == 3 { 2 } else { 1 })
            .ok_or_else(|| format!("unsupported_state: surface-bound window capture requires {}; no desktop-region fallback is available", names[index]))?;
    }
    Ok(found)
}

fn address(value: &str) -> Result<u64> {
    let digits = value
        .strip_prefix("0x")
        .ok_or("invalid Hyprland window address")?;
    if digits.is_empty()
        || digits.len() > 16
        || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("invalid Hyprland window address".into());
    }
    u64::from_str_radix(digits, 16)
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| "invalid Hyprland window address".into())
}

struct Output {
    temporary: PathBuf,
}
impl Output {
    fn new(path: &Path) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if path.file_name().is_none() {
            return Err("window capture requires an output file".into());
        }
        for _ in 0..16 {
            let temporary = directory.join(format!(
                ".zuse-capture-{}-{}-{}.png",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(_) => return Ok(Self { temporary }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("create private window capture output: {error}")),
            }
        }
        Err("cannot reserve a private window capture output".into())
    }
    fn publish(&self, path: &Path) -> Result<()> {
        std::fs::rename(&self.temporary, path)
            .map_err(|e| format!("publish window capture PNG: {e}"))
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.temporary);
    }
}

pub(super) fn capture(
    path: &Path,
    id: &str,
    socket_path: &Path,
    cancellation: &Cancellation,
) -> Result<String> {
    let deadline = Instant::now() + TIMEOUT;
    check(deadline, cancellation)?;
    super::Wayland::selector(id)?;
    let (before, peer) = observe(socket_path, id, None, deadline, cancellation)?;
    let full_address = address(&before.window)?;
    if before.mapped != Some(true) || !before.pid.is_some_and(|pid| pid > 0) {
        return Err("unsupported_state: surface-bound capture requires a mapped window with an observed process identity".into());
    }
    let mut connection = Connection::open(None, deadline, cancellation)?;
    same_compositor(peer, connection.peer)?;
    connection.select(full_address)?;
    before.verify(
        &observe(
            socket_path,
            &before.window,
            Some(peer),
            deadline,
            cancellation,
        )?
        .0,
    )?;
    let (spec, pixels, inverted) = connection.copy()?;
    connection.verify_mapping(full_address)?;
    before.verify(
        &observe(
            socket_path,
            &before.window,
            Some(peer),
            deadline,
            cancellation,
        )?
        .0,
    )?;
    connection.check_state()?;
    let output = Output::new(path)?;
    let encoded = crate::platform::png::write_png_checked(
        &output.temporary,
        spec.width,
        spec.height,
        |x, y| spec.pixel(&pixels, x, y, inverted),
        || check(deadline, cancellation),
    );
    check(deadline, cancellation)?;
    encoded?;
    // Encoding does not dispatch Wayland events. Re-map the retained object
    // afterwards so a queued close or compositor replacement cannot publish.
    connection.verify_mapping(full_address)?;
    before.verify(
        &observe(
            socket_path,
            &before.window,
            Some(peer),
            deadline,
            cancellation,
        )?
        .0,
    )?;
    connection.check_state()?;
    let metadata = super::capture_metadata(path, spec.width, spec.height, Some(&before))?;
    output.publish(path)?;
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixListener, sync::mpsc, thread};

    fn top(address: u64) -> Top {
        Top {
            object: ptr::null_mut(),
            mapping: ptr::null_mut(),
            address: Some(address),
            mapping_done: true,
            announced: true,
            closed: false,
        }
    }

    #[test]
    fn complete_address_mapping_never_matches_a_truncated_handle() {
        let full = 0x5576_98c3_97b0;
        assert_eq!(address("0x557698c397b0").unwrap(), full);
        for bad in [
            "",
            "active",
            "0",
            "0x",
            "0x0",
            "0x-1",
            "0x10000000000000000",
            "0x123z",
        ] {
            assert!(address(bad).is_err(), "{bad}");
        }
        let mut tops = vec![top(full), top(0xabcd_98c3_97b0), top(0x98c3_97b0)];
        assert_eq!(selected_toplevel(tops.iter().enumerate(), full).unwrap(), 0);
        tops[0].closed = true;
        assert!(
            selected_toplevel(tops.iter().enumerate(), full)
                .unwrap_err()
                .starts_with("stale_target:")
        );
        tops[0].closed = false;
        tops.push(top(full));
        assert!(
            selected_toplevel(tops.iter().enumerate(), full)
                .unwrap_err()
                .starts_with("ambiguous_target:")
        );
        let mut mapped_top = top(1);
        unsafe {
            mapped(
                (&mut mapped_top as *mut Top).cast(),
                ptr::null_mut(),
                0x5576,
                0x98c3_97b0,
            );
        }
        assert_eq!(mapped_top.address, Some(full));
    }

    #[test]
    fn all_surface_identity_protocols_and_export_v2_are_required() {
        let complete = [
            Some(Global {
                name: 1,
                version: 1,
            }),
            Some(Global {
                name: 2,
                version: 3,
            }),
            Some(Global {
                name: 3,
                version: 1,
            }),
            Some(Global {
                name: 4,
                version: 2,
            }),
        ];
        assert!(required_globals(&complete).is_ok());
        for index in 0..4 {
            let mut missing = complete;
            missing[index] = None;
            let error = required_globals(&missing).err().unwrap();
            assert!(error.starts_with("unsupported_state:"));
            assert!(error.contains("no desktop-region fallback"));
        }
        let mut legacy = complete;
        legacy[3].as_mut().unwrap().version = 1;
        assert!(
            required_globals(&legacy)
                .err()
                .unwrap()
                .contains("version 2")
        );
        let peer = Peer {
            pid: 123,
            uid: 1000,
            gid: 1000,
        };
        assert!(same_compositor(peer, peer).is_ok());
        for changed in [
            Peer { pid: 124, ..peer },
            Peer { uid: 1001, ..peer },
            Peer { gid: 1001, ..peer },
        ] {
            assert!(
                same_compositor(peer, changed)
                    .unwrap_err()
                    .starts_with("stale_target:")
            );
        }
    }

    #[test]
    fn shm_bounds_channels_padding_and_orientation_are_checked() {
        let base = BufferSpec {
            format: 0,
            width: 2,
            height: 2,
            stride: 12,
        };
        assert_eq!(base.bytes().unwrap(), 24);
        assert!(BufferSpec { format: 1, ..base }.bytes().is_ok());
        for bad in [
            BufferSpec { format: 2, ..base },
            BufferSpec { width: 0, ..base },
            BufferSpec { height: 0, ..base },
            BufferSpec {
                width: 16_385,
                ..base
            },
            BufferSpec {
                height: 16_385,
                ..base
            },
            BufferSpec { stride: 7, ..base },
            BufferSpec { stride: 4, ..base },
            BufferSpec {
                stride: u32::MAX - 3,
                ..base
            },
            BufferSpec {
                width: 16384,
                height: 16384,
                stride: 65536,
                ..base
            },
        ] {
            assert!(bad.bytes().is_err(), "{bad:?}");
        }
        let mut bytes = [0; 24];
        bytes[..4].copy_from_slice(&0xff12_3456u32.to_ne_bytes());
        bytes[12..16].copy_from_slice(&0xffab_cdefu32.to_ne_bytes());
        assert_eq!(base.pixel(&bytes, 0, 0, false), [0x12, 0x34, 0x56]);
        assert_eq!(base.pixel(&bytes, 0, 0, true), [0xab, 0xcd, 0xef]);
        assert_eq!(base.pixel(&bytes, 0, 1, true), [0x12, 0x34, 0x56]);
    }

    #[test]
    fn frame_events_require_a_single_buffer_copy_flags_and_ready_sequence() {
        let mut frame = Frame::default();
        let data = (&mut frame as *mut Frame).cast();
        unsafe {
            frame_buffer(data, ptr::null_mut(), 1, 2, 2, 8);
            frame_buffers_done(data, ptr::null_mut());
        }
        assert!(frame.error.is_none());
        frame.copy_sent = true;
        unsafe {
            frame_flags(data, ptr::null_mut(), 1);
            frame_ready(data, ptr::null_mut(), 0, 1, 10);
        }
        assert!(frame.ready && frame.error.is_none());
        unsafe {
            frame_ready(data, ptr::null_mut(), 0, 1, 10);
        }
        assert!(frame.error.is_some());
        for case in 0..5 {
            let mut invalid = Frame::default();
            let data = (&mut invalid as *mut Frame).cast();
            unsafe {
                match case {
                    0 => frame_ready(data, ptr::null_mut(), 0, 1, 0),
                    1 => {
                        invalid.copy_sent = true;
                        frame_ready(data, ptr::null_mut(), 0, 1, 0);
                    }
                    2 => {
                        invalid.copy_sent = true;
                        frame_flags(data, ptr::null_mut(), 2);
                    }
                    3 => {
                        frame_buffers_done(data, ptr::null_mut());
                        frame_buffer(data, ptr::null_mut(), 1, 1, 1, 4);
                    }
                    _ => {
                        frame_buffers_done(data, ptr::null_mut());
                        frame_buffers_done(data, ptr::null_mut());
                    }
                }
            }
            assert!(invalid.error.is_some(), "invalid event case {case}");
        }
    }

    fn temp_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "zuse-cap-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn cancelled_capture_and_failed_encoding_preserve_the_previous_file() {
        let directory = temp_directory("output");
        let path = directory.join("window.png");
        std::fs::write(&path, b"previous capture").unwrap();
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let error =
            capture(&path, "0x123", &directory.join("absent-ipc"), &cancellation).unwrap_err();
        assert!(error.starts_with("cancelled:"));
        assert_eq!(std::fs::read(&path).unwrap(), b"previous capture");
        let output = Output::new(&path).unwrap();
        let temporary = output.temporary.clone();
        let calls = Cell::new(0);
        let result = crate::platform::png::write_png_checked(
            &temporary,
            2,
            2,
            |_, _| [32; 3],
            || {
                calls.set(calls.get() + 1);
                if calls.get() >= 3 {
                    Err("cancelled: fixture encoding".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        drop(output);
        assert!(!temporary.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"previous capture");
        std::fs::remove_dir_all(directory).unwrap();
    }

    // Wire fixture exercises our real libwayland-client event loop without
    // connecting to a desktop. No compositor, GUI process or input is used.
    struct WireFixture {
        directory: PathBuf,
        path: PathBuf,
        stop: mpsc::Sender<()>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl WireFixture {
        fn new(stall: bool) -> Self {
            let directory = temp_directory("wire");
            let path = directory.join("display");
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let (stop, stopped) = mpsc::channel();
            let thread = thread::spawn(move || {
                let until = Instant::now() + Duration::from_secs(2);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if stopped.try_recv().is_ok() || Instant::now() >= until {
                                return;
                            }
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("wire fixture accept: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                for _ in 0..2 {
                    let mut header = [0; 8];
                    if stream.read_exact(&mut header).is_err() {
                        return;
                    }
                    let object = u32::from_ne_bytes(header[..4].try_into().unwrap());
                    let word = u32::from_ne_bytes(header[4..].try_into().unwrap());
                    assert_eq!(object, 1);
                    let size = (word >> 16) as usize;
                    assert_eq!(size, 12);
                    let mut payload = [0; 4];
                    stream.read_exact(&mut payload).unwrap();
                    if word & 0xffff == 0 && !stall {
                        let callback = u32::from_ne_bytes(payload);
                        let mut done = callback.to_ne_bytes().to_vec();
                        done.extend_from_slice(&(12u32 << 16).to_ne_bytes());
                        done.extend_from_slice(&0u32.to_ne_bytes());
                        stream.write_all(&done).unwrap();
                    }
                }
                let _ = stopped.recv_timeout(Duration::from_secs(2));
            });
            Self {
                directory,
                path,
                stop,
                thread: Some(thread),
            }
        }
    }
    impl Drop for WireFixture {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn native_connection_reports_missing_protocol_without_fallback() {
        if Library::open(c"libwayland-client.so.0").is_err() {
            return;
        }
        let fixture = WireFixture::new(false);
        let cancellation = Cancellation::default();
        let error = Connection::open(
            Some(&fixture.path),
            Instant::now() + Duration::from_secs(1),
            &cancellation,
        )
        .err()
        .expect("an empty registry must refuse window capture");
        assert!(error.starts_with("unsupported_state:"), "{error}");
        assert!(error.contains("requires wl_shm"), "{error}");
        assert!(error.contains("no desktop-region fallback"));
    }

    #[test]
    fn native_connection_wait_has_a_total_deadline() {
        if Library::open(c"libwayland-client.so.0").is_err() {
            return;
        }
        let fixture = WireFixture::new(true);
        let cancellation = Cancellation::default();
        let started = Instant::now();
        let error = Connection::open(
            Some(&fixture.path),
            started + Duration::from_millis(150),
            &cancellation,
        )
        .err()
        .expect("a stalled protocol must time out");
        assert!(error.starts_with("timeout:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn native_connection_wait_observes_runtime_cancellation() {
        if Library::open(c"libwayland-client.so.0").is_err() {
            return;
        }
        let fixture = WireFixture::new(true);
        let cancellation = Cancellation::default();
        let cancel = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        let started = Instant::now();
        let error = Connection::open(
            Some(&fixture.path),
            started + Duration::from_secs(2),
            &cancellation,
        )
        .err()
        .expect("a stalled protocol must observe cancellation");
        canceller.join().unwrap();
        assert!(error.starts_with("cancelled:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
