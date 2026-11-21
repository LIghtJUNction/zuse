//! Native Wayland virtual keyboard with explicit keymap restoration.
use super::{Result, evdev, keymap, xkb};
use crate::platform::dylib::Library;
use std::{
    cell::Cell,
    ffi::{CStr, c_char, c_int, c_long, c_ulong, c_void},
    fs::{File, OpenOptions},
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileExt, FileTypeExt},
    },
    ptr,
    time::{Duration, Instant},
};

type Proxy = *mut c_void;
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
// SAFETY: these protocol descriptors are immutable static C ABI metadata.
unsafe impl Sync for Message {}
unsafe impl Sync for Interface {}
struct Types<const N: usize>([*const Interface; N]);
// SAFETY: pointers refer only to immutable protocol descriptors or NULL.
unsafe impl<const N: usize> Sync for Types<N> {}
static NO_TYPES: Types<4> = Types([ptr::null(); 4]);
static KEYBOARD_METHODS: [Message; 4] = [
    Message {
        name: c"keymap".as_ptr(),
        signature: c"uhu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"key".as_ptr(),
        signature: c"uuu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"modifiers".as_ptr(),
        signature: c"uuuu".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
    Message {
        name: c"destroy".as_ptr(),
        signature: c"".as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    },
];
static KEYBOARD_INTERFACE: Interface = Interface {
    name: c"zwp_virtual_keyboard_v1".as_ptr(),
    version: 1,
    method_count: 4,
    methods: KEYBOARD_METHODS.as_ptr(),
    event_count: 0,
    events: ptr::null(),
};
static CREATE_TYPES: Types<2> = Types([ptr::null(), &KEYBOARD_INTERFACE]);
static MANAGER_METHODS: [Message; 1] = [Message {
    name: c"create_virtual_keyboard".as_ptr(),
    signature: c"on".as_ptr(),
    types: CREATE_TYPES.0.as_ptr(),
}];
static MANAGER_INTERFACE: Interface = Interface {
    name: c"zwp_virtual_keyboard_manager_v1".as_ptr(),
    version: 1,
    method_count: 1,
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
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn memfd_create(name: *const c_char, flags: u32) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

const KEY_NUMLOCK: u16 = 69;
const INPUT_EVENT_SIZE: usize = 2 * std::mem::size_of::<c_long>() + 8;

fn num_lock_events(down: bool) -> Vec<u8> {
    // Linux input_event contains two native longs followed by type, code and
    // value. Encoding the initialized fields avoids Rust struct padding.
    let values: &[i32] = if down { &[1, 0] } else { &[0] };
    let mut bytes = vec![0; INPUT_EVENT_SIZE * values.len() * 2];
    for (index, value) in values.iter().enumerate() {
        let at = index * INPUT_EVENT_SIZE * 2 + 2 * std::mem::size_of::<c_long>();
        bytes[at..at + 2].copy_from_slice(&1u16.to_ne_bytes()); // EV_KEY
        bytes[at + 2..at + 4].copy_from_slice(&KEY_NUMLOCK.to_ne_bytes());
        bytes[at + 4..at + 8].copy_from_slice(&value.to_ne_bytes());
        // The following zero-initialized event is EV_SYN/SYN_REPORT.
    }
    bytes
}

fn submit_num_lock(writer: &mut impl Write) -> Result<()> {
    let events = num_lock_events(true);
    match writer.write(&events) {
        Ok(written) if written == events.len() => Ok(()),
        Ok(written) => {
            // A short batch may have delivered the press. Release only; never
            // retry the batch or send a second toggle on an uncertain result.
            if written > 0 {
                let _ = writer.write(&num_lock_events(false));
            }
            Err(format!(
                "native Num Lock input was incomplete ({written}/{} bytes); input was not replayed",
                events.len()
            ))
        }
        Err(error) => Err(format!(
            "write Num Lock to the current keyboard: {error}; input was not replayed"
        )),
    }
}

pub(super) fn toggle_num_lock(
    sysname: &str,
    before_input: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let suffix = sysname
        .strip_prefix("event")
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("unsupported_state: invalid native keyboard sysname")?;
    let mut device = OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/dev/input/event{suffix}"))
        .map_err(|error| format!("open current keyboard for explicit Num Lock input: {error}"))?;
    if !device
        .metadata()
        .map_err(|error| error.to_string())?
        .file_type()
        .is_char_device()
    {
        return Err("unsupported_state: current keyboard is not an evdev character device".into());
    }
    // Linux EVIOCGBIT(EV_KEY, 96), covering KEY_MAX=0x2ff. Both supported
    // Linux architectures use the asm-generic ioctl request encoding.
    let mut keys = [0u8; 96];
    let key_bits = 0x8000_4521u32 | ((keys.len() as u32) << 16);
    if unsafe { ioctl(device.as_raw_fd(), key_bits as c_ulong, keys.as_mut_ptr()) } < 0 {
        return Err(format!(
            "inspect current keyboard capabilities: {}",
            std::io::Error::last_os_error()
        ));
    }
    if keys[usize::from(KEY_NUMLOCK / 8)] & (1 << (KEY_NUMLOCK % 8)) == 0 {
        return Err(
            "unsupported_state: current native keyboard does not expose KEY_NUMLOCK".into(),
        );
    }
    before_input()?;
    keys.fill(0);
    let held_bits = 0x8000_4518u32 | ((keys.len() as u32) << 16); // EVIOCGKEY
    if unsafe { ioctl(device.as_raw_fd(), held_bits as c_ulong, keys.as_mut_ptr()) } < 0 {
        return Err(format!(
            "inspect current keyboard held keys: {}",
            std::io::Error::last_os_error()
        ));
    }
    if keys.iter().any(|byte| *byte != 0) {
        return Err(
            "unsupported_state: a keyboard key is held; release it before Num Lock input".into(),
        );
    }
    submit_num_lock(&mut device)
}
#[allow(non_snake_case)]
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
    seat_interface: *const Interface,
    keyboard_interface: *const Interface,
    callback_interface: *const Interface,
}
impl Api {
    fn load(lib: &Library) -> Result<Self> {
        // SAFETY: declarations match the public libwayland-client ABI.
        unsafe {
            Ok(Self {
                connect: lib.symbol(c"wl_display_connect")?,
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
                registry_interface: lib.symbol(c"wl_registry_interface")?,
                seat_interface: lib.symbol(c"wl_seat_interface")?,
                keyboard_interface: lib.symbol(c"wl_keyboard_interface")?,
                callback_interface: lib.symbol(c"wl_callback_interface")?,
            })
        }
    }
}
#[derive(Default)]
struct State {
    seat: Option<(u32, u32)>,
    has_keyboard: bool,
    manager: Option<u32>,
    original: Vec<u8>,
    modifiers: Option<[u32; 4]>,
    error: Option<String>,
}
unsafe extern "C" fn global(
    data: Proxy,
    _registry: Proxy,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    if interface.is_null() {
        return;
    }
    // SAFETY: libwayland supplies a live NUL-terminated protocol interface name.
    let state = unsafe { &mut *data.cast::<State>() };
    match unsafe { CStr::from_ptr(interface) }.to_bytes() {
        b"wl_seat" if state.seat.is_none() => state.seat = Some((name, version.min(4))),
        b"wl_seat" => {
            state.error = Some("multiple Wayland seats require explicit seat selection".into())
        }
        b"zwp_virtual_keyboard_manager_v1" => state.manager = Some(name),
        _ => {}
    }
}
unsafe extern "C" fn global_remove(_: Proxy, _: Proxy, _: u32) {}
unsafe extern "C" fn capabilities(data: Proxy, _: Proxy, capabilities: u32) {
    // SAFETY: the listener data is the connection's stable boxed State.
    unsafe {
        (*data.cast::<State>()).has_keyboard = capabilities & 2 != 0;
    }
}
unsafe extern "C" fn seat_name(_: Proxy, _: Proxy, _: *const c_char) {}
unsafe extern "C" fn keymap_event(data: Proxy, _: Proxy, format: u32, fd: c_int, size: u32) {
    if fd < 0 {
        return;
    }
    // SAFETY: each keymap event transfers ownership of its received FD.
    let file = unsafe { File::from_raw_fd(fd) };
    let state = unsafe { &mut *data.cast::<State>() };
    if !state.original.is_empty() {
        return;
    }
    let read = (|| -> Result<Vec<u8>> {
        if format != 1 || !(2..=2_097_152).contains(&size) {
            return Err("Wayland keyboard did not supply a bounded XKB keymap".into());
        }
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() < u64::from(size) {
            return Err("Wayland keymap FD is not a sufficiently sized regular file".into());
        }
        let mut bytes = vec![0; size as usize];
        // Positional I/O preserves the sender's shared open-file offset.
        file.read_exact_at(&mut bytes, 0)
            .map_err(|e| e.to_string())?;
        if bytes.last() != Some(&0) || !bytes.starts_with(b"xkb_keymap") {
            return Err("Wayland keymap is malformed or not NUL terminated".into());
        }
        Ok(bytes)
    })();
    match read {
        Ok(bytes) => state.original = bytes,
        Err(error) => state.error = Some(error),
    }
}
unsafe extern "C" fn enter(_: Proxy, _: Proxy, _: u32, _: Proxy, _: Proxy) {}
unsafe extern "C" fn leave(_: Proxy, _: Proxy, _: u32, _: Proxy) {}
unsafe extern "C" fn key_event(_: Proxy, _: Proxy, _: u32, _: u32, _: u32, _: u32) {}
unsafe extern "C" fn modifiers(
    data: Proxy,
    _: Proxy,
    _: u32,
    depressed: u32,
    latched: u32,
    locked: u32,
    group: u32,
) {
    let state = unsafe { &mut *data.cast::<State>() };
    state.modifiers = Some([depressed, latched, locked, group]);
}
unsafe extern "C" fn repeat_info(_: Proxy, _: Proxy, _: c_int, _: c_int) {}
unsafe extern "C" fn sync_done(data: Proxy, _: Proxy, _: u32) {
    // SAFETY: roundtrip keeps this Cell alive until the callback is destroyed;
    // dispatch and its callback run on the same thread.
    unsafe {
        (*data.cast::<Cell<bool>>()).set(true);
    }
}
struct Listener<const N: usize>([*const c_void; N]);
// SAFETY: listener entries are immutable C callback function addresses.
unsafe impl<const N: usize> Sync for Listener<N> {}
static REGISTRY_LISTENER: Listener<2> =
    Listener([global as *const c_void, global_remove as *const c_void]);
static SEAT_LISTENER: Listener<2> =
    Listener([capabilities as *const c_void, seat_name as *const c_void]);
static KEYBOARD_LISTENER: Listener<6> = Listener([
    keymap_event as *const c_void,
    enter as *const c_void,
    leave as *const c_void,
    key_event as *const c_void,
    modifiers as *const c_void,
    repeat_info as *const c_void,
]);
struct Connection {
    api: Api,
    display: Proxy,
    objects: Vec<Proxy>,
    state: Box<State>,
    virtual_keyboard: Proxy,
    seat: Proxy,
    manager: Proxy,
    original: Vec<u8>,
    original_modifiers: Option<[u32; 4]>,
    deadline: Instant,
    _library: Library,
}
impl Connection {
    fn open() -> Result<Self> {
        let library = Library::open(c"libwayland-client.so.0")?;
        let api = Api::load(&library)?;
        let display = unsafe { (api.connect)(ptr::null()) };
        if display.is_null() {
            return Err("cannot connect to the Wayland display".into());
        }
        let mut this = Self {
            api,
            display,
            objects: Vec::new(),
            state: Box::default(),
            virtual_keyboard: ptr::null_mut(),
            seat: ptr::null_mut(),
            manager: ptr::null_mut(),
            original: Vec::new(),
            original_modifiers: None,
            deadline: Instant::now() + Duration::from_secs(15),
            _library: library,
        };
        // SAFETY: protocol opcodes and vararg order follow wayland.xml.
        let registry = unsafe {
            (this.api.marshal)(
                display,
                1,
                this.api.registry_interface,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        this.register(registry)?;
        let data = (&mut *this.state as *mut State).cast();
        if unsafe { (this.api.add_listener)(registry, REGISTRY_LISTENER.0.as_ptr(), data) } != 0 {
            return Err("cannot listen to the Wayland registry".into());
        }
        this.roundtrip()?;
        if let Some(error) = this.state.error.take() {
            return Err(error);
        }
        let (seat_id, seat_version) = this.state.seat.ok_or("Wayland display has no seat")?;
        let manager_id = this
            .state
            .manager
            .ok_or("Wayland compositor does not expose the virtual keyboard protocol")?;
        let seat = unsafe {
            (this.api.marshal)(
                registry,
                0,
                this.api.seat_interface,
                seat_version,
                0,
                seat_id,
                (*this.api.seat_interface).name,
                seat_version,
                ptr::null_mut::<c_void>(),
            )
        };
        this.register(seat)?;
        if unsafe { (this.api.add_listener)(seat, SEAT_LISTENER.0.as_ptr(), data) } != 0 {
            return Err("cannot listen to the Wayland seat".into());
        }
        this.roundtrip()?;
        if !this.state.has_keyboard {
            return Err("the Wayland seat has no keyboard capability to snapshot".into());
        }
        let keyboard = unsafe {
            (this.api.marshal)(
                seat,
                1,
                this.api.keyboard_interface,
                seat_version,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        this.register(keyboard)?;
        if unsafe { (this.api.add_listener)(keyboard, KEYBOARD_LISTENER.0.as_ptr(), data) } != 0 {
            return Err("cannot snapshot the Wayland keyboard".into());
        }
        this.roundtrip()?;
        if let Some(error) = this.state.error.take() {
            return Err(error);
        }
        if this.state.original.is_empty() {
            return Err("the Wayland seat has no keymap to preserve; configure a keyboard before text injection".into());
        }
        this.original = std::mem::take(&mut this.state.original);
        this.original_modifiers = this.state.modifiers;
        // Do not let later events replace the saved pre-injection keymap.
        this.state.original = vec![0];
        let manager = unsafe {
            (this.api.marshal)(
                registry,
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
        this.register(manager)?;
        this.seat = seat;
        this.manager = manager;
        Ok(this)
    }
    fn create_keyboard(&mut self) -> Result<()> {
        // Creating this object can change the active seat keyboard. The
        // original keymap must already be captured by the read-only open().
        self.virtual_keyboard = unsafe {
            (self.api.marshal)(
                self.manager,
                0,
                &KEYBOARD_INTERFACE,
                1,
                0,
                self.seat,
                ptr::null_mut::<c_void>(),
            )
        };
        if self.virtual_keyboard.is_null() {
            return Err("cannot create a virtual keyboard".into());
        }
        Ok(())
    }
    fn register(&mut self, proxy: Proxy) -> Result<()> {
        if proxy.is_null() {
            Err("Wayland object creation failed".into())
        } else {
            self.objects.push(proxy);
            Ok(())
        }
    }
    fn roundtrip(&mut self) -> Result<()> {
        self.roundtrip_checked(crate::key_hold::CheckDeadline::transport(self.deadline))?
            .ok_or_else(|| "Wayland input exceeded its deadline".into())
    }
    fn roundtrip_checked(
        &mut self,
        deadline: crate::key_hold::CheckDeadline,
    ) -> Result<Option<()>> {
        const TIMEOUT: &str = "Wayland input exceeded its deadline";
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
            return Err("Wayland sync creation failed".into());
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
                return Err("Wayland sync listener failed".into());
            }
            while !done.get() {
                if unsafe { (self.api.dispatch_pending)(self.display) } < 0 {
                    return Err("Wayland connection closed".into());
                }
                if done.get() {
                    break;
                }
                let Some(remaining) = deadline.remaining(TIMEOUT)? else {
                    return Ok(None);
                };
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
                    return Err("Wayland write failed".into());
                }
                let remaining = remaining.as_millis().clamp(1, 15_000) as c_int;
                let mut pfd = PollFd {
                    fd: unsafe { (self.api.fd)(self.display) },
                    events: 1 | if needs_write { 4 } else { 0 },
                    revents: 0,
                };
                let ready = unsafe { poll(&mut pfd, 1, remaining) };
                if ready > 0 && pfd.revents & 1 != 0 {
                    if unsafe { (self.api.read_events)(self.display) } < 0 {
                        return Err("Wayland read failed".into());
                    }
                } else {
                    unsafe {
                        (self.api.cancel_read)(self.display);
                    }
                    if ready < 0 && std::io::Error::last_os_error().raw_os_error() == Some(4) {
                        continue;
                    }
                    if ready < 0 {
                        return Err(format!(
                            "Wayland poll failed: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    if ready == 0 {
                        // poll uses whole milliseconds and may return just
                        // before the actual bound. Retry only the remaining
                        // observation time; a hold boundary ends the check.
                        if deadline.remaining(TIMEOUT)?.is_none() {
                            return Ok(None);
                        }
                        continue;
                    }
                    if pfd.revents & (8 | 16 | 32) != 0 {
                        return Err("Wayland display disconnected".into());
                    }
                }
            }
            Ok(Some(()))
        })();
        // Destroying the callback also removes pending events referring to the
        // stack listener/data before this function returns on an error.
        unsafe {
            (self.api.destroy)(callback);
        }
        result
    }
    fn install(&mut self, bytes: &[u8]) -> Result<()> {
        self.send_keymap(bytes)?;
        self.roundtrip()
    }
    fn send_keymap(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > 2_097_152 || bytes.last() != Some(&0) {
            return Err("invalid temporary keymap size".into());
        }
        let fd = unsafe { memfd_create(c"zuse-wayland-keymap".as_ptr(), 1) };
        if fd < 0 {
            return Err(format!(
                "create keymap memfd: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: this function exclusively owns the newly returned memfd.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes).map_err(|e| e.to_string())?;
        unsafe {
            (self.api.marshal)(
                self.virtual_keyboard,
                0,
                ptr::null(),
                1,
                0,
                1u32,
                file.as_raw_fd(),
                bytes.len() as u32,
            );
        }
        Ok(())
    }
    fn key_event(&mut self, code: u32, time: u32, pressed: bool) -> Result<()> {
        if !(8..=255).contains(&code) {
            return Err("temporary XKB keycode is out of range".into());
        }
        unsafe {
            (self.api.marshal)(
                self.virtual_keyboard,
                1,
                ptr::null(),
                1,
                0,
                time,
                code - 8,
                u32::from(pressed),
            );
        }
        let flushed = unsafe { (self.api.flush)(self.display) };
        if flushed < 0 && std::io::Error::last_os_error().raw_os_error() != Some(11) {
            return Err("Wayland key write failed".into());
        }
        Ok(())
    }
    fn press(&mut self, code: u32, time: u32) -> Result<()> {
        if !(8..=255).contains(&code) {
            return Err("temporary XKB keycode is out of range".into());
        }
        // SAFETY: virtual-keyboard keys use evdev codes, XKB minus eight.
        unsafe {
            (self.api.marshal)(
                self.virtual_keyboard,
                1,
                ptr::null(),
                1,
                0,
                time,
                code - 8,
                1u32,
            );
            (self.api.marshal)(
                self.virtual_keyboard,
                1,
                ptr::null(),
                1,
                0,
                time,
                code - 8,
                0u32,
            );
        }
        Ok(())
    }
    fn restore(&mut self) -> Result<()> {
        if self.virtual_keyboard.is_null() {
            return Ok(());
        }
        // Give cleanup its own finite budget even when input reached deadline.
        self.deadline = Instant::now() + Duration::from_secs(2);
        let original = std::mem::take(&mut self.original);
        let installed = self.install(&original);
        self.original = original;
        if installed.is_ok()
            && let Some([depressed, latched, locked, group]) = self.original_modifiers
        {
            unsafe {
                (self.api.marshal)(
                    self.virtual_keyboard,
                    2,
                    ptr::null(),
                    1,
                    0,
                    depressed,
                    latched,
                    locked,
                    group,
                );
            }
        }
        // Only this owned protocol object is destroyed; other keyboards and
        // compositor configuration are untouched.
        unsafe {
            (self.api.marshal)(self.virtual_keyboard, 3, ptr::null(), 1, 1);
        }
        self.virtual_keyboard = ptr::null_mut();
        let destroyed = self.roundtrip();
        installed.and(destroyed)
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if !self.virtual_keyboard.is_null() {
            let _ = self.restore();
        }
        // SAFETY: objects belong to this display and are destroyed locally in
        // reverse creation order before disconnecting and unloading the ABI.
        unsafe {
            for object in self.objects.drain(..).rev() {
                (self.api.destroy)(object);
            }
            (self.api.disconnect)(self.display);
        }
    }
}

pub(super) fn type_text(text: &str, before_input: impl FnOnce() -> Result<()>) -> Result<()> {
    keymap::validate(text)?;
    let mut connection = Connection::open()?;
    if text.is_empty() {
        return Ok(());
    }
    before_input()?;
    connection.create_keyboard()?;
    let start = Instant::now();
    let result = (|| {
        for batch in keymap::Batches::new(text) {
            let mut bytes = batch.keymap.into_bytes();
            if bytes.last() != Some(&0) {
                bytes.push(0);
            }
            connection.install(&bytes)?;
            // The caller has rejected known nonzero/held keyboard states.
            // Temporary keymaps reset client state; this implementation does
            // not claim preservation of unknown sticky/custom modifiers.
            unsafe {
                (connection.api.marshal)(
                    connection.virtual_keyboard,
                    2,
                    ptr::null(),
                    1,
                    0,
                    0u32,
                    0u32,
                    0u32,
                    0u32,
                );
            }
            connection.roundtrip()?;
            for (index, code) in batch.codes.iter().copied().enumerate() {
                if Instant::now() >= connection.deadline {
                    return Err("Wayland input exceeded its deadline".into());
                }
                connection.press(code, start.elapsed().as_millis() as u32)?;
                if index % 64 == 63 {
                    connection.roundtrip()?;
                }
            }
            connection.roundtrip()?;
        }
        Ok(())
    })();
    let restored = connection.restore();
    match (result, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!(
            "input completed but keyboard keymap restoration failed: {error}"
        )),
        (Err(error), Err(cleanup)) => Err(format!(
            "{error}; keyboard keymap restoration failed: {cleanup}"
        )),
    }
}

/// Foreground chords use the original keymap and keep its verified Num Lock
/// mask throughout input and cleanup. Unlike temporary Unicode maps, this path
/// never invents key meanings or sends a zero locked mask to the recipient.
pub(super) fn key_with_num_lock(
    key: &str,
    modifiers: &[&str],
    before_input: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mut connection = Connection::open()?;
    let chord = xkb::locked_chord(key, modifiers, &connection.original)?;
    let base = [0, 0, chord.locked, 0];
    if connection
        .original_modifiers
        .is_some_and(|observed| observed != base)
    {
        return Err("unsupported_state: observed keyboard modifiers differ from the supported Num Lock state".into());
    }
    // Recheck compositor state after read-only map capture and path resolution.
    // The caller has also verified that removing this owned keyboard restores
    // the same active native keyboard, rather than a different device.
    before_input()?;
    connection.original_modifiers = Some(base);
    connection.create_keyboard()?;
    let result = (|| {
        let original = std::mem::take(&mut connection.original);
        let installed = connection.install(&original);
        connection.original = original;
        installed?;
        // SAFETY: the masks were resolved against this unchanged XKB map;
        // protocol argument order is depressed, latched, locked, group.
        unsafe {
            (connection.api.marshal)(
                connection.virtual_keyboard,
                2,
                ptr::null(),
                1,
                0,
                chord.depressed,
                0u32,
                chord.locked,
                0u32,
            );
        }
        connection.roundtrip()?;
        connection.press(chord.code, 0)?;
        connection.roundtrip()
    })();
    // The base mask is explicitly replayed before destruction, including when
    // submitting the key failed. The user's original keyboard is never edited.
    let restored = connection.restore();
    match (result, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!(
            "key submitted but Num Lock state restoration failed: {error}"
        )),
        (Err(error), Err(cleanup)) => Err(format!(
            "{error}; Num Lock state restoration failed: {cleanup}"
        )),
    }
}

pub(super) fn original_keymap() -> Result<Vec<u8>> {
    Connection::open().map(|mut c| std::mem::take(&mut c.original))
}

/// Hold one permitted game key in the unchanged native map. The owned keyboard is removed
/// after releasing the key, with the original modifier state restored.
pub(super) fn hold_key(
    key: crate::key_hold::NativeKey,
    duration_ms: u64,
    num_lock: bool,
    allow_other_keys: bool,
    cancellation: &crate::observation::Cancellation,
    before_input: impl FnOnce() -> Result<()>,
    mut verify_target: impl FnMut(crate::key_hold::CheckDeadline, u32, bool, &[u32]) -> Result<()>,
) -> Result<String> {
    use crate::key_hold::{self, Event};
    key_hold::validate_duration(duration_ms)?;
    cancellation.check()?;
    let mut connection = Connection::open()?;
    let chord = xkb::chord_in_state(key.native(), &[], &connection.original, num_lock)?;
    let ordinary = xkb::ordinary_keycodes(&connection.original, num_lock)?;
    if ordinary.binary_search(&chord.code).is_err() {
        return Err("unsupported_state: the requested key changes modifiers, locks or layout in the current map".into());
    }
    let base = [0, 0, chord.locked, 0];
    if connection
        .original_modifiers
        .is_some_and(|observed| observed != base)
    {
        return Err(
            "unsupported_state: observed keyboard modifiers changed before the key hold".into(),
        );
    }
    before_input()?;
    connection.original_modifiers = Some(base);
    let mut pending_press = false;
    let mut pressed_by_us = false;
    let started = Instant::now();
    let result = key_hold::run(duration_ms, cancellation, |event| match event {
        Event::Press { .. } => {
            connection.create_keyboard()?;
            let original = std::mem::take(&mut connection.original);
            let installed = connection.send_keymap(&original);
            connection.original = original;
            installed?;
            unsafe {
                (connection.api.marshal)(
                    connection.virtual_keyboard,
                    2,
                    ptr::null(),
                    1,
                    0,
                    0u32,
                    0u32,
                    chord.locked,
                    0u32,
                );
            }
            pending_press = true;
            pressed_by_us = true;
            connection.key_event(chord.code, started.elapsed().as_millis() as u32, true)
        }
        Event::Check { deadline } => {
            let deadline = key_hold::CheckDeadline::for_hold(
                deadline,
                Instant::now() + Duration::from_millis(100),
                pressed_by_us,
            );
            if pending_press {
                if connection.roundtrip_checked(deadline)?.is_none() {
                    return Ok(());
                }
                pending_press = false;
            }
            verify_target(
                deadline,
                chord.code,
                pressed_by_us,
                if allow_other_keys { &ordinary } else { &[] },
            )
        }
        Event::Release { deadline } => {
            if connection.virtual_keyboard.is_null() {
                return Ok(());
            }
            connection.deadline = deadline;
            connection.key_event(chord.code, started.elapsed().as_millis() as u32, false)?;
            connection.roundtrip()
        }
    });
    let restored = connection.restore();
    match (result, restored) {
        (Ok(elapsed), Ok(())) => Ok(key_hold::native_receipt(key, duration_ms, elapsed)),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!(
            "key released but keyboard state restoration failed: {error}"
        )),
        (Err(error), Err(cleanup)) => Err(format!(
            "{error}; keyboard state restoration failed: {cleanup}"
        )),
    }
}

/// Submit through the already selected native device without creating or
/// switching keyboards. The Wayland connection only captures the actual XKB
/// map; all state checks and cancellation still use the shared hold loop.
pub(super) fn hold_evdev(
    key: crate::key_hold::NativeKey,
    duration_ms: u64,
    num_lock: bool,
    target: (&str, &[String]),
    cancellation: &crate::observation::Cancellation,
    before_input: impl FnOnce() -> Result<()>,
    mut verify_target: impl FnMut(crate::key_hold::CheckDeadline, u32, bool, &[u32]) -> Result<()>,
) -> Result<String> {
    use crate::key_hold::{self, Event};
    key_hold::validate_duration(duration_ms)?;
    cancellation.check()?;
    let connection = Connection::open()?;
    let chord = xkb::chord_in_state(key.native(), &[], &connection.original, num_lock)?;
    let ordinary = xkb::ordinary_keycodes(&connection.original, num_lock)?;
    if ordinary.binary_search(&chord.code).is_err() {
        return Err("unsupported_state: the requested key changes modifiers, locks or layout in the current map".into());
    }
    if connection
        .original_modifiers
        .is_some_and(|observed| observed != [0, 0, chord.locked, 0])
    {
        return Err(
            "unsupported_state: observed keyboard modifiers changed before the native key hold"
                .into(),
        );
    }
    let mut device = evdev::Keyboard::open_unique(target.0, target.1, chord.code)?;
    before_input()?;
    let elapsed = key_hold::run(duration_ms, cancellation, |event| match event {
        Event::Press { .. } => device.press(),
        Event::Release { .. } => device.release(),
        Event::Check { deadline } => {
            device.verify()?;
            let pressed = device.pressed_by_us();
            key_hold::check_pressed_keys(
                true,
                pressed,
                chord.code,
                device.held_codes()?,
                |code| ordinary.binary_search(&code).is_ok(),
            )?;
            verify_target(
                key_hold::CheckDeadline::for_hold(
                    deadline,
                    Instant::now() + Duration::from_millis(100),
                    pressed,
                ),
                chord.code,
                pressed,
                &ordinary,
            )
        }
    })?;
    Ok(
        key_hold::native_receipt(key, duration_ms, elapsed).replacen(
            '{',
            "{\"transport\":\"native_evdev\",",
            1,
        ),
    )
}

pub(super) fn probe() -> Result<()> {
    Connection::open().map(|_| ())
}

#[cfg(test)]
mod num_lock_tests {
    use super::{INPUT_EVENT_SIZE, KEY_NUMLOCK, num_lock_events, submit_num_lock};
    use std::io::{self, Write};

    fn events(bytes: &[u8]) -> Vec<(u16, u16, i32)> {
        bytes
            .as_chunks::<INPUT_EVENT_SIZE>()
            .0
            .iter()
            .map(|event| {
                let at = INPUT_EVENT_SIZE - 8;
                (
                    u16::from_ne_bytes(event[at..at + 2].try_into().unwrap()),
                    u16::from_ne_bytes(event[at + 2..at + 4].try_into().unwrap()),
                    i32::from_ne_bytes(event[at + 4..at + 8].try_into().unwrap()),
                )
            })
            .collect()
    }

    #[test]
    fn native_num_lock_is_one_press_release_with_sync_frames() {
        let mut bytes = Vec::new();
        submit_num_lock(&mut bytes).unwrap();
        assert_eq!(
            events(&bytes),
            [
                (1, KEY_NUMLOCK, 1),
                (0, 0, 0),
                (1, KEY_NUMLOCK, 0),
                (0, 0, 0)
            ]
        );
    }

    #[test]
    fn short_num_lock_write_only_releases_and_never_replays_the_press() {
        #[derive(Default)]
        struct ShortWrite(Vec<Vec<u8>>);
        impl Write for ShortWrite {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.push(bytes.to_vec());
                Ok(if self.0.len() == 1 {
                    INPUT_EVENT_SIZE
                } else {
                    bytes.len()
                })
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = ShortWrite::default();
        assert!(
            submit_num_lock(&mut writer)
                .unwrap_err()
                .contains("not replayed")
        );
        assert_eq!(writer.0.len(), 2);
        assert_eq!(writer.0[1], num_lock_events(false));
        assert!(events(&writer.0[1]).iter().all(|(_, _, value)| *value == 0));
    }
}
