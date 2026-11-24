//! Click-through Wayland operation markers, independent of input delivery.
//!
//! Protocol layouts follow wayland.xml, xdg-output-unstable-v1.xml and
//! <https://github.com/swaywm/wlr-protocols/blob/master/unstable/wlr-layer-shell-unstable-v1.xml>.
//! Coordinates and image sizes are logical pixels. The compositor handles
//! output transforms and fractional scaling; this module never creates a seat
//! or an input device and never changes the user's system cursor.

use super::{Result, dylib::Library};
use crate::cursor::CursorImage;
use std::{
    cell::Cell,
    ffi::{CStr, CString, c_char, c_int, c_void},
    fs::File,
    io::Write,
    os::fd::FromRawFd,
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
// SAFETY: these descriptors contain immutable protocol metadata only.
unsafe impl Sync for Message {}
unsafe impl Sync for Interface {}
struct Types<const N: usize>([*const Interface; N]);
// SAFETY: pointers refer to immutable descriptors or NULL.
unsafe impl<const N: usize> Sync for Types<N> {}
static NO_TYPES: Types<5> = Types([ptr::null(); 5]);
const fn message(name: &'static CStr, signature: &'static CStr) -> Message {
    Message {
        name: name.as_ptr(),
        signature: signature.as_ptr(),
        types: NO_TYPES.0.as_ptr(),
    }
}
static LAYER_METHODS: [Message; 8] = [
    message(c"set_size", c"uu"),
    message(c"set_anchor", c"u"),
    message(c"set_exclusive_zone", c"i"),
    message(c"set_margin", c"iiii"),
    message(c"set_keyboard_interactivity", c"u"),
    message(c"get_popup", c"o"),
    message(c"ack_configure", c"u"),
    message(c"destroy", c""),
];
static LAYER_EVENTS: [Message; 2] = [message(c"configure", c"uuu"), message(c"closed", c"")];
static LAYER_INTERFACE: Interface = Interface {
    name: c"zwlr_layer_surface_v1".as_ptr(),
    version: 1,
    method_count: 8,
    methods: LAYER_METHODS.as_ptr(),
    event_count: 2,
    events: LAYER_EVENTS.as_ptr(),
};
static LAYER_TYPES: Types<5> = Types([
    &LAYER_INTERFACE,
    ptr::null(),
    ptr::null(),
    ptr::null(),
    ptr::null(),
]);
static SHELL_METHODS: [Message; 1] = [Message {
    name: c"get_layer_surface".as_ptr(),
    signature: c"no?ous".as_ptr(),
    types: LAYER_TYPES.0.as_ptr(),
}];
static SHELL_INTERFACE: Interface = Interface {
    name: c"zwlr_layer_shell_v1".as_ptr(),
    version: 1,
    method_count: 1,
    methods: SHELL_METHODS.as_ptr(),
    event_count: 0,
    events: ptr::null(),
};
static XDG_OUTPUT_METHODS: [Message; 1] = [message(c"destroy", c"")];
static XDG_OUTPUT_EVENTS: [Message; 5] = [
    message(c"logical_position", c"ii"),
    message(c"logical_size", c"ii"),
    message(c"done", c""),
    message(c"name", c"2s"),
    message(c"description", c"2s"),
];
static XDG_OUTPUT_INTERFACE: Interface = Interface {
    name: c"zxdg_output_v1".as_ptr(),
    version: 2,
    method_count: 1,
    methods: XDG_OUTPUT_METHODS.as_ptr(),
    event_count: 5,
    events: XDG_OUTPUT_EVENTS.as_ptr(),
};
static XDG_OUTPUT_TYPES: Types<2> = Types([&XDG_OUTPUT_INTERFACE, ptr::null()]);
static OUTPUT_MANAGER_METHODS: [Message; 2] = [
    message(c"destroy", c""),
    Message {
        name: c"get_xdg_output".as_ptr(),
        signature: c"no".as_ptr(),
        types: XDG_OUTPUT_TYPES.0.as_ptr(),
    },
];
static OUTPUT_MANAGER_INTERFACE: Interface = Interface {
    name: c"zxdg_output_manager_v1".as_ptr(),
    version: 2,
    method_count: 2,
    methods: OUTPUT_MANAGER_METHODS.as_ptr(),
    event_count: 0,
    events: ptr::null(),
};
struct Listener<const N: usize>([*const c_void; N]);
// SAFETY: listeners contain immutable C callback addresses.
unsafe impl<const N: usize> Sync for Listener<N> {}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn memfd_create(name: *const c_char, flags: u32) -> c_int;
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
    registry: *const Interface,
    compositor: *const Interface,
    surface: *const Interface,
    region: *const Interface,
    shm: *const Interface,
    pool: *const Interface,
    buffer: *const Interface,
    output: *const Interface,
    callback: *const Interface,
}
impl Api {
    fn load(lib: &Library) -> Result<Self> {
        // SAFETY: function declarations and exported metadata match the public
        // libwayland-client ABI; the library outlives every use of this table.
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
                registry: lib.symbol(c"wl_registry_interface")?,
                compositor: lib.symbol(c"wl_compositor_interface")?,
                surface: lib.symbol(c"wl_surface_interface")?,
                region: lib.symbol(c"wl_region_interface")?,
                shm: lib.symbol(c"wl_shm_interface")?,
                pool: lib.symbol(c"wl_shm_pool_interface")?,
                buffer: lib.symbol(c"wl_buffer_interface")?,
                output: lib.symbol(c"wl_output_interface")?,
                callback: lib.symbol(c"wl_callback_interface")?,
            })
        }
    }
}

#[derive(Clone, Copy)]
struct Global {
    name: u32,
    version: u32,
}
#[derive(Default)]
struct RegistryState {
    compositor: Option<Global>,
    shm: Option<Global>,
    shell: Option<Global>,
    output_manager: Option<Global>,
    outputs: Vec<Global>,
    argb: bool,
    error: Option<String>,
}
unsafe extern "C" fn global(
    data: Proxy,
    _: Proxy,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    if interface.is_null() {
        return;
    }
    // SAFETY: listener data is the marker's stable boxed RegistryState.
    let state = unsafe { &mut *data.cast::<RegistryState>() };
    let g = Global { name, version };
    match unsafe { CStr::from_ptr(interface) }.to_bytes() {
        b"wl_compositor" => {
            state.compositor.get_or_insert(g);
        }
        b"wl_shm" => {
            state.shm.get_or_insert(g);
        }
        b"zwlr_layer_shell_v1" => {
            state.shell.get_or_insert(g);
        }
        b"zxdg_output_manager_v1" => {
            state.output_manager.get_or_insert(g);
        }
        b"wl_output" if state.outputs.len() < 64 => state.outputs.push(g),
        b"wl_output" => state.error = Some("Wayland marker supports at most 64 outputs".into()),
        _ => {}
    }
}
unsafe extern "C" fn global_remove(data: Proxy, _: Proxy, name: u32) {
    let state = unsafe { &mut *data.cast::<RegistryState>() };
    state.outputs.retain(|global| global.name != name);
    for slot in [
        &mut state.compositor,
        &mut state.shm,
        &mut state.shell,
        &mut state.output_manager,
    ] {
        if slot.is_some_and(|global| global.name == name) {
            *slot = None;
            state.error = Some("Wayland marker protocol global was removed".into());
        }
    }
}
unsafe extern "C" fn shm_format(data: Proxy, _: Proxy, format: u32) {
    if format == 0 {
        unsafe {
            (*data.cast::<RegistryState>()).argb = true;
        }
    }
}
unsafe extern "C" fn output_geometry(
    _: Proxy,
    _: Proxy,
    _: i32,
    _: i32,
    _: i32,
    _: i32,
    _: i32,
    _: *const c_char,
    _: *const c_char,
    _: i32,
) {
}
unsafe extern "C" fn output_mode(_: Proxy, _: Proxy, _: u32, _: i32, _: i32, _: i32) {}
unsafe extern "C" fn output_done(_: Proxy, _: Proxy) {}
unsafe extern "C" fn output_scale(_: Proxy, _: Proxy, _: i32) {}
unsafe extern "C" fn ignore_string(_: Proxy, _: Proxy, _: *const c_char) {}
unsafe extern "C" fn surface_output(_: Proxy, _: Proxy, _: Proxy) {}
unsafe extern "C" fn buffer_release(_: Proxy, _: Proxy) {}
unsafe extern "C" fn sync_done(data: Proxy, _: Proxy, _: u32) {
    unsafe {
        (*data.cast::<Cell<bool>>()).set(true);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Geometry {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}
impl Geometry {
    fn contains(self, x: i32, y: i32) -> bool {
        self.width > 0
            && self.height > 0
            && i64::from(x) >= i64::from(self.x)
            && i64::from(y) >= i64::from(self.y)
            && i64::from(x) < i64::from(self.x) + i64::from(self.width)
            && i64::from(y) < i64::from(self.y) + i64::from(self.height)
    }
    fn margins(self, point: (i32, i32), hotspot: (u32, u32)) -> Result<(i32, i32)> {
        let left = i64::from(point.0) - i64::from(self.x) - i64::from(hotspot.0);
        let top = i64::from(point.1) - i64::from(self.y) - i64::from(hotspot.1);
        Ok((
            i32::try_from(top).map_err(|_| "marker vertical coordinate overflow")?,
            i32::try_from(left).map_err(|_| "marker horizontal coordinate overflow")?,
        ))
    }
}
#[derive(Default)]
struct OutputState {
    pending: Geometry,
    has_position: bool,
    has_size: bool,
    logical: Option<Geometry>,
}
unsafe extern "C" fn logical_position(data: Proxy, _: Proxy, x: i32, y: i32) {
    let state = unsafe { &mut *data.cast::<OutputState>() };
    state.pending.x = x;
    state.pending.y = y;
    state.has_position = true;
}
unsafe extern "C" fn logical_size(data: Proxy, _: Proxy, width: i32, height: i32) {
    let state = unsafe { &mut *data.cast::<OutputState>() };
    state.pending.width = width;
    state.pending.height = height;
    state.has_size = true;
}
unsafe extern "C" fn logical_done(data: Proxy, _: Proxy) {
    let state = unsafe { &mut *data.cast::<OutputState>() };
    state.logical = (state.has_position
        && state.has_size
        && state.pending.width > 0
        && state.pending.height > 0)
        .then_some(state.pending);
}
#[derive(Default)]
struct LayerState {
    configure: Option<u32>,
    closed: bool,
}
impl LayerState {
    fn require_open(&self) -> Result<()> {
        if self.closed {
            Err("the compositor closed the Wayland marker surface".into())
        } else {
            Ok(())
        }
    }
}
unsafe extern "C" fn layer_configure(data: Proxy, _: Proxy, serial: u32, _: u32, _: u32) {
    unsafe {
        (*data.cast::<LayerState>()).configure = Some(serial);
    }
}
unsafe extern "C" fn layer_closed(data: Proxy, _: Proxy) {
    unsafe {
        (*data.cast::<LayerState>()).closed = true;
    }
}
static REGISTRY_LISTENER: Listener<2> =
    Listener([global as *const c_void, global_remove as *const c_void]);
static SHM_LISTENER: Listener<1> = Listener([shm_format as *const c_void]);
static OUTPUT_LISTENER: Listener<4> = Listener([
    output_geometry as *const c_void,
    output_mode as *const c_void,
    output_done as *const c_void,
    output_scale as *const c_void,
]);
static XDG_LISTENER: Listener<5> = Listener([
    logical_position as *const c_void,
    logical_size as *const c_void,
    logical_done as *const c_void,
    ignore_string as *const c_void,
    ignore_string as *const c_void,
]);
static LAYER_LISTENER: Listener<2> = Listener([
    layer_configure as *const c_void,
    layer_closed as *const c_void,
]);
static SURFACE_LISTENER: Listener<2> = Listener([
    surface_output as *const c_void,
    surface_output as *const c_void,
]);
static BUFFER_LISTENER: Listener<1> = Listener([buffer_release as *const c_void]);

struct Output {
    name: u32,
    version: u32,
    proxy: Proxy,
    xdg: Proxy,
    state: Box<OutputState>,
}
struct Layer {
    output: u32,
    surface: Proxy,
    role: Proxy,
    state: Box<LayerState>,
}
struct Buffer {
    proxy: Proxy,
    width: u32,
    height: u32,
    hotspot: (u32, u32),
    // The immutable backing store is owned until its wl_buffer is destroyed.
    _file: File,
}

/// An inert image anchored to the last operation point, not a second pointer.
pub(crate) struct WaylandMarker {
    api: Api,
    display: Proxy,
    registry: Proxy,
    compositor: Proxy,
    shm: Proxy,
    shell: Proxy,
    output_manager: Proxy,
    surface_version: u32,
    output_version: u32,
    state: Box<RegistryState>,
    outputs: Vec<Output>,
    layer: Option<Layer>,
    buffer: Option<Buffer>,
    point: Option<(i32, i32)>,
    namespace: CString,
    deadline: Instant,
    _library: Library,
}
impl WaylandMarker {
    pub(crate) fn connect(name: &str, image: &CursorImage) -> Result<Self> {
        image.validate()?;
        let namespace =
            CString::new(format!("zuse-marker-{name}")).map_err(|_| "marker name contains NUL")?;
        let library = Library::open(c"libwayland-client.so.0")?;
        let api = Api::load(&library)?;
        let display = unsafe { (api.connect)(ptr::null()) };
        if display.is_null() {
            return Err("cannot connect the operation marker to Wayland".into());
        }
        let mut this = Self {
            api,
            display,
            registry: ptr::null_mut(),
            compositor: ptr::null_mut(),
            shm: ptr::null_mut(),
            shell: ptr::null_mut(),
            output_manager: ptr::null_mut(),
            surface_version: 1,
            output_version: 1,
            state: Box::default(),
            outputs: Vec::new(),
            layer: None,
            buffer: None,
            point: None,
            namespace,
            deadline: Instant::now() + Duration::from_secs(2),
            _library: library,
        };
        this.registry = unsafe {
            (this.api.marshal)(
                display,
                1,
                this.api.registry,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        let state_data = (&mut *this.state as *mut RegistryState).cast();
        this.listen(this.registry, &REGISTRY_LISTENER, state_data)?;
        this.roundtrip()?;
        this.check_registry()?;
        let compositor = this
            .state
            .compositor
            .ok_or("Wayland marker requires wl_compositor")?;
        let shm = this.state.shm.ok_or("Wayland marker requires wl_shm")?;
        let shell = this
            .state
            .shell
            .ok_or("unsupported: compositor does not support wlr-layer-shell operation markers")?;
        let outputs = this
            .state
            .output_manager
            .ok_or("unsupported: marker requires xdg-output logical display coordinates")?;
        this.surface_version = compositor.version.min(4);
        this.output_version = outputs.version.min(2);
        this.compositor = this.bind(compositor, this.api.compositor, this.surface_version)?;
        this.shm = this.bind(shm, this.api.shm, 1)?;
        this.shell = this.bind(shell, &SHELL_INTERFACE, 1)?;
        this.output_manager = this.bind(outputs, &OUTPUT_MANAGER_INTERFACE, this.output_version)?;
        let state_data = (&mut *this.state as *mut RegistryState).cast();
        this.listen(this.shm, &SHM_LISTENER, state_data)?;
        this.bind_outputs()?;
        this.roundtrip()?;
        this.check_registry()?;
        if !this.state.argb {
            return Err("Wayland marker requires premultiplied ARGB8888 shared memory".into());
        }
        if !this
            .outputs
            .iter()
            .any(|output| output.state.logical.is_some())
        {
            return Err("Wayland marker has no output with known logical coordinates".into());
        }
        this.buffer = Some(this.create_buffer(image)?);
        this.roundtrip()?;
        Ok(this)
    }

    fn check_registry(&self) -> Result<()> {
        match &self.state.error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
    fn listen<const N: usize>(
        &self,
        proxy: Proxy,
        listener: &Listener<N>,
        data: Proxy,
    ) -> Result<()> {
        if proxy.is_null() {
            return Err("Wayland marker object creation failed".into());
        }
        if unsafe { (self.api.add_listener)(proxy, listener.0.as_ptr(), data) } != 0 {
            return Err("Wayland marker listener registration failed".into());
        }
        Ok(())
    }
    fn bind(&self, global: Global, interface: *const Interface, version: u32) -> Result<Proxy> {
        if version == 0 {
            return Err("Wayland marker protocol has an invalid version".into());
        }
        // SAFETY: registry.bind uses name, interface name, version, new_id.
        let proxy = unsafe {
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
        if proxy.is_null() {
            Err("Wayland marker global binding failed".into())
        } else {
            Ok(proxy)
        }
    }
    fn bind_outputs(&mut self) -> Result<bool> {
        let pending = self
            .state
            .outputs
            .iter()
            .copied()
            .filter(|global| !self.outputs.iter().any(|output| output.name == global.name))
            .collect::<Vec<_>>();
        let added = !pending.is_empty();
        for global in pending {
            let version = global.version.min(3);
            let proxy = self.bind(global, self.api.output, version)?;
            self.outputs.push(Output {
                name: global.name,
                version,
                proxy,
                xdg: ptr::null_mut(),
                state: Box::default(),
            });
            self.listen(proxy, &OUTPUT_LISTENER, ptr::null_mut())?;
            let xdg = unsafe {
                (self.api.marshal)(
                    self.output_manager,
                    1,
                    &XDG_OUTPUT_INTERFACE,
                    self.output_version,
                    0,
                    ptr::null_mut::<c_void>(),
                    proxy,
                )
            };
            let output = self.outputs.last_mut().ok_or("marker output disappeared")?;
            output.xdg = xdg;
            let data = (&mut *output.state as *mut OutputState).cast();
            self.listen(xdg, &XDG_LISTENER, data)?;
        }
        Ok(added)
    }
    fn refresh(&mut self) -> Result<()> {
        self.roundtrip()?;
        self.check_registry()?;
        if self.layer.as_ref().is_some_and(|layer| {
            !self
                .state
                .outputs
                .iter()
                .any(|global| global.name == layer.output)
        }) {
            self.destroy_layer();
        }
        for index in (0..self.outputs.len()).rev() {
            if !self
                .state
                .outputs
                .iter()
                .any(|global| global.name == self.outputs[index].name)
            {
                let output = self.outputs.swap_remove(index);
                self.destroy_output(output);
            }
        }
        if self.bind_outputs()? {
            self.roundtrip()?;
        }
        self.check_registry()
    }

    fn destroy_output(&self, output: Output) {
        unsafe {
            if !output.xdg.is_null() {
                (self.api.marshal)(output.xdg, 0, ptr::null(), self.output_version, 1);
            }
            if output.version >= 3 {
                (self.api.marshal)(output.proxy, 0, ptr::null(), output.version, 1);
            } else {
                (self.api.destroy)(output.proxy);
            }
        }
    }

    fn roundtrip(&mut self) -> Result<()> {
        if self.display.is_null() {
            return Err("Wayland marker is closed".into());
        }
        let done = Cell::new(false);
        let listener = [sync_done as *const c_void];
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
            return Err("Wayland marker sync creation failed".into());
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
                return Err("Wayland marker sync listener failed".into());
            }
            while !done.get() {
                if unsafe { (self.api.dispatch_pending)(self.display) } < 0 {
                    return Err("Wayland marker connection closed".into());
                }
                if done.get() {
                    break;
                }
                if Instant::now() >= self.deadline {
                    return Err("Wayland marker exceeded its deadline".into());
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
                    return Err("Wayland marker write failed".into());
                }
                let remaining = self
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(1, 2000) as c_int;
                let mut pfd = PollFd {
                    fd: unsafe { (self.api.fd)(self.display) },
                    events: 1 | if needs_write { 4 } else { 0 },
                    revents: 0,
                };
                let ready = unsafe { poll(&mut pfd, 1, remaining) };
                if ready > 0 && pfd.revents & 1 != 0 {
                    if unsafe { (self.api.read_events)(self.display) } < 0 {
                        return Err("Wayland marker read failed".into());
                    }
                } else {
                    unsafe {
                        (self.api.cancel_read)(self.display);
                    }
                    if ready < 0 && std::io::Error::last_os_error().raw_os_error() == Some(4) {
                        continue;
                    }
                    if ready <= 0 {
                        return Err("Wayland marker exceeded its deadline".into());
                    }
                    if pfd.revents & (8 | 16 | 32) != 0 {
                        return Err("Wayland marker display disconnected".into());
                    }
                }
            }
            Ok(())
        })();
        // Destroy the proxy before the listener's stack data leaves scope.
        unsafe {
            (self.api.destroy)(callback);
        }
        result
    }

    fn create_buffer(&self, image: &CursorImage) -> Result<Buffer> {
        image.validate()?;
        let fd = unsafe { memfd_create(c"zuse-wayland-marker".as_ptr(), 1) };
        if fd < 0 {
            return Err(format!(
                "create marker shared memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: this function exclusively owns the returned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
        for pixel in &image.pixels {
            bytes.extend_from_slice(&pixel.to_ne_bytes());
        }
        file.write_all(&bytes)
            .map_err(|error| format!("write marker image: {error}"))?;
        let pool = unsafe {
            (self.api.marshal)(
                self.shm,
                0,
                self.api.pool,
                1,
                0,
                ptr::null_mut::<c_void>(),
                fd,
                bytes.len() as i32,
            )
        };
        if pool.is_null() {
            return Err("Wayland marker shared-memory pool creation failed".into());
        }
        let proxy = unsafe {
            (self.api.marshal)(
                pool,
                0,
                self.api.buffer,
                1,
                0,
                ptr::null_mut::<c_void>(),
                0i32,
                image.width as i32,
                image.height as i32,
                (image.width * 4) as i32,
                0u32,
            )
        };
        // wl_buffer retains the server-side pool mapping after pool.destroy.
        unsafe {
            (self.api.marshal)(pool, 1, ptr::null(), 1, 1);
        }
        if let Err(error) = self.listen(proxy, &BUFFER_LISTENER, ptr::null_mut()) {
            if !proxy.is_null() {
                unsafe {
                    (self.api.marshal)(proxy, 0, ptr::null(), 1, 1);
                }
            }
            return Err(error);
        }
        Ok(Buffer {
            proxy,
            width: image.width,
            height: image.height,
            hotspot: (image.hotspot_x, image.hotspot_y),
            _file: file,
        })
    }

    fn destroy_layer(&mut self) {
        if let Some(layer) = self.layer.take() {
            unsafe {
                if !layer.role.is_null() {
                    (self.api.marshal)(layer.role, 7, ptr::null(), 1, 1);
                }
                if !layer.surface.is_null() {
                    (self.api.marshal)(layer.surface, 0, ptr::null(), self.surface_version, 1);
                }
            }
        }
    }
    fn create_layer(&mut self, name: u32, output: Proxy, margin: (i32, i32)) -> Result<()> {
        self.destroy_layer();
        let surface = unsafe {
            (self.api.marshal)(
                self.compositor,
                0,
                self.api.surface,
                self.surface_version,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        self.layer = Some(Layer {
            output: name,
            surface,
            role: ptr::null_mut(),
            state: Box::default(),
        });
        self.listen(surface, &SURFACE_LISTENER, ptr::null_mut())?;
        let role = unsafe {
            (self.api.marshal)(
                self.shell,
                0,
                &LAYER_INTERFACE,
                1,
                0,
                ptr::null_mut::<c_void>(),
                surface,
                output,
                3u32,
                self.namespace.as_ptr(),
            )
        };
        let layer = self.layer.as_mut().ok_or("marker layer is missing")?;
        layer.role = role;
        let data = (&mut *layer.state as *mut LayerState).cast();
        self.listen(role, &LAYER_LISTENER, data)?;
        let region = unsafe {
            (self.api.marshal)(
                self.compositor,
                1,
                self.api.region,
                1,
                0,
                ptr::null_mut::<c_void>(),
            )
        };
        if region.is_null() {
            return Err("Wayland marker input-region creation failed".into());
        }
        let buffer = self.buffer.as_ref().ok_or("marker image is missing")?;
        // The region is deliberately empty (no region.add requests). Passing
        // NULL here would restore an infinite input region and intercept clicks.
        // All policy is set before the first bufferless commit.
        unsafe {
            (self.api.marshal)(surface, 5, ptr::null(), self.surface_version, 0, region);
            (self.api.marshal)(region, 0, ptr::null(), 1, 1);
            (self.api.marshal)(role, 0, ptr::null(), 1, 0, buffer.width, buffer.height);
            (self.api.marshal)(role, 1, ptr::null(), 1, 0, 5u32); // top | left
            (self.api.marshal)(role, 2, ptr::null(), 1, 0, -1i32);
            (self.api.marshal)(role, 3, ptr::null(), 1, 0, margin.0, 0i32, 0i32, margin.1);
            (self.api.marshal)(role, 4, ptr::null(), 1, 0, 0u32); // no keyboard interaction
            if self.surface_version >= 3 {
                (self.api.marshal)(surface, 8, ptr::null(), self.surface_version, 0, 1i32);
            }
            (self.api.marshal)(surface, 6, ptr::null(), self.surface_version, 0);
        }
        self.roundtrip()?;
        let layer = self.layer.as_ref().ok_or("marker layer is missing")?;
        layer.state.require_open()?;
        if layer.state.configure.is_none() {
            return Err("Wayland compositor did not configure the marker surface".into());
        }
        Ok(())
    }
    fn draw(&mut self, point: (i32, i32), attach: bool) -> Result<()> {
        self.refresh()?;
        let output = self
            .outputs
            .iter()
            .find(|output| {
                self.state
                    .outputs
                    .iter()
                    .any(|live| live.name == output.name)
                    && output
                        .state
                        .logical
                        .is_some_and(|geometry| geometry.contains(point.0, point.1))
            })
            .ok_or("operation point is outside every known Wayland logical output")?;
        let (name, proxy, geometry) = (
            output.name,
            output.proxy,
            output.state.logical.ok_or("output geometry is unknown")?,
        );
        let buffer = self.buffer.as_ref().ok_or("marker image is missing")?;
        let margin = geometry.margins(point, buffer.hotspot)?;
        let recreate = self.layer.as_ref().is_none_or(|layer| layer.output != name);
        if recreate {
            self.create_layer(name, proxy, margin)?;
        }
        self.commit_layer(margin, recreate || attach)?;
        self.roundtrip()?;
        self.layer
            .as_ref()
            .ok_or("marker layer is missing")?
            .state
            .require_open()?;
        self.point = Some(point);
        Ok(())
    }

    fn commit_layer(&mut self, margin: (i32, i32), attach: bool) -> Result<()> {
        let layer = self.layer.as_mut().ok_or("marker layer is missing")?;
        layer.state.require_open()?;
        let buffer = self.buffer.as_ref().ok_or("marker image is missing")?;
        unsafe {
            if let Some(serial) = layer.state.configure.take() {
                (self.api.marshal)(layer.role, 6, ptr::null(), 1, 0, serial);
            }
            (self.api.marshal)(
                layer.role,
                3,
                ptr::null(),
                1,
                0,
                margin.0,
                0i32,
                0i32,
                margin.1,
            );
            if attach {
                (self.api.marshal)(
                    layer.role,
                    0,
                    ptr::null(),
                    1,
                    0,
                    buffer.width,
                    buffer.height,
                );
                (self.api.marshal)(
                    layer.surface,
                    1,
                    ptr::null(),
                    self.surface_version,
                    0,
                    buffer.proxy,
                    0i32,
                    0i32,
                );
                (self.api.marshal)(
                    layer.surface,
                    2,
                    ptr::null(),
                    self.surface_version,
                    0,
                    0i32,
                    0i32,
                    buffer.width as i32,
                    buffer.height as i32,
                );
            }
            (self.api.marshal)(layer.surface, 6, ptr::null(), self.surface_version, 0);
        }
        Ok(())
    }

    fn receive_pending(&self) -> Result<()> {
        const READABLE: i16 = 1;
        const CONNECTION_ERROR: i16 = 8 | 16 | 32;
        // Drain queued events first. This connection has a single reader.
        while unsafe { (self.api.prepare_read)(self.display) } != 0 {
            if unsafe { (self.api.dispatch_pending)(self.display) } < 0 {
                return Err("Wayland marker connection closed".into());
            }
        }
        let mut fd = PollFd {
            fd: unsafe { (self.api.fd)(self.display) },
            events: READABLE,
            revents: 0,
        };
        let ready = unsafe { poll(&mut fd, 1, 0) };
        if ready > 0 && fd.revents & READABLE != 0 {
            if unsafe { (self.api.read_events)(self.display) } < 0 {
                return Err("Wayland marker read failed".into());
            }
        } else {
            unsafe { (self.api.cancel_read)(self.display) };
            if fd.revents & CONNECTION_ERROR != 0 {
                return Err("Wayland marker display disconnected".into());
            }
            if ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                return Err("Wayland marker polling failed".into());
            }
        }
        if unsafe { (self.api.dispatch_pending)(self.display) } < 0 {
            return Err("Wayland marker connection closed".into());
        }
        self.check_registry()
    }

    /// Update an established overlay without a compositor round trip per frame.
    /// Output changes use the synchronized layer creation path.
    pub(crate) fn animate_to(&mut self, x: i32, y: i32) -> Result<()> {
        if self.display.is_null() {
            return Err("Wayland marker is closed".into());
        }
        self.receive_pending()?;
        let Some(layer) = self.layer.as_ref() else {
            return self.move_to(x, y);
        };
        layer.state.require_open()?;
        let geometry = self
            .outputs
            .iter()
            .find(|output| {
                output.name == layer.output
                    && self
                        .state
                        .outputs
                        .iter()
                        .any(|live| live.name == output.name)
            })
            .and_then(|output| output.state.logical)
            .filter(|geometry| geometry.contains(x, y));
        let Some(geometry) = geometry else {
            return self.move_to(x, y);
        };
        if self.point == Some((x, y)) && layer.state.configure.is_none() {
            return Ok(());
        }
        let buffer = self.buffer.as_ref().ok_or("marker image is missing")?;
        self.commit_layer(geometry.margins((x, y), buffer.hotspot)?, false)?;
        if unsafe { (self.api.flush)(self.display) } < 0 {
            return Err(format!(
                "Wayland marker animation write: {}",
                std::io::Error::last_os_error()
            ));
        }
        self.point = Some((x, y));
        Ok(())
    }

    pub(crate) fn position(&self) -> Result<(i32, i32)> {
        self.point
            .ok_or_else(|| "Wayland marker has no position".into())
    }

    pub(crate) fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        self.deadline = Instant::now() + Duration::from_secs(2);
        self.draw((x, y), false)
    }
    pub(crate) fn set_cursor(&mut self, image: &CursorImage) -> Result<()> {
        if self.display.is_null() {
            return Err("Wayland marker is closed".into());
        }
        self.deadline = Instant::now() + Duration::from_secs(2);
        let new = self.create_buffer(image)?;
        let old = self.buffer.replace(new);
        let result = if let Some(point) = self.point {
            self.draw(point, true)
        } else {
            self.roundtrip()
        };
        if let Some(old) = old {
            unsafe {
                (self.api.marshal)(old.proxy, 0, ptr::null(), 1, 1);
            }
        }
        result
    }
    pub(crate) fn close(&mut self) -> Result<()> {
        if self.display.is_null() {
            return Ok(());
        }
        self.deadline = Instant::now() + Duration::from_millis(500);
        self.destroy_layer();
        if let Some(buffer) = self.buffer.take() {
            unsafe {
                (self.api.marshal)(buffer.proxy, 0, ptr::null(), 1, 1);
            }
        }
        for output in &mut self.outputs {
            if !output.xdg.is_null() {
                unsafe {
                    (self.api.marshal)(output.xdg, 0, ptr::null(), self.output_version, 1);
                }
                output.xdg = ptr::null_mut();
            }
        }
        if !self.output_manager.is_null() {
            unsafe {
                (self.api.marshal)(self.output_manager, 0, ptr::null(), self.output_version, 1);
            }
            self.output_manager = ptr::null_mut();
        }
        let result = self.roundtrip();
        // Bound globals without destroy requests are reclaimed by
        // disconnect. Local proxies go first while their listeners still live.
        unsafe {
            for output in self.outputs.drain(..) {
                if output.version >= 3 {
                    (self.api.marshal)(output.proxy, 0, ptr::null(), output.version, 1);
                } else {
                    (self.api.destroy)(output.proxy);
                }
            }
            for proxy in [self.shell, self.shm, self.compositor, self.registry] {
                if !proxy.is_null() {
                    (self.api.destroy)(proxy);
                }
            }
            (self.api.disconnect)(self.display);
        }
        self.display = ptr::null_mut();
        self.point = None;
        result
    }
}
impl Drop for WaylandMarker {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Geometry, LayerState, OutputState, layer_closed, logical_done, logical_position,
        logical_size,
    };
    use std::ptr;
    #[test]
    fn logical_output_coordinates_preserve_negative_origins_and_fractional_scaling() {
        // A 1920x1080 output at 125% has 1536x864 logical coordinates. No
        // second scaling is applied to its surface position or hotspot.
        let output = Geometry {
            x: -1536,
            y: 120,
            width: 1536,
            height: 864,
        };
        assert!(output.contains(-1536, 120));
        assert!(output.contains(-1, 983));
        assert!(!output.contains(0, 983));
        assert!(!output.contains(-1, 984));
        assert_eq!(output.margins((-1000, 420), (8, 12)).unwrap(), (288, 528));
        assert_eq!(output.margins((-1536, 120), (8, 12)).unwrap(), (-12, -8));
    }
    #[test]
    fn output_geometry_is_published_at_done_and_updated_without_guessing() {
        let mut state = OutputState::default();
        let data = (&mut state as *mut OutputState).cast();
        unsafe {
            logical_position(data, ptr::null_mut(), 1920, 0);
        }
        assert!(state.logical.is_none());
        unsafe {
            logical_size(data, ptr::null_mut(), 1536, 864);
            logical_done(data, ptr::null_mut());
        }
        assert_eq!(state.logical.unwrap().x, 1920);
        unsafe {
            logical_position(data, ptr::null_mut(), 0, -864);
            logical_done(data, ptr::null_mut());
        }
        assert_eq!(
            state.logical.unwrap(),
            Geometry {
                x: 0,
                y: -864,
                width: 1536,
                height: 864
            }
        );
        unsafe {
            logical_size(data, ptr::null_mut(), 0, 0);
            logical_done(data, ptr::null_mut());
        }
        assert!(state.logical.is_none());
    }
    #[test]
    fn compositor_closed_marker_stops_updates() {
        let mut state = LayerState::default();
        state.require_open().unwrap();
        unsafe {
            layer_closed((&mut state as *mut LayerState).cast(), ptr::null_mut());
        }
        assert!(state.require_open().unwrap_err().contains("closed"));
    }
}
