//! Queue one AT-SPI text edit under its input permit, then await the reply.
//!
//! libatspi exposes only a synchronous SetTextContents wrapper. This adapter
//! uses that method's public D-Bus protocol and the public C layouts in
//! atspi-object.h / atspi-application.h, without private libatspi entry points.
//! Sources: https://gitlab.gnome.org/GNOME/at-spi2-core/-/tree/main/atspi
//! https://dbus.freedesktop.org/doc/api/html/group__DBusConnection.html
use super::*;
use crate::observation::Cancellation;
use std::{mem::size_of, ptr};

// GObject's public ABI is needed only as the embedded parent; none of its
// private fields is read. g_type_query checks the loaded instance sizes before
// any AtspiObject or AtspiApplication field is accessed.
#[repr(C)]
struct GObjectLayout {
    type_class: Obj,
    reference_count: u32,
    qdata: Obj,
}
#[repr(C)]
struct AtspiObjectLayout {
    parent: GObjectLayout,
    app: *mut ApplicationPrefix,
    path: *const c_char,
}
#[repr(C)]
struct ApplicationPrefix {
    parent: GObjectLayout,
    hash: Obj,
    bus_name: *const c_char,
    bus: Obj,
}
#[repr(C)]
struct TypeQuery {
    kind: usize,
    name: *const c_char,
    class_size: u32,
    instance_size: u32,
}

struct BusApi {
    message_new:
        unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> Obj,
    message_append: unsafe extern "C" fn(Obj, c_int, ...) -> u32,
    message_get: unsafe extern "C" fn(Obj, Obj, c_int, ...) -> u32,
    message_type: unsafe extern "C" fn(Obj) -> c_int,
    message_signature: unsafe extern "C" fn(Obj) -> *const c_char,
    message_error: unsafe extern "C" fn(Obj) -> *const c_char,
    message_unref: unsafe extern "C" fn(Obj),
    connection_ref: unsafe extern "C" fn(Obj) -> Obj,
    connection_unref: unsafe extern "C" fn(Obj),
    send: unsafe extern "C" fn(Obj, Obj, *mut Obj, c_int) -> u32,
    poll: unsafe extern "C" fn(Obj, c_int) -> u32,
    completed: unsafe extern "C" fn(Obj) -> u32,
    reply: unsafe extern "C" fn(Obj) -> Obj,
    cancel: unsafe extern "C" fn(Obj),
    pending_unref: unsafe extern "C" fn(Obj),
}
static API: OnceLock<Result<BusApi>> = OnceLock::new();
const REPLY_TIMEOUT_MS: c_int = 1000;
const POLL_MS: c_int = 10;

fn bus_api() -> Result<&'static BusApi> {
    API.get_or_init(|| {
        let atspi = Library::open(c"libatspi.so.0")?;
        let dbus = Library::open(c"libdbus-1.so.3")?;
        // SAFETY: all signatures and layouts follow the installed libraries'
        // public headers. No object fields are read while checking their ABI.
        let api = unsafe {
            let query: unsafe extern "C" fn(usize, *mut TypeQuery) =
                atspi.symbol(c"g_type_query")?;
            for (name, minimum, exact) in [
                (c"g_object_get_type", size_of::<GObjectLayout>(), true),
                (
                    c"atspi_object_get_type",
                    size_of::<AtspiObjectLayout>(),
                    true,
                ),
                (
                    c"atspi_application_get_type",
                    size_of::<ApplicationPrefix>(),
                    false,
                ),
            ] {
                let get_type: unsafe extern "C" fn() -> usize = atspi.symbol(name)?;
                let mut info = TypeQuery {
                    kind: 0,
                    name: ptr::null(),
                    class_size: 0,
                    instance_size: 0,
                };
                query(get_type(), &mut info);
                let actual = info.instance_size as usize;
                if actual < minimum || (exact && actual != minimum) {
                    return Err(format!(
                        "unsupported AT-SPI public object ABI ({})",
                        name.to_string_lossy()
                    ));
                }
            }
            BusApi {
                message_new: dbus.symbol(c"dbus_message_new_method_call")?,
                message_append: dbus.symbol(c"dbus_message_append_args")?,
                message_get: dbus.symbol(c"dbus_message_get_args")?,
                message_type: dbus.symbol(c"dbus_message_get_type")?,
                message_signature: dbus.symbol(c"dbus_message_get_signature")?,
                message_error: dbus.symbol(c"dbus_message_get_error_name")?,
                message_unref: dbus.symbol(c"dbus_message_unref")?,
                connection_ref: dbus.symbol(c"dbus_connection_ref")?,
                connection_unref: dbus.symbol(c"dbus_connection_unref")?,
                send: dbus.symbol(c"dbus_connection_send_with_reply")?,
                poll: dbus.symbol(c"dbus_connection_read_write_dispatch")?,
                completed: dbus.symbol(c"dbus_pending_call_get_completed")?,
                reply: dbus.symbol(c"dbus_pending_call_steal_reply")?,
                cancel: dbus.symbol(c"dbus_pending_call_cancel")?,
                pending_unref: dbus.symbol(c"dbus_pending_call_unref")?,
            }
        };
        atspi.pin();
        dbus.pin();
        Ok(api)
    })
    .as_ref()
    .map_err(Clone::clone)
}

struct Request {
    api: &'static BusApi,
    connection: Obj,
    message: Obj,
    pending: Obj,
}
impl Request {
    fn new(object: &Object<'_>, text: &CStr) -> Result<Self> {
        let api = bus_api()?;
        // SAFETY: the caller holds ACCESS and an owned AtspiAccessible reference.
        // AtspiAccessible publicly embeds AtspiObject. The named public fields
        // are inspected only after the ABI check above, never by numeric offset.
        unsafe {
            let endpoint = &*object.ptr.cast::<AtspiObjectLayout>();
            let app = endpoint
                .app
                .as_ref()
                .ok_or("stale_target: accessibility application is unavailable")?;
            if endpoint.path.is_null() || app.bus_name.is_null() || app.bus.is_null() {
                return Err("stale_target: accessibility text endpoint is unavailable".into());
            }
            // The app may use a direct connection. atspi_get_a11y_bus() returns
            // the registry bus, not necessarily the connection of this object.
            let message = (api.message_new)(
                app.bus_name,
                endpoint.path,
                c"org.a11y.atspi.EditableText".as_ptr(),
                c"SetTextContents".as_ptr(),
            );
            if message.is_null() {
                return Err("cannot allocate accessibility text request".into());
            }
            let request = Self {
                api,
                connection: (api.connection_ref)(app.bus),
                message,
                pending: ptr::null_mut(),
            };
            let value = text.as_ptr();
            if (api.message_append)(
                message,
                b's' as c_int,
                &value as *const *const c_char,
                0 as c_int,
            ) == 0
            {
                return Err("cannot encode accessibility text request".into());
            }
            Ok(request)
        }
    }

    // This method must run inside Cancellation::dispatch. send_with_reply
    // queues the message; it neither flushes the socket nor waits for a reply.
    fn submit(mut self) -> Result<Self> {
        let sent = unsafe {
            (self.api.send)(
                self.connection,
                self.message,
                &mut self.pending,
                REPLY_TIMEOUT_MS,
            )
        };
        if sent == 0 {
            return Err("cannot queue accessibility text request".into());
        }
        if self.pending.is_null() {
            return Err("accessibility bus disconnected before text submission".into());
        }
        Ok(self)
    }

    fn wait(&self, cancellation: &Cancellation) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(REPLY_TIMEOUT_MS as u64);
        loop {
            cancellation.check()?;
            if unsafe { (self.api.completed)(self.pending) } != 0 {
                break;
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for accessibility text reply".into());
            }
            // Only the I/O slice is bounded here. Existing native callbacks
            // can run during dispatch; none hold the input permit. Do not use
            // dbus_pending_call_block or a blocking flush under that permit.
            if unsafe { (self.api.poll)(self.connection, POLL_MS) } == 0 {
                cancellation.check()?;
                return Err("accessibility bus disconnected while awaiting text reply".into());
            }
        }
        cancellation.check()?;
        let reply = unsafe { (self.api.reply)(self.pending) };
        if reply.is_null() {
            return Err("accessibility text reply is unavailable".into());
        }
        let result = self.decode_reply(reply);
        // SAFETY: steal_reply transfers one reference; decode only borrows it.
        unsafe { (self.api.message_unref)(reply) };
        result
    }

    fn decode_reply(&self, reply: Obj) -> Result<()> {
        // SAFETY: this is a live owned D-Bus message. get_args receives pointers
        // of the exact declared wire types and a null optional DBusError.
        unsafe {
            if (self.api.message_type)(reply) == 3 {
                let name = bounded_string((self.api.message_error)(reply));
                let mut detail: *const c_char = ptr::null();
                (self.api.message_get)(
                    reply,
                    ptr::null_mut(),
                    b's' as c_int,
                    &mut detail as *mut *const c_char,
                    0 as c_int,
                );
                return Err(format!(
                    "AT-SPI text input error {name}: {}",
                    bounded_string(detail)
                ));
            }
            let signature = (self.api.message_signature)(reply);
            if (self.api.message_type)(reply) != 2
                || signature.is_null()
                || CStr::from_ptr(signature) != c"b"
            {
                return Err("invalid accessibility text reply (expected one Boolean)".into());
            }
            let mut success: u32 = 0;
            if (self.api.message_get)(
                reply,
                ptr::null_mut(),
                b'b' as c_int,
                &mut success as *mut u32,
                0 as c_int,
            ) == 0
            {
                return Err("cannot decode accessibility text reply".into());
            }
            if success == 0 {
                return Err("application rejected accessibility text input".into());
            }
        }
        Ok(())
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        // Cancelling a pending reply only stops local reply tracking. It does
        // not retract a queued request, so all wait errors report submission.
        unsafe {
            if !self.pending.is_null() {
                (self.api.cancel)(self.pending);
                (self.api.pending_unref)(self.pending);
            }
            (self.api.message_unref)(self.message);
            (self.api.connection_unref)(self.connection);
        }
    }
}

unsafe fn bounded_string(value: *const c_char) -> String {
    if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .chars()
            .take(4096)
            .collect()
    }
}

fn submitted_error(error: String) -> String {
    if error.starts_with("cancelled:") {
        "cancelled: accessibility text was submitted; completion was not confirmed; inspect the current draft before retrying".into()
    } else {
        format!("accessibility text was submitted; {error}")
    }
}

pub(super) fn check_submitted(cancellation: &Cancellation) -> Result<()> {
    cancellation.check().map_err(submitted_error)
}

// The helper makes the permit lifetime explicit and tests the same boundary
// used by the native path, without requiring a bus or a graphical session.
fn submit_and_wait<T>(
    cancellation: &Cancellation,
    submit: impl FnOnce() -> Result<T>,
    wait: impl FnOnce(T) -> Result<()>,
) -> Result<()> {
    let pending = cancellation.dispatch(submit)?;
    wait(pending).map_err(submitted_error)
}

pub(super) fn set_contents(
    object: &Object<'_>,
    text: &CStr,
    cancellation: &Cancellation,
) -> Result<()> {
    let request = Request::new(object, text)?;
    submit_and_wait(
        cancellation,
        || request.submit(),
        |request| request.wait(cancellation),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    struct Permit(Arc<AtomicBool>);
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    #[test]
    fn cancellation_does_not_wait_for_the_application_reply() {
        let held = Arc::new(AtomicBool::new(false));
        let acquire = held.clone();
        let cancellation = Cancellation::with_dispatch_guard(move || {
            acquire.store(true, Ordering::Release);
            Ok(Box::new(Permit(acquire.clone())))
        });
        let worker_token = cancellation.clone();
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            submit_and_wait(
                &worker_token,
                || Ok(()),
                |()| {
                    waiting_tx.send(held.load(Ordering::Acquire)).unwrap();
                    reply_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    worker_token.check()
                },
            )
        });
        let permit_still_held = waiting_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let canceller = std::thread::spawn(move || {
            cancellation.cancel();
            cancel_tx.send(()).unwrap();
        });
        let cancelled_before_reply = cancel_rx.recv_timeout(Duration::from_millis(250)).is_ok();
        reply_tx.send(()).unwrap();
        canceller.join().unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert!(
            !permit_still_held,
            "host permit must end at request submission"
        );
        assert!(cancelled_before_reply, "cancellation waited for the reply");
        assert!(error.starts_with("cancelled:"), "{error}");
        assert!(error.contains("submitted"), "{error}");
    }

    #[test]
    fn revoked_or_cancelled_input_never_submits_a_request() {
        let cancelled = Cancellation::default();
        cancelled.cancel();
        let denied = Cancellation::with_dispatch_guard(|| Err("permission revoked".into()));
        for cancellation in [cancelled, denied] {
            let submitted = AtomicBool::new(false);
            let error = submit_and_wait(
                &cancellation,
                || {
                    submitted.store(true, Ordering::Release);
                    Ok(())
                },
                |()| Ok(()),
            )
            .unwrap_err();
            assert!(!submitted.load(Ordering::Acquire));
            assert!(!error.contains("was submitted"), "{error}");
        }
    }
}
