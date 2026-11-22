//! Complete precise Hyprland warps with a native pointer frame.
//!
//! The persistent virtual pointer only submits zero relative motion and a
//! frame. Hyprland still owns the coordinate transform; no button or nonzero
//! movement is synthesized here. All I/O runs inside the caller's input gate.
use super::Result;
use crate::platform::dylib::Library;
use std::{
    cell::Cell,
    ffi::{CStr, c_char, c_int, c_long, c_void},
    ptr,
    time::{Duration, Instant},
};

type Proxy = *mut c_void;
const IO_TIMEOUT: Duration = Duration::from_secs(1);

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
// SAFETY: these descriptors are immutable static protocol metadata.
unsafe impl Sync for Message {}
unsafe impl Sync for Interface {}
struct Types<const N: usize>([*const Interface; N]);
// SAFETY: pointers refer only to immutable protocol descriptors or NULL.
unsafe impl<const N: usize> Sync for Types<N> {}
static NO_TYPES: Types<5> = Types([ptr::null(); 5]);
static POINTER_METHODS: [Message; 9] = [
    Message {
        name: c"motion".as_ptr(),
        signature: c"uff".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"motion_absolute".as_ptr(),
        signature: c"uuuuu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"button".as_ptr(),
        signature: c"uuu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"axis".as_ptr(),
        signature: c"uuf".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"frame".as_ptr(),
        signature: c"".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"axis_source".as_ptr(),
        signature: c"u".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"axis_stop".as_ptr(),
        signature: c"uu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"axis_discrete".as_ptr(),
        signature: c"uufi".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"destroy".as_ptr(),
        signature: c"".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
];
static POINTER_INTERFACE: Interface = Interface {
    name: c"zwlr_virtual_pointer_v1".as_ptr(),
    version: 1,
    method_count: 9,
    methods: POINTER_METHODS.as_ptr(),
    event_count: 0,
    events: ptr::null(),
};
static CREATE_TYPES: Types<2> = Types([ptr::null(), &POINTER_INTERFACE]);
static MANAGER_METHODS: [Message; 2] = [
    Message {
        name: c"create_virtual_pointer".as_ptr(),
        signature: c"?on".as_ptr(),
        types: CREATE_TYPES.0.as_ptr(),
    },
    Message {
        name: c"destroy".as_ptr(),
        signature: c"".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
];
static MANAGER_INTERFACE: Interface = Interface {
    name: c"zwlr_virtual_pointer_manager_v1".as_ptr(),
    version: 1,
    method_count: 2,
    methods: MANAGER_METHODS.as_ptr(),
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
struct Timespec {
    seconds: c_long,
    nanoseconds: c_long,
}
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
}
struct Api {
    connect: unsafe extern "C" fn(*const c_char) -> Proxy,
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
    callback_interface: *const Interface,
}
impl Api {
    fn load(library: &Library) -> Result<Self> {
        // SAFETY: declarations match the public libwayland-client ABI.
        unsafe {
            Ok(Self {
                connect: library.symbol(c"wl_display_connect")?,
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
                callback_interface: library.symbol(c"wl_callback_interface")?,
            })
        }
    }
}
#[derive(Default)]
struct State {
    seat: Option<u32>,
    manager: Option<u32>,
    error: Option<&'static str>,
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
    // SAFETY: listener data is the connection's stable boxed State; the
    // interface name is supplied as a NUL-terminated string by libwayland.
    let state = unsafe { &mut *data.cast::<State>() };
    match unsafe { CStr::from_ptr(interface) }.to_bytes() {
        b"wl_seat" if state.seat.is_none() => state.seat = Some(name),
        b"wl_seat" => {
            state.error = Some(
                "unsupported_state: multiple Wayland seats require explicit pointer seat selection",
            )
        }
        b"zwlr_virtual_pointer_manager_v1" => state.manager = Some(name),
        _ => {}
    }
}
unsafe extern "C" fn global_remove(data: Proxy, _: Proxy, name: u32) {
    // SAFETY: listener data remains alive until the display is disconnected.
    let state = unsafe { &mut *data.cast::<State>() };
    if state.seat == Some(name) || state.manager == Some(name) {
        state.error = Some(
            "unsupported_state: the Wayland pointer seat or virtual pointer manager was removed",
        );
    }
}
unsafe extern "C" fn sync_done(data: Proxy, _: Proxy, _: u32) {
    // SAFETY: roundtrip retains this Cell until the callback is destroyed;
    // dispatch and callbacks run on the same thread.
    unsafe {
        (*data.cast::<Cell<bool>>()).set(true);
    }
}
struct Listener<const N: usize>([*const c_void; N]);
// SAFETY: listener entries are immutable C callback function addresses.
unsafe impl<const N: usize> Sync for Listener<N> {}
static REGISTRY_LISTENER: Listener<2> =
    Listener([global as *const c_void, global_remove as *const c_void]);

pub(super) struct Connection {
    api: Api,
    display: Proxy,
    registry: Proxy,
    manager: Proxy,
    pointer: Proxy,
    state: Box<State>,
    _library: Library,
}
impl Connection {
    pub(super) fn open() -> Result<Self> {
        Self::open_display(None)
    }
    fn open_display(name: Option<&CStr>) -> Result<Self> {
        let deadline = Instant::now() + IO_TIMEOUT;
        let library = Library::open(c"libwayland-client.so.0")?;
        let api = Api::load(&library)?;
        let display = unsafe { (api.connect)(name.map_or(ptr::null(), CStr::as_ptr)) };
        if display.is_null() {
            return Err("cannot connect to the Wayland display for pointer frames".into());
        }
        let mut this = Self {
            api,
            display,
            registry: ptr::null_mut(),
            manager: ptr::null_mut(),
            pointer: ptr::null_mut(),
            state: Box::default(),
            _library: library,
        };
        // SAFETY: the opcodes and vararg order follow wayland.xml and
        // wlr-virtual-pointer-unstable-v1.xml, bound at protocol version 1.
        this.registry = unsafe {
            (this.api.marshal)(
                display,
                1,
                this.api.registry_interface,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if this.registry.is_null() {
            return Err("cannot create Wayland pointer registry".into());
        }
        let data = (&mut *this.state as *mut State).cast();
        if unsafe { (this.api.add_listener)(this.registry, REGISTRY_LISTENER.0.as_ptr(), data) }
            != 0
        {
            return Err("cannot listen to the Wayland pointer registry".into());
        }
        this.roundtrip(deadline)?;
        if this.state.seat.is_none() {
            return Err("unsupported_state: Wayland display has no pointer seat".into());
        }
        let manager_id = this.state.manager.ok_or("unsupported_state: compositor does not expose zwlr_virtual_pointer_manager_v1; pointer motion frames are unavailable")?;
        this.manager = unsafe {
            (this.api.marshal)(
                this.registry,
                0,
                &MANAGER_INTERFACE,
                1,
                0,
                manager_id,
                MANAGER_INTERFACE.name,
                1u32,
                ptr::null_mut::<c_void>(),
            )
        };
        if this.manager.is_null() {
            return Err("cannot bind Wayland virtual pointer manager".into());
        }
        // A NULL seat selects the compositor's sole seat. Multiple seats were
        // rejected above, so this cannot silently select another user's seat.
        this.pointer = unsafe {
            (this.api.marshal)(
                this.manager,
                0,
                &POINTER_INTERFACE,
                1,
                0,
                ptr::null_mut::<c_void>(),
                ptr::null_mut::<c_void>(),
            )
        };
        if this.pointer.is_null() {
            return Err("cannot create Wayland virtual pointer".into());
        }
        this.roundtrip(deadline)?;
        Ok(this)
    }
    pub(super) fn finish_motion(&mut self) -> Result<()> {
        let deadline = Instant::now() + IO_TIMEOUT;
        if let Some(error) = self.state.error {
            return Err(error.into());
        }
        let mut time = Timespec {
            seconds: 0,
            nanoseconds: 0,
        };
        // CLOCK_MONOTONIC matches native input timestamps and wraps at u32 ms.
        if unsafe { clock_gettime(1, &mut time) } != 0 {
            return Err(format!(
                "read pointer timestamp: {}",
                std::io::Error::last_os_error()
            ));
        }
        let milliseconds = (time.seconds as u64)
            .wrapping_mul(1000)
            .wrapping_add(time.nanoseconds as u64 / 1_000_000) as u32;
        // A zero relative motion preserves the preceding logical warp. On
        // Hyprland it also routes through the native motion handler, which
        // sends wl_pointer.frame even when no coordinate changed.
        unsafe {
            (self.api.marshal)(self.pointer, 0, ptr::null(), 1, 0, milliseconds, 0i32, 0i32);
            (self.api.marshal)(self.pointer, 4, ptr::null(), 1, 0);
        }
        self.roundtrip(deadline)
    }
    fn roundtrip(&mut self, deadline: Instant) -> Result<()> {
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
            return Err("Wayland pointer sync creation failed".into());
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
                return Err("Wayland pointer sync listener failed".into());
            }
            while !done.get() {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .filter(|duration| !duration.is_zero())
                    .ok_or("Wayland pointer frame exceeded its 1 second deadline")?;
                if unsafe { (self.api.dispatch_pending)(self.display) } < 0 {
                    return Err("Wayland pointer connection closed".into());
                }
                if let Some(error) = self.state.error {
                    return Err(error.into());
                }
                if done.get() {
                    break;
                }
                if unsafe { (self.api.prepare_read)(self.display) } != 0 {
                    continue;
                }
                let flushed = unsafe { (self.api.flush)(self.display) };
                let needs_write =
                    flushed < 0 && std::io::Error::last_os_error().raw_os_error() == Some(11);
                if flushed < 0 && !needs_write {
                    unsafe {
                        (self.api.cancel_read)(self.display);
                    }
                    return Err("Wayland pointer write failed".into());
                }
                let mut pfd = PollFd {
                    fd: unsafe { (self.api.fd)(self.display) },
                    events: 1 | if needs_write { 4 } else { 0 },
                    revents: 0,
                };
                let ready =
                    unsafe { poll(&mut pfd, 1, remaining.as_millis().clamp(1, 1000) as c_int) };
                if ready > 0 && pfd.revents & 1 != 0 {
                    // read_events completes the successful prepare_read even
                    // on failure; every other branch must cancel it.
                    if unsafe { (self.api.read_events)(self.display) } < 0 {
                        return Err("Wayland pointer read failed".into());
                    }
                } else {
                    let error = std::io::Error::last_os_error();
                    unsafe {
                        (self.api.cancel_read)(self.display);
                    }
                    if ready < 0 && error.raw_os_error() == Some(4) {
                        continue;
                    }
                    if ready < 0 {
                        return Err(format!("Wayland pointer poll failed: {error}"));
                    }
                    if pfd.revents & (8 | 16 | 32) != 0 {
                        return Err("Wayland pointer display disconnected".into());
                    }
                }
            }
            Ok(())
        })();
        // Drop queued callback events before stack listener/data go away,
        // including on timeout. No request is replayed on an uncertain result.
        unsafe {
            (self.api.destroy)(callback);
        }
        result
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        // Only zero motion was emitted, so no held input requires recovery.
        // flush is nonblocking; teardown must not extend the input deadline.
        unsafe {
            if !self.pointer.is_null() {
                (self.api.marshal)(self.pointer, 8, ptr::null(), 1, 1);
            }
            if !self.manager.is_null() {
                (self.api.marshal)(self.manager, 1, ptr::null(), 1, 1);
            }
            if !self.registry.is_null() {
                (self.api.destroy)(self.registry);
            }
            (self.api.flush)(self.display);
            (self.api.disconnect)(self.display);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::CString,
        io::{Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
        thread::{self, JoinHandle},
    };

    #[derive(Clone, Copy)]
    enum Mode {
        Available,
        MissingManager,
        StallFrame,
    }
    struct Wire {
        object: u32,
        opcode: u16,
        payload: Vec<u8>,
    }
    struct Fixture {
        path: PathBuf,
        server: Option<JoinHandle<Vec<Wire>>>,
    }
    impl Fixture {
        fn new(mode: Mode) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "zuse-pointer-wire-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = thread::spawn(move || {
                let until = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < until =>
                        {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("pointer fixture connection missing: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut messages = Vec::new();
                let mut syncs = 0;
                loop {
                    let mut header = [0; 8];
                    match stream.read_exact(&mut header) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                        Err(error) => panic!("pointer fixture read: {error}"),
                    }
                    let word = u32::from_ne_bytes(header[4..8].try_into().unwrap());
                    let size = (word >> 16) as usize;
                    assert!((8..=4096).contains(&size));
                    let mut message = Wire {
                        object: u32::from_ne_bytes(header[..4].try_into().unwrap()),
                        opcode: word as u16,
                        payload: vec![0; size - 8],
                    };
                    stream.read_exact(&mut message.payload).unwrap();
                    if message.object == 1 && message.opcode == 1 {
                        let registry = number(&message.payload, 0);
                        advertise(&mut stream, registry, 50, b"wl_seat\0");
                        if !matches!(mode, Mode::MissingManager) {
                            advertise(
                                &mut stream,
                                registry,
                                51,
                                b"zwlr_virtual_pointer_manager_v1\0",
                            );
                        }
                    } else if message.object == 1 && message.opcode == 0 {
                        syncs += 1;
                        if !matches!(mode, Mode::StallFrame) || syncs < 3 {
                            let callback = number(&message.payload, 0);
                            send(&mut stream, callback, 0, &123u32.to_ne_bytes());
                            send(&mut stream, 1, 1, &callback.to_ne_bytes());
                        }
                    }
                    messages.push(message);
                }
                messages
            });
            Self {
                path,
                server: Some(server),
            }
        }
        fn open(&self) -> Result<Connection> {
            Connection::open_display(Some(
                &CString::new(self.path.as_os_str().as_encoded_bytes()).unwrap(),
            ))
        }
        fn finish(mut self) -> Vec<Wire> {
            self.server.take().unwrap().join().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn number(bytes: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap())
    }
    fn send(stream: &mut UnixStream, object: u32, opcode: u16, payload: &[u8]) {
        let mut bytes = object.to_ne_bytes().to_vec();
        bytes.extend_from_slice(
            &(((payload.len() as u32 + 8) << 16) | u32::from(opcode)).to_ne_bytes(),
        );
        bytes.extend_from_slice(payload);
        stream.write_all(&bytes).unwrap();
    }
    fn advertise(stream: &mut UnixStream, registry: u32, name: u32, interface: &[u8]) {
        let mut payload = name.to_ne_bytes().to_vec();
        payload.extend_from_slice(&(interface.len() as u32).to_ne_bytes());
        payload.extend_from_slice(interface);
        while !payload.len().is_multiple_of(4) {
            payload.push(0);
        }
        payload.extend_from_slice(&1u32.to_ne_bytes());
        send(stream, registry, 0, &payload);
    }
    fn has_client_library() -> bool {
        if let Err(error) = Library::open(c"libwayland-client.so.0") {
            eprintln!("native pointer wire fixture requires libwayland-client.so.0: {error}");
            false
        } else {
            true
        }
    }

    #[test]
    fn frames_preserve_coordinates_and_reuse_one_virtual_pointer() {
        if !has_client_library() {
            return;
        }
        let fixture = Fixture::new(Mode::Available);
        let mut connection = fixture.open().unwrap();
        connection.finish_motion().unwrap();
        connection.finish_motion().unwrap();
        drop(connection);
        let wire = fixture.finish();
        let bind = wire
            .iter()
            .find(|message| message.object != 1 && message.payload.len() > 8)
            .unwrap();
        assert_eq!(number(&bind.payload, 0), 51);
        assert_eq!(number(&bind.payload, bind.payload.len() - 8), 1);
        let manager = number(&bind.payload, bind.payload.len() - 4);
        let creates: Vec<_> = wire
            .iter()
            .filter(|message| message.object == manager && message.opcode == 0)
            .collect();
        assert_eq!(creates.len(), 1);
        assert_eq!(creates[0].payload.len(), 8);
        assert_eq!(
            number(&creates[0].payload, 0),
            0,
            "sole seat uses the nullable seat argument"
        );
        let pointer = number(&creates[0].payload, 4);
        let input: Vec<_> = wire
            .iter()
            .filter(|message| message.object == pointer)
            .collect();
        assert_eq!(
            input
                .iter()
                .map(|message| message.opcode)
                .collect::<Vec<_>>(),
            [0, 4, 0, 4, 8]
        );
        for motion in input.iter().filter(|message| message.opcode == 0) {
            assert_eq!(motion.payload.len(), 12);
            assert_eq!(
                &motion.payload[4..],
                &[0; 8],
                "zero relative motion must not alter coordinates"
            );
        }
        assert_eq!(
            wire.iter()
                .filter(|message| message.object == 1 && message.opcode == 0)
                .count(),
            4
        );
    }

    #[test]
    fn missing_virtual_pointer_protocol_fails_before_input() {
        if !has_client_library() {
            return;
        }
        let fixture = Fixture::new(Mode::MissingManager);
        let error = fixture.open().err().unwrap();
        assert!(error.contains("zwlr_virtual_pointer_manager_v1"));
        let wire = fixture.finish();
        assert!(wire.iter().all(|message| message.object == 1));
    }

    #[test]
    fn unacknowledged_frame_times_out_without_replay() {
        if !has_client_library() {
            return;
        }
        let fixture = Fixture::new(Mode::StallFrame);
        let mut connection = fixture.open().unwrap();
        let started = Instant::now();
        assert!(connection.finish_motion().unwrap_err().contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(connection);
        let wire = fixture.finish();
        // One creation, one zero motion and one frame; no retry after timeout.
        assert_eq!(
            wire.iter()
                .filter(|message| message.object != 1 && message.opcode == 4)
                .count(),
            1
        );
        assert_eq!(
            wire.iter()
                .filter(|message| message.object == 1 && message.opcode == 0)
                .count(),
            3
        );
    }

    #[test]
    fn ambiguous_or_removed_seat_is_reported() {
        let mut state = State::default();
        let data = (&mut state as *mut State).cast();
        unsafe {
            global(data, ptr::null_mut(), 50, c"wl_seat".as_ptr(), 1);
            global(data, ptr::null_mut(), 51, c"wl_seat".as_ptr(), 1);
        }
        assert!(state.error.unwrap().contains("multiple"));
        state.error = None;
        unsafe {
            global_remove(data, ptr::null_mut(), 50);
        }
        assert!(state.error.unwrap().contains("removed"));
    }
}
