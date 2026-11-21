//! AT-SPI subscriptions use the existing serialized native connection.
use super::*;
use crate::observation::Cancellation;
use std::{
    ptr,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

// Only the public prefix is inspected; the trailing GValue remains opaque.
#[repr(C)]
struct EventPrefix {
    kind: *mut c_char,
    source: Obj,
    detail1: c_int,
    detail2: c_int,
}
type Callback = unsafe extern "C" fn(*mut EventPrefix, Obj);
type Destroy = unsafe extern "C" fn(Obj);

struct EventApi {
    listener: unsafe extern "C" fn(Callback, Obj, Option<Destroy>) -> Obj,
    register: unsafe extern "C" fn(Obj, *const c_char, Obj, Obj, *mut ErrPtr) -> c_int,
    deregister: unsafe extern "C" fn(Obj, *const c_char, *mut ErrPtr) -> c_int,
    application: unsafe extern "C" fn(Obj, *mut ErrPtr) -> Obj,
    parent: unsafe extern "C" fn(Obj, *mut ErrPtr) -> Obj,
    index: unsafe extern "C" fn(Obj, *mut ErrPtr) -> c_int,
    event_type: unsafe extern "C" fn() -> usize,
    boxed_free: unsafe extern "C" fn(usize, Obj),
    reference: unsafe extern "C" fn(Obj) -> Obj,
    context: unsafe extern "C" fn() -> Obj,
    iteration: unsafe extern "C" fn(Obj, c_int) -> c_int,
    wakeup: unsafe extern "C" fn(Obj),
    timer: unsafe extern "C" fn(u32) -> Obj,
    source_callback:
        unsafe extern "C" fn(Obj, unsafe extern "C" fn(Obj) -> c_int, Obj, Option<Destroy>),
    source_attach: unsafe extern "C" fn(Obj, Obj) -> u32,
    source_destroy: unsafe extern "C" fn(Obj),
    source_unref: unsafe extern "C" fn(Obj),
}

static API: OnceLock<std::result::Result<EventApi, String>> = OnceLock::new();
fn event_api() -> Result<&'static EventApi> {
    API.get_or_init(|| {
        let library = Library::open(c"libatspi.so.0")?;
        // These signatures come from the public libatspi and GLib headers.
        let api = unsafe {
            EventApi {
                listener: library.symbol(c"atspi_event_listener_new")?,
                register: library.symbol(c"atspi_event_listener_register_with_app")?,
                deregister: library.symbol(c"atspi_event_listener_deregister")?,
                application: library.symbol(c"atspi_accessible_get_application")?,
                parent: library.symbol(c"atspi_accessible_get_parent")?,
                index: library.symbol(c"atspi_accessible_get_index_in_parent")?,
                event_type: library.symbol(c"atspi_event_get_type")?,
                boxed_free: library.symbol(c"g_boxed_free")?,
                reference: library.symbol(c"g_object_ref")?,
                context: library.symbol(c"g_main_context_default")?,
                iteration: library.symbol(c"g_main_context_iteration")?,
                wakeup: library.symbol(c"g_main_context_wakeup")?,
                timer: library.symbol(c"g_timeout_source_new")?,
                source_callback: library.symbol(c"g_source_set_callback")?,
                source_attach: library.symbol(c"g_source_attach")?,
                source_destroy: library.symbol(c"g_source_destroy")?,
                source_unref: library.symbol(c"g_source_unref")?,
            }
        };
        library.pin();
        Ok(api)
    })
    .as_ref()
    .map_err(Clone::clone)
}

const EVENTS: [&std::ffi::CStr; 6] = [
    c"object:children-changed",
    c"object:property-change",
    c"object:text-changed",
    c"object:state-changed",
    c"object:bounds-changed",
    c"window:destroy",
];

struct State {
    api: &'static Api,
    events: &'static EventApi,
    root: Obj,
    generation: AtomicU64,
    dirty: AtomicBool,
}

unsafe extern "C" fn release_state(data: Obj) {
    if !data.is_null() {
        // The listener owns exactly the Box passed at construction.
        unsafe {
            drop(Box::from_raw(data.cast::<State>()));
        }
    }
}

unsafe extern "C" fn event_callback(event: *mut EventPrefix, data: Obj) {
    if event.is_null() || data.is_null() {
        return;
    }
    // GLib dispatch is serialized with all other AT-SPI calls by ACCESS.
    let state = unsafe { &*data.cast::<State>() };
    let source = unsafe { (*event).source };
    let mut current = source;
    let mut owned = false;
    for _ in 0..34 {
        if current.is_null() {
            break;
        }
        if current == state.root {
            state.dirty.store(true, Ordering::Relaxed);
            state.generation.fetch_add(1, Ordering::Relaxed);
            break;
        }
        let mut error = ptr::null_mut();
        let parent = unsafe { (state.events.parent)(current, &mut error) };
        if owned {
            unsafe {
                (state.api.unref)(current);
            }
        }
        current = parent;
        owned = !current.is_null();
        if state.api.error(error).is_err() {
            break;
        }
    }
    if owned && !current.is_null() {
        unsafe {
            (state.api.unref)(current);
        }
    }
    // EventListenerCB receives an owned boxed AtspiEvent, including its source.
    unsafe {
        (state.events.boxed_free)((state.events.event_type)(), event.cast());
    }
}

pub struct EventWatch {
    api: &'static Api,
    events: &'static EventApi,
    root: Object<'static>,
    application: Object<'static>,
    application_index: usize,
    listener: Obj,
    state: *mut State,
    registered: usize,
    cached: String,
}

impl EventWatch {
    pub fn new(scope: &str) -> Result<Self> {
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let api = load()?;
        let events = event_api()?;
        let root = api.resolve(scope)?;
        let mut error = ptr::null_mut();
        let app = unsafe { (events.application)(root.ptr, &mut error) };
        if let Err(error) = api.error(error) {
            if !app.is_null() {
                unsafe {
                    (api.unref)(app);
                }
            }
            return Err(error);
        }
        let app = api.object(app)?;
        let state = Box::into_raw(Box::new(State {
            api,
            events,
            root: root.ptr,
            generation: AtomicU64::new(0),
            dirty: AtomicBool::new(true),
        }));
        let listener =
            unsafe { (events.listener)(event_callback, state.cast(), Some(release_state)) };
        if listener.is_null() {
            unsafe {
                drop(Box::from_raw(state));
            }
            return Err("AT-SPI could not create an event listener".into());
        }
        let application_index = scope
            .split('/')
            .nth(1)
            .and_then(|value| value.parse().ok())
            .ok_or("accessibility watch requires an application path")?;
        let mut result = Self {
            api,
            events,
            root,
            application: app,
            application_index,
            listener,
            state,
            registered: 0,
            cached: String::new(),
        };
        for event in EVENTS {
            let mut error = ptr::null_mut();
            let accepted = unsafe {
                (events.register)(
                    listener,
                    event.as_ptr(),
                    ptr::null_mut(),
                    result.application.ptr,
                    &mut error,
                )
            };
            let error = api.error(error);
            if accepted == 0 || error.is_err() {
                result.cleanup();
                return Err(error
                    .err()
                    .unwrap_or_else(|| "AT-SPI refused scoped event subscription".into()));
            }
            result.registered += 1;
        }
        Ok(result)
    }

    pub fn snapshot(&mut self) -> Result<String> {
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let context = unsafe { (self.events.context)() };
        // Drain only already queued events. No timer or repeated desktop query.
        for _ in 0..256 {
            if unsafe { (self.events.iteration)(context, 0) } == 0 {
                break;
            }
        }
        if unsafe { (*self.state).dirty.load(Ordering::Relaxed) } {
            let generation = unsafe { (*self.state).generation.load(Ordering::Relaxed) };
            unsafe {
                (self.api.clear_cache)(self.root.ptr);
            }
            let states = self
                .api
                .object(unsafe { (self.api.state_set)(self.root.ptr) })?;
            // ATSPI_STATE_DEFUNCT == 6.
            let defunct = unsafe { (self.api.state_contains)(states.ptr, 6) } != 0;
            self.cached = if defunct {
                r#"{"backend":"atspi","connected":false,"nodes":[],"truncated":false,"errors":0}"#
                    .into()
            } else {
                let path = self.current_path()?;
                let reference = self
                    .api
                    .object(unsafe { (self.events.reference)(self.root.ptr) })?;
                scan_root(self.api, reference, &path, 512, 16)?
            };
            // Synchronous D-Bus calls may dispatch a newly queued event.
            if unsafe { (*self.state).generation.load(Ordering::Relaxed) } == generation {
                unsafe {
                    (*self.state).dirty.store(false, Ordering::Relaxed);
                }
            }
        }
        Ok(self.cached.clone())
    }

    fn current_path(&self) -> Result<String> {
        let desktop = self.api.object(unsafe { (self.api.desktop)(0) })?;
        let mut error = ptr::null_mut();
        let application =
            unsafe { (self.api.child)(desktop.ptr, self.application_index as c_int, &mut error) };
        if let Err(error) = self.api.error(error) {
            if !application.is_null() {
                unsafe {
                    (self.api.unref)(application);
                }
            }
            return Err(error);
        }
        let application = self.api.object(application)?;
        if application.ptr != self.application.ptr {
            return Err(
                "observed application index changed; its original scope must be observed again"
                    .into(),
            );
        }
        let mut current = self
            .api
            .object(unsafe { (self.events.reference)(self.root.ptr) })?;
        let mut indices = Vec::new();
        // The registry's desktop is not a parent supplied by the toolkit.
        // Application roots can legitimately report index=-1. Verify that
        // original application once above; ask indices only inside its tree.
        while current.ptr != self.application.ptr {
            if indices.len() == 32 {
                return Err("observed accessibility scope exceeds maximum depth".into());
            }
            let mut error = ptr::null_mut();
            let index = unsafe { (self.events.index)(current.ptr, &mut error) };
            self.api.error(error)?;
            if index < 0 {
                return Err(format!(
                    "observed accessibility scope has no parent index at ancestor depth {}",
                    indices.len()
                ));
            }
            indices.push(index);
            let mut error = ptr::null_mut();
            let parent = unsafe { (self.events.parent)(current.ptr, &mut error) };
            if let Err(error) = self.api.error(error) {
                if !parent.is_null() {
                    unsafe {
                        (self.api.unref)(parent);
                    }
                }
                return Err(error);
            }
            current = self.api.object(parent)?;
        }
        let mut path = format!("0/{}", self.application_index);
        for index in indices.iter().rev() {
            path.push_str(&format!("/{index}"));
        }
        Ok(path)
    }

    pub fn wait_event(&mut self, deadline: Instant, cancellation: &Cancellation) -> Result<bool> {
        cancellation.check()?;
        let _guard = ACCESS
            .lock()
            .map_err(|_| "accessibility state is poisoned")?;
        let context = unsafe { (self.events.context)() };
        let wakeup = self.events.wakeup;
        let context_word = context as usize;
        let _wake = cancellation.on_cancel(move || unsafe {
            wakeup(context_word as Obj);
        });
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let timer = Timer::new(self.events, context, remaining)?;
        loop {
            cancellation.check()?;
            if unsafe { (*self.state).dirty.load(Ordering::Relaxed) } {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            // Blocks in GLib until a native accessibility event, deadline source
            // or cancellation wakeup. The selected subtree is not queried here.
            unsafe {
                (self.events.iteration)(context, 1);
            }
            let _ = &timer;
        }
    }

    fn cleanup(&mut self) {
        if self.listener.is_null() {
            return;
        }
        for event in &EVENTS[..self.registered] {
            let mut error = ptr::null_mut();
            unsafe {
                (self.events.deregister)(self.listener, event.as_ptr(), &mut error);
            }
            let _ = self.api.error(error);
        }
        unsafe {
            (self.api.unref)(self.listener);
        }
        self.listener = ptr::null_mut();
    }
}

impl Drop for EventWatch {
    fn drop(&mut self) {
        // Constructor cleanup already holds ACCESS. Its listener is null.
        if self.listener.is_null() {
            return;
        }
        if let Ok(_guard) = ACCESS.lock() {
            self.cleanup();
        }
    }
}

struct Timer {
    api: &'static EventApi,
    source: Obj,
}
unsafe extern "C" fn deadline_ready(_: Obj) -> c_int {
    0
}
impl Timer {
    fn new(api: &'static EventApi, context: Obj, duration: Duration) -> Result<Self> {
        let millis = duration
            .as_millis()
            .saturating_add(1)
            .min(u128::from(u32::MAX)) as u32;
        let source = unsafe { (api.timer)(millis) };
        if source.is_null() {
            return Err("GLib could not allocate the observation deadline".into());
        }
        unsafe {
            (api.source_callback)(source, deadline_ready, ptr::null_mut(), None);
        }
        if unsafe { (api.source_attach)(source, context) } == 0 {
            unsafe {
                (api.source_unref)(source);
            }
            return Err("GLib could not attach the observation deadline".into());
        }
        Ok(Self { api, source })
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            (self.api.source_destroy)(self.source);
            (self.api.source_unref)(self.source);
        }
    }
}
