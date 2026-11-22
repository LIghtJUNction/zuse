//! Native data-control ownership and bounded transfer on one private thread.
use super::{MAX_BYTES, Result, validate_text};
use crate::{observation::Cancellation, platform::dylib::Library};
use std::{
    cell::Cell,
    collections::BTreeMap,
    ffi::{CStr, c_char, c_int, c_void},
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    os::unix::ffi::OsStrExt,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type Proxy = *mut c_void;
const OP_TIMEOUT: Duration = Duration::from_secs(3);
const TICK: Duration = Duration::from_millis(20);
const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;
const O_NONBLOCK: c_int = 0x800;
const O_CLOEXEC: c_int = 0x80000;
const MIMES: [&CStr; 3] = [c"text/plain;charset=utf-8", c"UTF8_STRING", c"text/plain"];

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
// SAFETY: immutable descriptors contain only static protocol pointers.
unsafe impl Sync for Message {}
unsafe impl Sync for Interface {}
struct Types<const N: usize>([*const Interface; N]);
unsafe impl<const N: usize> Sync for Types<N> {}
static NO_TYPES: Types<3> = Types([ptr::null(); 3]);

struct Protocol {
    manager: &'static Interface,
    device: &'static Interface,
    source: &'static Interface,
    name: &'static str,
}

// Request/event order follows ext-data-control-v1 and wlr-data-control-v1.
// Wlr is deliberately bound at v1: it never changes primary selection.
macro_rules! protocol {
    ($module:ident, $manager:literal, $device:literal, $source:literal, $offer:literal, $primary:literal) => {
        mod $module {
            use super::*;
            static OFFER_METHODS: [Message; 2] = [
                Message {
                    name: c"receive".as_ptr(),
                    signature: c"sh".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"destroy".as_ptr(),
                    signature: c"".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
            ];
            static OFFER_EVENTS: [Message; 1] = [Message {
                name: c"offer".as_ptr(),
                signature: c"s".as_ptr(),
                types: NO_TYPES.0.as_ptr(),
            }];
            static OFFER: Interface = Interface {
                name: $offer.as_ptr(),
                version: 1,
                method_count: 2,
                methods: OFFER_METHODS.as_ptr(),
                event_count: 1,
                events: OFFER_EVENTS.as_ptr(),
            };
            static SOURCE_METHODS: [Message; 2] = [
                Message {
                    name: c"offer".as_ptr(),
                    signature: c"s".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"destroy".as_ptr(),
                    signature: c"".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
            ];
            static SOURCE_EVENTS: [Message; 2] = [
                Message {
                    name: c"send".as_ptr(),
                    signature: c"sh".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"cancelled".as_ptr(),
                    signature: c"".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
            ];
            static SOURCE: Interface = Interface {
                name: $source.as_ptr(),
                version: 1,
                method_count: 2,
                methods: SOURCE_METHODS.as_ptr(),
                event_count: 2,
                events: SOURCE_EVENTS.as_ptr(),
            };
            static SOURCE_TYPES: Types<1> = Types([&SOURCE]);
            static OFFER_TYPES: Types<1> = Types([&OFFER]);
            static DEVICE_METHODS: [Message; 3] = [
                Message {
                    name: c"set_selection".as_ptr(),
                    signature: c"?o".as_ptr(),
                    types: SOURCE_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"destroy".as_ptr(),
                    signature: c"".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"set_primary_selection".as_ptr(),
                    signature: $primary.as_ptr(),
                    types: SOURCE_TYPES.0.as_ptr(),
                },
            ];
            static DEVICE_EVENTS: [Message; 4] = [
                Message {
                    name: c"data_offer".as_ptr(),
                    signature: c"n".as_ptr(),
                    types: OFFER_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"selection".as_ptr(),
                    signature: c"?o".as_ptr(),
                    types: OFFER_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"finished".as_ptr(),
                    signature: c"".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"primary_selection".as_ptr(),
                    signature: $primary.as_ptr(),
                    types: OFFER_TYPES.0.as_ptr(),
                },
            ];
            static DEVICE: Interface = Interface {
                name: $device.as_ptr(),
                version: 1,
                method_count: 3,
                methods: DEVICE_METHODS.as_ptr(),
                event_count: 4,
                events: DEVICE_EVENTS.as_ptr(),
            };
            static DEVICE_TYPES: Types<2> = Types([&DEVICE, ptr::null()]);
            static MANAGER_METHODS: [Message; 3] = [
                Message {
                    name: c"create_data_source".as_ptr(),
                    signature: c"n".as_ptr(),
                    types: SOURCE_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"get_data_device".as_ptr(),
                    signature: c"no".as_ptr(),
                    types: DEVICE_TYPES.0.as_ptr(),
                },
                Message {
                    name: c"destroy".as_ptr(),
                    signature: c"".as_ptr(),
                    types: NO_TYPES.0.as_ptr(),
                },
            ];
            static MANAGER: Interface = Interface {
                name: $manager.as_ptr(),
                version: 1,
                method_count: 3,
                methods: MANAGER_METHODS.as_ptr(),
                event_count: 0,
                events: ptr::null(),
            };
            pub(super) static PROTOCOL: Protocol = Protocol {
                manager: &MANAGER,
                device: &DEVICE,
                source: &SOURCE,
                name: stringify!($module),
            };
        }
    };
}
protocol!(
    ext_data_control_v1,
    c"ext_data_control_manager_v1",
    c"ext_data_control_device_v1",
    c"ext_data_control_source_v1",
    c"ext_data_control_offer_v1",
    c"?o"
);
protocol!(
    wlr_data_control_v1,
    c"zwlr_data_control_manager_v1",
    c"zwlr_data_control_device_v1",
    c"zwlr_data_control_source_v1",
    c"zwlr_data_control_offer_v1",
    c"2?o"
);

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
// Match the Linux sigset_t ABI declaration in script::interrupt as well as
// libc's 128-byte storage; one C symbol must not have conflicting Rust types.
type SigSet = [u64; 16];
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn connect(fd: c_int, address: *const c_void, length: u32) -> c_int;
    fn getsockopt(
        fd: c_int,
        level: c_int,
        option: c_int,
        value: *mut c_void,
        length: *mut u32,
    ) -> c_int;
    fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    fn fcntl(fd: c_int, operation: c_int, ...) -> c_int;
    fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize;
    fn sigemptyset(set: *mut SigSet) -> c_int;
    fn sigaddset(set: *mut SigSet, signal: c_int) -> c_int;
    fn pthread_sigmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
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
    registry_interface: *const Interface,
    seat_interface: *const Interface,
    callback_interface: *const Interface,
}
impl Api {
    fn load(library: &Library) -> Result<Self> {
        // SAFETY: exact public libwayland-client ABI; owner outlives all proxies.
        unsafe {
            Ok(Self {
                connect_fd: library.symbol(c"wl_display_connect_to_fd")?,
                disconnect: library.symbol(c"wl_display_disconnect")?,
                fd: library.symbol(c"wl_display_get_fd")?,
                flush: library.symbol(c"wl_display_flush")?,
                dispatch_pending: library.symbol(c"wl_display_dispatch_pending")?,
                prepare_read: library.symbol(c"wl_display_prepare_read")?,
                read_events: library.symbol(c"wl_display_read_events")?,
                cancel_read: library.symbol(c"wl_display_cancel_read")?,
                destroy: library.symbol(c"wl_proxy_destroy")?,
                add_listener: library.symbol(c"wl_proxy_add_listener")?,
                marshal: library.symbol(c"wl_proxy_marshal_flags")?,
                registry_interface: library.symbol(c"wl_registry_interface")?,
                seat_interface: library.symbol(c"wl_seat_interface")?,
                callback_interface: library.symbol(c"wl_callback_interface")?,
            })
        }
    }
    unsafe fn dispose(&self, proxy: Proxy, opcode: u32) {
        if !proxy.is_null() {
            unsafe {
                (self.marshal)(proxy, opcode, ptr::null(), 1, 1);
            }
        }
    }
}

fn check(deadline: Instant, cancellation: Option<&Cancellation>, stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        return Err("clipboard owner is stopping".into());
    }
    if let Some(token) = cancellation {
        token.check()?;
    }
    if Instant::now() >= deadline {
        return Err("clipboard operation exceeded its 3 second deadline".into());
    }
    Ok(())
}

fn nonblocking(fd: c_int) -> Result<()> {
    let flags = unsafe { fcntl(fd, 3) }; // F_GETFL
    if flags < 0
        || unsafe { fcntl(fd, 4, flags | O_NONBLOCK) } < 0
        || unsafe { fcntl(fd, 2, 1) } < 0
    {
        // F_SETFL, F_SETFD(FD_CLOEXEC)
        return Err(format!(
            "configure clipboard transfer fd: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn display_socket(
    path: &std::path::Path,
    deadline: Instant,
    cancellation: &Cancellation,
    stop: &AtomicBool,
) -> Result<OwnedFd> {
    let path = path.as_os_str().as_bytes();
    if path.is_empty() || path.len() >= 108 || path.contains(&0) {
        return Err("unsupported_state: Wayland display socket path is invalid or too long".into());
    }
    let raw = unsafe { socket(1, 1 | O_NONBLOCK | O_CLOEXEC, 0) }; // AF_UNIX, SOCK_STREAM
    if raw < 0 {
        return Err(format!(
            "create clipboard display socket: {}",
            io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut address = SockaddrUn {
        family: 1,
        path: [0; 108],
    };
    for (out, byte) in address.path.iter_mut().zip(path) {
        *out = *byte as c_char;
    }
    check(deadline, Some(cancellation), stop)?;
    let connected = unsafe {
        connect(
            raw,
            (&address as *const SockaddrUn).cast(),
            (2 + path.len() + 1) as u32,
        )
    };
    if connected == 0 {
        return Ok(fd);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(115) {
        return Err(format!("connect clipboard Wayland display: {error}"));
    }
    loop {
        check(deadline, Some(cancellation), stop)?;
        let mut pfd = PollFd {
            fd: raw,
            events: POLLOUT,
            revents: 0,
        };
        let ready = unsafe { poll(&mut pfd, 1, 20) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll clipboard display connection: {error}"));
        }
        if ready == 0 {
            continue;
        }
        let mut failure: c_int = 0;
        let mut size = std::mem::size_of::<c_int>() as u32;
        if unsafe { getsockopt(raw, 1, 4, (&mut failure as *mut c_int).cast(), &mut size) } < 0 {
            return Err(format!(
                "inspect clipboard display connection: {}",
                io::Error::last_os_error()
            ));
        }
        if failure != 0 {
            return Err(format!(
                "connect clipboard Wayland display: {}",
                io::Error::from_raw_os_error(failure)
            ));
        }
        return Ok(fd);
    }
}

struct Offer {
    proxy: Proxy,
    mimes: u8,
}
struct Transfer {
    fd: OwnedFd,
    bytes: Arc<[u8]>,
    sent: usize,
    deadline: Instant,
}
struct State {
    api: Api,
    seat_id: Option<u32>,
    ext_id: Option<u32>,
    wlr_id: Option<u32>,
    manager_id: Option<u32>,
    selection: Proxy,
    selection_seen: bool,
    generation: u64,
    offers: BTreeMap<usize, Offer>,
    sources: BTreeMap<usize, Arc<[u8]>>,
    transfers: Vec<Transfer>,
    error: Option<String>,
}
impl State {
    fn destroy_offer(&mut self, proxy: Proxy) {
        if let Some(offer) = self.offers.remove(&(proxy as usize)) {
            unsafe {
                self.api.dispose(offer.proxy, 1);
            }
        }
    }
    fn advance_transfers(&mut self) {
        self.transfers.retain_mut(|transfer| {
            if Instant::now() >= transfer.deadline {
                return false;
            }
            let remaining = &transfer.bytes[transfer.sent..];
            if remaining.is_empty() {
                return false;
            }
            let chunk = &remaining[..remaining.len().min(65_536)];
            let sent =
                unsafe { write(transfer.fd.as_raw_fd(), chunk.as_ptr().cast(), chunk.len()) };
            if sent >= 0 {
                transfer.sent += sent as usize;
                transfer.sent < transfer.bytes.len()
            } else {
                matches!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                )
            }
        });
    }
}

unsafe extern "C" fn global(data: Proxy, _: Proxy, id: u32, name: *const c_char, version: u32) {
    if name.is_null() || version == 0 {
        return;
    }
    let state = unsafe { &mut *data.cast::<State>() };
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"wl_seat" if state.seat_id.is_none() => state.seat_id = Some(id),
        b"wl_seat" if state.seat_id != Some(id) => {
            state.error =
                Some("unsupported_state: clipboard requires one unambiguous Wayland seat".into())
        }
        b"ext_data_control_manager_v1" => state.ext_id = Some(id),
        b"zwlr_data_control_manager_v1" => state.wlr_id = Some(id),
        _ => {}
    }
}
unsafe extern "C" fn global_remove(data: Proxy, _: Proxy, id: u32) {
    let state = unsafe { &mut *data.cast::<State>() };
    if state.seat_id == Some(id) || state.manager_id == Some(id) {
        state.error =
            Some("stale_target: Wayland clipboard seat or data-control manager was removed".into());
    }
}
unsafe extern "C" fn offer_mime(data: Proxy, proxy: Proxy, mime: *const c_char) {
    if mime.is_null() {
        return;
    }
    let state = unsafe { &mut *data.cast::<State>() };
    if let Some(offer) = state.offers.get_mut(&(proxy as usize)) {
        match unsafe { CStr::from_ptr(mime) }.to_bytes() {
            b"text/plain;charset=utf-8" => offer.mimes |= 1,
            b"UTF8_STRING" => offer.mimes |= 2,
            b"text/plain" => offer.mimes |= 4,
            b"text/plain;charset=UTF-8" => offer.mimes |= 8,
            _ => {}
        }
    }
}
unsafe extern "C" fn data_offer(data: Proxy, _: Proxy, proxy: Proxy) {
    let state = unsafe { &mut *data.cast::<State>() };
    if proxy.is_null() || state.offers.len() >= 64 {
        state.error = Some("clipboard data offers exceed the bounded protocol budget".into());
        if !proxy.is_null() {
            unsafe {
                state.api.dispose(proxy, 1);
            }
        }
        return;
    }
    state
        .offers
        .insert(proxy as usize, Offer { proxy, mimes: 0 });
    if unsafe { (state.api.add_listener)(proxy, OFFER_LISTENER.0.as_ptr(), data) } != 0 {
        state.error = Some("cannot listen to clipboard MIME offer".into());
    }
}
unsafe extern "C" fn selection(data: Proxy, _: Proxy, proxy: Proxy) {
    let state = unsafe { &mut *data.cast::<State>() };
    let old = state.selection;
    state.selection = proxy;
    state.selection_seen = true;
    state.generation = state.generation.wrapping_add(1);
    if old != proxy {
        state.destroy_offer(old);
    }
}
unsafe extern "C" fn primary_selection(data: Proxy, _: Proxy, proxy: Proxy) {
    // Observing and discarding an offer does not change primary selection.
    unsafe { &mut *data.cast::<State>() }.destroy_offer(proxy);
}
unsafe extern "C" fn finished(data: Proxy, _: Proxy) {
    unsafe { &mut *data.cast::<State>() }.error =
        Some("unsupported_state: compositor ended clipboard data-control access".into());
}
unsafe extern "C" fn source_send(data: Proxy, proxy: Proxy, mime: *const c_char, fd: c_int) {
    if fd < 0 {
        return;
    }
    // The event transfers this fd to us on every path, including rejection.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let state = unsafe { &mut *data.cast::<State>() };
    if mime.is_null() || state.transfers.len() >= 16 {
        return;
    }
    let mime = unsafe { CStr::from_ptr(mime) };
    if !MIMES.contains(&mime) || nonblocking(fd.as_raw_fd()).is_err() {
        return;
    }
    if let Some(bytes) = state.sources.get(&(proxy as usize)) {
        state.transfers.push(Transfer {
            fd,
            bytes: bytes.clone(),
            sent: 0,
            deadline: Instant::now() + OP_TIMEOUT,
        });
    }
}
unsafe extern "C" fn source_cancelled(data: Proxy, proxy: Proxy) {
    let state = unsafe { &mut *data.cast::<State>() };
    if state.sources.remove(&(proxy as usize)).is_some() {
        unsafe {
            state.api.dispose(proxy, 1);
        }
    }
}
unsafe extern "C" fn sync_done(data: Proxy, _: Proxy, _: u32) {
    unsafe {
        (*data.cast::<Cell<bool>>()).set(true);
    }
}
unsafe extern "C" fn seat_capabilities(_: Proxy, _: Proxy, _: u32) {}
struct Listener<const N: usize>([*const c_void; N]);
unsafe impl<const N: usize> Sync for Listener<N> {}
static REGISTRY_LISTENER: Listener<2> =
    Listener([global as *const c_void, global_remove as *const c_void]);
static OFFER_LISTENER: Listener<1> = Listener([offer_mime as *const c_void]);
static DEVICE_LISTENER: Listener<4> = Listener([
    data_offer as *const c_void,
    selection as *const c_void,
    finished as *const c_void,
    primary_selection as *const c_void,
]);
static SOURCE_LISTENER: Listener<2> = Listener([
    source_send as *const c_void,
    source_cancelled as *const c_void,
]);
static SEAT_LISTENER: Listener<1> = Listener([seat_capabilities as *const c_void]);

struct Connection {
    api: Api,
    display: Proxy,
    registry: Proxy,
    seat: Proxy,
    manager: Proxy,
    device: Proxy,
    protocol: &'static Protocol,
    state: Box<State>,
    stop: Arc<AtomicBool>,
    _library: Library,
}
impl Connection {
    fn open(
        path: Option<&std::path::Path>,
        deadline: Instant,
        cancellation: &Cancellation,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        check(deadline, Some(cancellation), &stop)?;
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => {
                if std::env::var_os("WAYLAND_SOCKET").is_some() {
                    return Err("unsupported_state: clipboard requires its own Wayland display socket, not an inherited WAYLAND_SOCKET fd".into());
                }
                let display = std::env::var_os("WAYLAND_DISPLAY").ok_or(
                    "unsupported_state: clipboard requires a Wayland session with WAYLAND_DISPLAY",
                )?;
                let display = std::path::PathBuf::from(display);
                if display.is_absolute() {
                    display
                } else {
                    std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").ok_or(
                        "unsupported_state: clipboard cannot locate the Wayland runtime directory",
                    )?)
                    .join(display)
                }
            }
        };
        let fd = display_socket(&path, deadline, cancellation, &stop)?;
        let library = Library::open(c"libwayland-client.so.0")?;
        let api = Api::load(&library)?;
        // libwayland takes ownership, including its failure cleanup.
        let display = unsafe { (api.connect_fd)(fd.into_raw_fd()) };
        if display.is_null() {
            return Err("cannot create native clipboard Wayland connection".into());
        }
        let mut this = Self {
            api,
            display,
            registry: ptr::null_mut(),
            seat: ptr::null_mut(),
            manager: ptr::null_mut(),
            device: ptr::null_mut(),
            protocol: &wlr_data_control_v1::PROTOCOL,
            state: Box::new(State {
                api,
                seat_id: None,
                ext_id: None,
                wlr_id: None,
                manager_id: None,
                selection: ptr::null_mut(),
                selection_seen: false,
                generation: 0,
                offers: BTreeMap::new(),
                sources: BTreeMap::new(),
                transfers: Vec::new(),
                error: None,
            }),
            stop,
            _library: library,
        };
        this.registry = unsafe {
            (api.marshal)(
                display,
                1,
                api.registry_interface,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if this.registry.is_null() {
            return Err("cannot create clipboard Wayland registry".into());
        }
        let data = (&mut *this.state as *mut State).cast();
        if unsafe { (api.add_listener)(this.registry, REGISTRY_LISTENER.0.as_ptr(), data) } != 0 {
            return Err("cannot listen to clipboard Wayland registry".into());
        }
        this.roundtrip(deadline, Some(cancellation))?;
        let seat_id = this
            .state
            .seat_id
            .ok_or("unsupported_state: Wayland display has no clipboard seat")?;
        let manager_id = if let Some(id) = this.state.ext_id {
            this.protocol = &ext_data_control_v1::PROTOCOL;
            id
        } else {
            this.state.wlr_id.ok_or("unsupported_state: compositor exposes neither ext_data_control_manager_v1 nor zwlr_data_control_manager_v1; native clipboard access is unavailable")?
        };
        this.state.manager_id = Some(manager_id);
        this.seat = unsafe {
            (api.marshal)(
                this.registry,
                0,
                api.seat_interface,
                1,
                0,
                seat_id,
                (*api.seat_interface).name,
                1u32,
                ptr::null_mut::<c_void>(),
            )
        };
        if this.seat.is_null() {
            return Err("cannot bind clipboard Wayland seat".into());
        }
        if unsafe { (api.add_listener)(this.seat, SEAT_LISTENER.0.as_ptr(), data) } != 0 {
            return Err("cannot listen to clipboard Wayland seat".into());
        }
        this.manager = unsafe {
            (api.marshal)(
                this.registry,
                0,
                this.protocol.manager,
                1,
                0,
                manager_id,
                this.protocol.manager.name,
                1u32,
                ptr::null_mut::<c_void>(),
            )
        };
        if this.manager.is_null() {
            return Err("cannot bind clipboard data-control manager".into());
        }
        this.device = unsafe {
            (api.marshal)(
                this.manager,
                1,
                this.protocol.device,
                1,
                0,
                ptr::null_mut::<c_void>(),
                this.seat,
            )
        };
        if this.device.is_null() {
            return Err("cannot create clipboard data-control device".into());
        }
        if unsafe { (api.add_listener)(this.device, DEVICE_LISTENER.0.as_ptr(), data) } != 0 {
            return Err("cannot listen to clipboard data-control device".into());
        }
        this.roundtrip(deadline, Some(cancellation))?;
        if !this.state.selection_seen {
            return Err(
                "clipboard selection was not advertised; no empty selection was assumed".into(),
            );
        }
        Ok(this)
    }

    fn pump(
        &mut self,
        deadline: Instant,
        cancellation: Option<&Cancellation>,
        transfer_fd: Option<c_int>,
    ) -> Result<()> {
        if self.stop.load(Ordering::Acquire) {
            return Err("clipboard owner is stopping".into());
        }
        if let Some(token) = cancellation {
            token.check()?;
        }
        if unsafe { (self.api.dispatch_pending)(self.display) } < 0 {
            return Err("clipboard Wayland connection closed".into());
        }
        if let Some(error) = &self.state.error {
            return Err(error.clone());
        }
        self.state.advance_transfers();
        if Instant::now() >= deadline {
            return Ok(());
        }
        if unsafe { (self.api.prepare_read)(self.display) } != 0 {
            return Ok(());
        }
        let flushed = unsafe { (self.api.flush)(self.display) };
        let pending_write =
            flushed < 0 && io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock;
        if flushed < 0 && !pending_write {
            unsafe {
                (self.api.cancel_read)(self.display);
            }
            return Err("clipboard Wayland write failed".into());
        }
        let mut fds = Vec::with_capacity(18);
        fds.push(PollFd {
            fd: unsafe { (self.api.fd)(self.display) },
            events: POLLIN | if pending_write { POLLOUT } else { 0 },
            revents: 0,
        });
        if let Some(fd) = transfer_fd {
            fds.push(PollFd {
                fd,
                events: POLLIN,
                revents: 0,
            });
        }
        for transfer in &self.state.transfers {
            fds.push(PollFd {
                fd: transfer.fd.as_raw_fd(),
                events: POLLOUT,
                revents: 0,
            });
        }
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(TICK)
            .as_millis()
            .clamp(1, 20) as c_int;
        let ready = unsafe { poll(fds.as_mut_ptr(), fds.len(), timeout) };
        let error = io::Error::last_os_error();
        if ready > 0 && fds[0].revents & POLLIN != 0 {
            // read_events completes prepare_read even if it fails.
            if unsafe { (self.api.read_events)(self.display) } < 0 {
                return Err("clipboard Wayland read failed".into());
            }
        } else {
            unsafe {
                (self.api.cancel_read)(self.display);
            }
            if ready < 0 && error.kind() != io::ErrorKind::Interrupted {
                return Err(format!("clipboard poll failed: {error}"));
            }
            if fds[0].revents & (8 | 16 | 32) != 0 {
                return Err("clipboard Wayland display disconnected".into());
            }
        }
        Ok(())
    }

    fn roundtrip(&mut self, deadline: Instant, cancellation: Option<&Cancellation>) -> Result<()> {
        check(deadline, cancellation, &self.stop)?;
        let done = Cell::new(false);
        let listener = [sync_done as *const c_void];
        let callback = unsafe {
            (self.api.marshal)(
                self.display,
                0,
                self.api.callback_interface,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if callback.is_null() {
            return Err("cannot create clipboard synchronization callback".into());
        }
        let result = (|| {
            if unsafe {
                (self.api.add_listener)(
                    callback,
                    listener.as_ptr(),
                    (&done as *const Cell<bool>).cast_mut().cast(),
                )
            } != 0
            {
                return Err("cannot listen to clipboard synchronization callback".into());
            }
            while !done.get() {
                check(deadline, cancellation, &self.stop)?;
                self.pump(deadline, cancellation, None)?;
            }
            Ok(())
        })();
        // Remove callback data before the stack cell/listener can disappear.
        unsafe {
            (self.api.destroy)(callback);
        }
        result
    }

    fn read_selection(
        &mut self,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Option<(String, String)>> {
        self.roundtrip(deadline, Some(cancellation))?;
        if self.state.selection.is_null() {
            return Ok(None);
        }
        let offer = self
            .state
            .offers
            .get(&(self.state.selection as usize))
            .ok_or("clipboard selection has no readable offer")?;
        let mime = if offer.mimes & 1 != 0 {
            MIMES[0]
        } else if offer.mimes & 8 != 0 {
            c"text/plain;charset=UTF-8"
        } else if offer.mimes & 2 != 0 {
            MIMES[1]
        } else if offer.mimes & 4 != 0 {
            MIMES[2]
        } else {
            return Err(
                "unsupported_state: clipboard selection offers no UTF-8 text MIME type".into(),
            );
        };
        let proxy = offer.proxy;
        let generation = self.state.generation;
        let mut raw = [-1; 2];
        if unsafe { pipe2(raw.as_mut_ptr(), O_CLOEXEC) } < 0 {
            return Err(format!(
                "create clipboard receive pipe: {}",
                io::Error::last_os_error()
            ));
        }
        let reader = unsafe { OwnedFd::from_raw_fd(raw[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(raw[1]) };
        nonblocking(reader.as_raw_fd())?;
        check(deadline, Some(cancellation), &self.stop)?;
        unsafe {
            (self.api.marshal)(
                proxy,
                0,
                ptr::null(),
                1,
                0,
                mime.as_ptr(),
                writer.as_raw_fd(),
            );
        }
        // libwayland retained its own queued fd copy for SCM_RIGHTS transfer.
        drop(writer);
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            check(deadline, Some(cancellation), &self.stop)?;
            if generation != self.state.generation {
                return Err("stale_target: clipboard selection changed during reading".into());
            }
            let size = unsafe { read(reader.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
            if size == 0 {
                self.roundtrip(deadline, Some(cancellation))?;
                if generation != self.state.generation {
                    return Err("stale_target: clipboard selection changed during reading".into());
                }
                let text = String::from_utf8(bytes)
                    .map_err(|_| "clipboard selection is not valid UTF-8 text")?;
                validate_text(&text)?;
                return Ok(Some((mime.to_string_lossy().into_owned(), text)));
            }
            if size > 0 {
                let size = size as usize;
                if bytes.len().saturating_add(size) > MAX_BYTES {
                    return Err("clipboard text exceeds the 1 MiB limit".into());
                }
                bytes.extend_from_slice(&chunk[..size]);
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(format!("read clipboard text: {error}"));
            }
            self.pump(deadline, Some(cancellation), Some(reader.as_raw_fd()))?;
        }
    }

    fn write_selection(
        &mut self,
        bytes: Arc<[u8]>,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<()> {
        check(deadline, Some(cancellation), &self.stop)?;
        self.roundtrip(deadline, Some(cancellation))?;
        if self.state.sources.len() >= 4 {
            return Err("clipboard source cleanup exceeded its bounded budget".into());
        }
        let source = unsafe {
            (self.api.marshal)(
                self.manager,
                0,
                self.protocol.source,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if source.is_null() {
            return Err("cannot create clipboard source".into());
        }
        let data = (&mut *self.state as *mut State).cast();
        if unsafe { (self.api.add_listener)(source, SOURCE_LISTENER.0.as_ptr(), data) } != 0 {
            unsafe {
                self.api.dispose(source, 1);
            }
            return Err("cannot listen to clipboard source".into());
        }
        self.state.sources.insert(source as usize, bytes);
        for mime in MIMES {
            unsafe {
                (self.api.marshal)(source, 0, ptr::null(), 1, 0, mime.as_ptr());
            }
        }
        let mut submitted = false;
        let result = cancellation.dispatch(|| {
            check(deadline, Some(cancellation), &self.stop)?;
            submitted = true;
            unsafe {
                (self.api.marshal)(self.device, 0, ptr::null(), 1, 0, source);
            }
            let result = unsafe { (self.api.flush)(self.display) };
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock {
                Err("clipboard selection write failed".into())
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            if !submitted {
                self.state.sources.remove(&(source as usize));
                unsafe {
                    self.api.dispose(source, 1);
                }
                return Err(error);
            }
            return Err(format!(
                "{error}; clipboard selection may already have changed; no replay was attempted"
            ));
        }
        if let Err(error) = self.roundtrip(deadline, Some(cancellation)) {
            return Err(format!(
                "{error}; clipboard selection may already have changed; its source remains with this Runtime while connected"
            ));
        }
        if !self.state.sources.contains_key(&(source as usize)) {
            return Err(
                "stale_target: clipboard source was replaced before ownership could be confirmed"
                    .into(),
            );
        }
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.state.transfers.clear();
        unsafe {
            for (source, _) in std::mem::take(&mut self.state.sources) {
                self.api.dispose(source as Proxy, 1);
            }
            for (_, offer) in std::mem::take(&mut self.state.offers) {
                self.api.dispose(offer.proxy, 1);
            }
            self.api.dispose(self.device, 1);
            self.api.dispose(self.manager, 2);
            if !self.seat.is_null() {
                (self.api.destroy)(self.seat);
            }
            if !self.registry.is_null() {
                (self.api.destroy)(self.registry);
            }
            (self.api.flush)(self.display);
            (self.api.disconnect)(self.display);
        }
    }
}

enum Action {
    Read,
    Write(Arc<[u8]>),
}
enum Response {
    Read(Option<(String, String)>),
    Written,
}
struct Command {
    action: Action,
    deadline: Instant,
    cancellation: Cancellation,
    reply: mpsc::SyncSender<Result<Response>>,
}

pub(super) struct Owner {
    sender: mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    protocol: String,
}
impl Owner {
    pub(super) fn start(cancellation: &Cancellation) -> Result<Self> {
        Self::start_at(None, cancellation)
    }
    fn start_at(path: Option<std::path::PathBuf>, cancellation: &Cancellation) -> Result<Self> {
        cancellation.check()?;
        let deadline = Instant::now() + OP_TIMEOUT;
        let (sender, commands) = mpsc::sync_channel::<Command>(1);
        let (ready, initialized) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let token = cancellation.clone();
        let worker = thread::Builder::new()
            .name("zuse-clipboard".into())
            .spawn(move || {
                // A closed recipient pipe must never deliver SIGPIPE to the host.
                // Block it only on this private worker; no process-wide handler changes.
                let mut blocked: SigSet = [0; 16];
                if unsafe { sigemptyset(&mut blocked) } != 0
                    || unsafe { sigaddset(&mut blocked, 13) } != 0
                    || unsafe { pthread_sigmask(0, &blocked, ptr::null_mut()) } != 0
                {
                    let _ = ready.send(Err(
                        "cannot configure the native clipboard worker signal mask".into(),
                    ));
                    return;
                }
                let mut connection = match Connection::open(
                    path.as_deref(),
                    deadline,
                    &token,
                    worker_stop.clone(),
                ) {
                    Ok(connection) => connection,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(connection.protocol.name.to_owned())).is_err() {
                    return;
                }
                while !worker_stop.load(Ordering::Acquire) {
                    match commands.try_recv() {
                        Ok(command) => {
                            let result = match command.action {
                                Action::Read => connection
                                    .read_selection(command.deadline, &command.cancellation)
                                    .map(Response::Read),
                                Action::Write(bytes) => connection
                                    .write_selection(bytes, command.deadline, &command.cancellation)
                                    .map(|()| Response::Written),
                            };
                            let _ = command.reply.send(result);
                        }
                        Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                    if connection.pump(Instant::now() + TICK, None, None).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("start native clipboard owner: {error}"))?;
        let mut owner = Self {
            sender,
            stop,
            thread: Some(worker),
            protocol: String::new(),
        };
        loop {
            cancellation.check()?;
            if Instant::now() >= deadline {
                return Err("clipboard connection exceeded its 3 second deadline".into());
            }
            match initialized.recv_timeout(TICK) {
                Ok(result) => {
                    owner.protocol = result?;
                    return Ok(owner);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("native clipboard owner failed to initialize".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    pub(super) fn protocol(&self) -> &str {
        &self.protocol
    }
    fn request(&mut self, action: Action, cancellation: &Cancellation) -> Result<Response> {
        cancellation.check()?;
        let deadline = Instant::now() + OP_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .try_send(Command {
                action,
                deadline,
                cancellation: cancellation.clone(),
                reply,
            })
            .map_err(|_| "native clipboard owner is unavailable or busy; no action was replayed")?;
        // Wait for cleanup acknowledgement even after cancellation; the worker
        // observes cancellation on each <=20ms poll and before selection input.
        loop {
            match response.recv_timeout(TICK) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(
                        "native clipboard owner disconnected; selection delivery may be unknown"
                            .into(),
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() > deadline + Duration::from_millis(100) {
                        return Err("native clipboard operation did not acknowledge its deadline; selection delivery may be unknown".into());
                    }
                }
            }
        }
    }
    pub(super) fn read(&mut self, cancellation: &Cancellation) -> Result<Option<(String, String)>> {
        match self.request(Action::Read, cancellation)? {
            Response::Read(text) => Ok(text),
            Response::Written => Err("native clipboard returned an unexpected write reply".into()),
        }
    }
    pub(super) fn write(&mut self, text: &str, cancellation: &Cancellation) -> Result<()> {
        match self.request(Action::Write(Arc::from(text.as_bytes())), cancellation)? {
            Response::Written => Ok(()),
            Response::Read(_) => Err("native clipboard returned an unexpected read reply".into()),
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        sync::atomic::AtomicUsize,
    };

    #[derive(Clone, Copy)]
    enum Mode {
        Empty,
        Binary,
        MissingProtocol,
    }
    struct Fixture {
        path: PathBuf,
        request_content: mpsc::SyncSender<()>,
        content: mpsc::Receiver<Vec<u8>>,
        thread: Option<JoinHandle<usize>>,
    }
    fn number(bytes: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap())
    }
    fn string(text: &[u8]) -> Vec<u8> {
        let mut bytes = (text.len() as u32 + 1).to_ne_bytes().to_vec();
        bytes.extend_from_slice(text);
        bytes.push(0);
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        bytes
    }
    fn frame(object: u32, opcode: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = object.to_ne_bytes().to_vec();
        bytes.extend_from_slice(
            &(((payload.len() as u32 + 8) << 16) | u32::from(opcode)).to_ne_bytes(),
        );
        bytes.extend_from_slice(payload);
        bytes
    }
    fn send(stream: &mut UnixStream, object: u32, opcode: u16, payload: &[u8]) {
        stream.write_all(&frame(object, opcode, payload)).unwrap();
    }
    fn advertise(stream: &mut UnixStream, registry: u32, id: u32, name: &[u8]) {
        let mut payload = id.to_ne_bytes().to_vec();
        payload.extend_from_slice(&string(name));
        payload.extend_from_slice(&1u32.to_ne_bytes());
        send(stream, registry, 0, &payload);
    }
    #[repr(C)]
    struct Iovec {
        base: *mut c_void,
        size: usize,
    }
    #[repr(C)]
    struct MsgHdr {
        name: *mut c_void,
        name_len: u32,
        iov: *mut Iovec,
        iov_len: usize,
        control: *mut c_void,
        control_len: usize,
        flags: c_int,
    }
    #[repr(C)]
    struct CmsgHdr {
        size: usize,
        level: c_int,
        kind: c_int,
    }
    unsafe extern "C" {
        fn sendmsg(fd: c_int, message: *const MsgHdr, flags: c_int) -> isize;
    }
    fn send_transfer(stream: &UnixStream, source: u32) -> OwnedFd {
        let mut raw = [-1; 2];
        assert_eq!(unsafe { pipe2(raw.as_mut_ptr(), O_CLOEXEC) }, 0);
        let reader = unsafe { OwnedFd::from_raw_fd(raw[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(raw[1]) };
        nonblocking(reader.as_raw_fd()).unwrap();
        let mut payload = frame(source, 0, &string(b"text/plain;charset=utf-8"));
        let mut iov = Iovec {
            base: payload.as_mut_ptr().cast(),
            size: payload.len(),
        };
        let mut control = [0usize;
            (std::mem::size_of::<CmsgHdr>()
                + std::mem::size_of::<c_int>()
                + std::mem::size_of::<usize>()
                - 1)
                / std::mem::size_of::<usize>()];
        unsafe {
            let header = control.as_mut_ptr().cast::<CmsgHdr>();
            (*header).size = std::mem::size_of::<CmsgHdr>() + std::mem::size_of::<c_int>();
            (*header).level = 1; // SOL_SOCKET
            (*header).kind = 1; // SCM_RIGHTS
            control
                .as_mut_ptr()
                .cast::<u8>()
                .add(std::mem::size_of::<CmsgHdr>())
                .cast::<c_int>()
                .write(writer.as_raw_fd());
        }
        let message = MsgHdr {
            name: ptr::null_mut(),
            name_len: 0,
            iov: &mut iov,
            iov_len: 1,
            control: control.as_mut_ptr().cast(),
            control_len: std::mem::size_of_val(&control),
            flags: 0,
        };
        assert_eq!(
            unsafe { sendmsg(stream.as_raw_fd(), &message, 0x4000) },
            payload.len() as isize
        );
        reader
    }
    fn receive_transfer(reader: OwnedFd) -> Vec<u8> {
        let deadline = Instant::now() + OP_TIMEOUT;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            assert!(
                Instant::now() < deadline,
                "clipboard fixture transfer stalled"
            );
            let size = unsafe { read(reader.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
            if size == 0 {
                return bytes;
            }
            if size > 0 {
                bytes.extend_from_slice(&chunk[..size as usize]);
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            let mut pfd = PollFd {
                fd: reader.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            };
            assert!(unsafe { poll(&mut pfd, 1, 20) } >= 0);
        }
    }
    impl Fixture {
        fn new(mode: Mode) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "zuse-clipboard-wire-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let listener = UnixListener::bind(&path).unwrap();
            let (request_content, requested) = mpsc::sync_channel(1);
            let (content_sender, content) = mpsc::sync_channel(1);
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut registry = 0;
                let mut manager = 0;
                let mut device = 0;
                let mut source = None;
                let mut selections = 0;
                loop {
                    let mut header = [0u8; 8];
                    match stream.read_exact(&mut header) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(error) => panic!("clipboard wire read: {error}"),
                    }
                    let object = number(&header, 0);
                    let word = number(&header, 4);
                    let opcode = word as u16;
                    let size = (word >> 16) as usize;
                    assert!((8..=4096).contains(&size));
                    let mut payload = vec![0; size - 8];
                    stream.read_exact(&mut payload).unwrap();
                    if object == 1 && opcode == 1 {
                        registry = number(&payload, 0);
                        advertise(&mut stream, registry, 50, b"wl_seat");
                        if !matches!(mode, Mode::MissingProtocol) {
                            advertise(&mut stream, registry, 51, b"ext_data_control_manager_v1");
                        }
                    } else if object == registry && opcode == 0 {
                        if number(&payload, 0) == 51 {
                            manager = number(&payload, payload.len() - 4);
                        }
                    } else if object == manager && opcode == 1 {
                        device = number(&payload, 0);
                        if matches!(mode, Mode::Binary) {
                            // Server IDs are densely allocated starting at
                            // WL_SERVER_ID_START; skipping the first is invalid.
                            let offer = 0xff00_0000u32;
                            send(&mut stream, device, 0, &offer.to_ne_bytes());
                            send(&mut stream, offer, 0, &string(b"image/png"));
                            send(&mut stream, device, 1, &offer.to_ne_bytes());
                        } else {
                            send(&mut stream, device, 1, &0u32.to_ne_bytes());
                        }
                    } else if object == device && opcode == 0 {
                        selections += 1;
                        source = Some(number(&payload, 0));
                    } else if object == 1 && opcode == 0 {
                        let callback = number(&payload, 0);
                        send(&mut stream, callback, 0, &123u32.to_ne_bytes());
                        send(&mut stream, 1, 1, &callback.to_ne_bytes());
                        if let Some(source) = source.take() {
                            // The test asks only after write() has returned.
                            // The process must serve while Runtime is idle.
                            requested.recv_timeout(Duration::from_secs(3)).unwrap();
                            let reader = send_transfer(&stream, source);
                            content_sender.send(receive_transfer(reader)).unwrap();
                        }
                    }
                }
                selections
            });
            Self {
                path,
                request_content,
                content,
                thread: Some(server),
            }
        }
        fn owner(&self) -> Result<Owner> {
            Owner::start_at(Some(self.path.clone()), &Cancellation::default())
        }
        fn finish(mut self) -> usize {
            self.thread.take().unwrap().join().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn available() -> bool {
        if let Err(error) = Library::open(c"libwayland-client.so.0") {
            eprintln!("clipboard wire test needs libwayland-client.so.0: {error}");
            false
        } else {
            true
        }
    }

    #[test]
    fn utf8_source_is_served_after_write_returns_until_owner_is_dropped() {
        if !available() {
            return;
        }
        let fixture = Fixture::new(Mode::Empty);
        let mut owner = fixture.owner().unwrap();
        assert!(owner.read(&Cancellation::default()).unwrap().is_none());
        let expected = "Zuse 原生剪贴板\nsecond line 🙂";
        owner.write(expected, &Cancellation::default()).unwrap();
        fixture.request_content.send(()).unwrap();
        let received = fixture
            .content
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        assert_eq!(received, expected.as_bytes());
        drop(owner);
        assert_eq!(
            fixture.finish(),
            1,
            "one selection change, with no replay or primary input"
        );
    }

    #[test]
    fn missing_protocol_and_nontext_selection_do_not_become_empty_text() {
        if !available() {
            return;
        }
        let fixture = Fixture::new(Mode::MissingProtocol);
        let error = fixture.owner().err().unwrap();
        assert!(error.contains("data_control_manager_v1"), "{error}");
        assert_eq!(fixture.finish(), 0);
        let fixture = Fixture::new(Mode::Binary);
        let mut owner = fixture.owner().unwrap();
        let error = owner.read(&Cancellation::default()).unwrap_err();
        assert!(error.contains("no UTF-8 text MIME"), "{error}");
        let cancellation = Cancellation::default();
        cancellation.cancel();
        assert!(
            owner
                .write("must never replace selection", &cancellation)
                .unwrap_err()
                .starts_with("cancelled:")
        );
        drop(owner);
        assert_eq!(fixture.finish(), 0);
    }
}
