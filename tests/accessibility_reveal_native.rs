#![cfg(all(feature = "std", target_os = "linux"))]

//! The fixture itself is Rust using GTK's public C ABI. Run this ignored test
//! binary in a private X/Wayland and accessibility-bus session with
//! ZUSE_ATSPI_FIXTURE=1; it never accepts the user's ordinary desktop session.
use std::{
    ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void},
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use zuse::{
    accessibility::{
        self,
        controls::{self, Operation, Query},
    },
    json,
    observation::Cancellation,
};

type Obj = *mut c_void;

unsafe fn symbol<T: Copy>(library: &zsys::dylib::Library, name: &CStr) -> T {
    let pointer = library.symbol(name).unwrap();
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&pointer));
    unsafe { std::mem::transmute_copy(&pointer) }
}

struct ClickState {
    label: Obj,
    set_text: unsafe extern "C" fn(Obj, *const c_char),
    clicks: usize,
}
unsafe extern "C" fn clicked(_: Obj, data: Obj) {
    let state = unsafe { &mut *data.cast::<ClickState>() };
    state.clicks += 1;
    let text = CString::new(format!("Zuse reveal clicks:{}", state.clicks)).unwrap();
    unsafe { (state.set_text)(state.label, text.as_ptr()) };
}

#[test]
#[ignore = "helper process for the isolated Rust GTK reveal fixture"]
fn native_reveal_fixture_child() {
    if std::env::var("ZUSE_REVEAL_FIXTURE_CHILD").as_deref() != Ok("1") {
        return;
    }
    assert_eq!(std::env::var("ZUSE_ATSPI_FIXTURE").as_deref(), Ok("1"));
    assert!(std::env::var("AT_SPI_BUS_ADDRESS").is_ok());
    let gtk = unsafe { zsys::dylib::Library::open(c"libgtk-3.so.0") }.unwrap();
    // Every symbol and widget stays alive until the child process exits.
    unsafe {
        let init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int =
            symbol(&gtk, c"gtk_init_check");
        let window_new: unsafe extern "C" fn(c_int) -> Obj = symbol(&gtk, c"gtk_window_new");
        let title: unsafe extern "C" fn(Obj, *const c_char) = symbol(&gtk, c"gtk_window_set_title");
        let size: unsafe extern "C" fn(Obj, c_int, c_int) =
            symbol(&gtk, c"gtk_window_set_default_size");
        let box_new: unsafe extern "C" fn(c_int, c_int) -> Obj = symbol(&gtk, c"gtk_box_new");
        let pack: unsafe extern "C" fn(Obj, Obj, c_int, c_int, c_uint) =
            symbol(&gtk, c"gtk_box_pack_start");
        let add: unsafe extern "C" fn(Obj, Obj) = symbol(&gtk, c"gtk_container_add");
        let entry_new: unsafe extern "C" fn() -> Obj = symbol(&gtk, c"gtk_entry_new");
        let label_new: unsafe extern "C" fn(*const c_char) -> Obj = symbol(&gtk, c"gtk_label_new");
        let label_text: unsafe extern "C" fn(Obj, *const c_char) =
            symbol(&gtk, c"gtk_label_set_text");
        let button_new: unsafe extern "C" fn(*const c_char) -> Obj =
            symbol(&gtk, c"gtk_button_new_with_label");
        let drawing_new: unsafe extern "C" fn() -> Obj = symbol(&gtk, c"gtk_drawing_area_new");
        let request: unsafe extern "C" fn(Obj, c_int, c_int) =
            symbol(&gtk, c"gtk_widget_set_size_request");
        let scroll_new: unsafe extern "C" fn(Obj, Obj) -> Obj =
            symbol(&gtk, c"gtk_scrolled_window_new");
        let policy: unsafe extern "C" fn(Obj, c_int, c_int) =
            symbol(&gtk, c"gtk_scrolled_window_set_policy");
        let accessible: unsafe extern "C" fn(Obj) -> Obj =
            symbol(&gtk, c"gtk_widget_get_accessible");
        let name: unsafe extern "C" fn(Obj, *const c_char) = symbol(&gtk, c"atk_object_set_name");
        let sensitive: unsafe extern "C" fn(Obj, c_int) = symbol(&gtk, c"gtk_widget_set_sensitive");
        let show: unsafe extern "C" fn(Obj) = symbol(&gtk, c"gtk_widget_show_all");
        let focus: unsafe extern "C" fn(Obj) = symbol(&gtk, c"gtk_widget_grab_focus");
        let main_loop: unsafe extern "C" fn() = symbol(&gtk, c"gtk_main");
        let connect: unsafe extern "C" fn(
            Obj,
            *const c_char,
            Option<unsafe extern "C" fn()>,
            Obj,
            Option<unsafe extern "C" fn(Obj, Obj)>,
            c_uint,
        ) -> c_ulong = symbol(&gtk, c"g_signal_connect_data");
        assert_ne!(init(std::ptr::null_mut(), std::ptr::null_mut()), 0);
        let window = window_new(0);
        title(window, c"zuse-reveal-fixture".as_ptr());
        size(window, 360, 260);
        let outer = box_new(1, 6);
        add(window, outer);
        let entry = entry_new();
        pack(outer, entry, 0, 0, 0);
        let status = label_new(c"Zuse reveal clicks:0".as_ptr());
        name(accessible(status), c"Zuse reveal status".as_ptr());
        pack(outer, status, 0, 0, 0);
        let scrolled = scroll_new(std::ptr::null_mut(), std::ptr::null_mut());
        policy(scrolled, 2, 1); // horizontal NEVER, vertical AUTOMATIC
        pack(outer, scrolled, 1, 1, 0);
        let content = box_new(1, 8);
        add(scrolled, content);
        let spacer = drawing_new();
        request(spacer, 1, 1400);
        pack(content, spacer, 0, 0, 0);
        let target = button_new(c"Zuse reveal target".as_ptr());
        pack(content, target, 0, 0, 0);
        for _ in 0..2 {
            pack(
                content,
                button_new(c"Zuse reveal duplicate".as_ptr()),
                0,
                0,
                0,
            );
        }
        let disabled = button_new(c"Zuse reveal disabled".as_ptr());
        sensitive(disabled, 0);
        pack(content, disabled, 0, 0, 0);
        let mut clicks = Box::new(ClickState {
            label: status,
            set_text: label_text,
            clicks: 0,
        });
        let callback =
            std::mem::transmute::<unsafe extern "C" fn(Obj, Obj), unsafe extern "C" fn()>(clicked);
        connect(
            target,
            c"clicked".as_ptr(),
            Some(callback),
            (&mut *clicks as *mut ClickState).cast(),
            None,
            0,
        );
        // A second top-level node proves PID discovery returns all siblings.
        let popup = window_new(0);
        title(popup, c"zuse-reveal-popup".as_ptr());
        add(popup, label_new(c"Zuse popup marker".as_ptr()));
        show(popup);
        show(window);
        focus(entry); // never let GTK's initial focus scroll to the bottom button
        println!("ZUSE_REVEAL_READY");
        std::io::stdout().flush().unwrap();
        main_loop();
        drop(clicks);
    }
}

struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_fixture() -> Fixture {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_reveal_fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .env("ZUSE_REVEAL_FIXTURE_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let fixture = Fixture(child);
    let (send, ready) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.is_ok_and(|line| line == "ZUSE_REVEAL_READY") {
                let _ = send.send(());
                break;
            }
        }
    });
    ready
        .recv_timeout(Duration::from_secs(5))
        .expect("Rust GTK fixture did not start");
    fixture
}

fn matches(query: &Query<'_>) -> Vec<String> {
    json::with_value(
        &controls::find(query, &Cancellation::default()).unwrap(),
        |root| {
            assert_eq!(root.get("errors").unwrap().integer().unwrap(), 0);
            assert_eq!(root.get("truncated").unwrap().boolean(), Some(false));
            Ok(root
                .get("matches")
                .unwrap()
                .elements()
                .map(|node| node.raw().to_owned())
                .collect())
        },
    )
    .unwrap()
}

fn showing(query: &Query<'_>) -> bool {
    let nodes = matches(query);
    assert_eq!(nodes.len(), 1);
    json::with_value(&nodes[0], |node| {
        assert_eq!(node.get("visible").unwrap().boolean(), Some(true));
        Ok(node.get("showing").unwrap().boolean() == Some(true))
    })
    .unwrap()
}

#[test]
#[ignore = "requires a private display and AT-SPI session, ZUSE_ATSPI_FIXTURE=1"]
fn native_reveal_preserves_guards_and_never_activates_an_offscreen_control() {
    assert_eq!(std::env::var("ZUSE_ATSPI_FIXTURE").as_deref(), Ok("1"));
    assert!(std::env::var("AT_SPI_BUS_ADDRESS").is_ok());
    let cancellation = Cancellation::default();
    // Initialize the persistent libatspi client before the fixture registers
    // its application/windows. Discovery must observe a later registration.
    accessibility::windows(std::process::id(), &cancellation).unwrap();
    let fixture = start_fixture();
    let pid = fixture.0.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    let scope = loop {
        let observed = accessibility::windows(pid, &cancellation).unwrap();
        let scopes = json::with_value(&observed, |root| {
            root.get("windows")
                .unwrap()
                .elements()
                .map(|window| {
                    assert_eq!(
                        window.get("pid").unwrap().integer().unwrap(),
                        i64::from(pid)
                    );
                    Ok((
                        json::text(window.get("name").unwrap())?,
                        json::text(window.get("scope").unwrap())?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .unwrap();
        if scopes.iter().any(|(name, _)| name == "zuse-reveal-popup")
            && let Some((_, scope)) = scopes
                .iter()
                .find(|(name, _)| name == "zuse-reveal-fixture")
        {
            assert_eq!(scopes.len(), 2, "all direct windows must be retained");
            break scope.clone();
        }
        assert!(
            Instant::now() < deadline,
            "fixture windows did not become enumerable"
        );
        thread::sleep(Duration::from_millis(40));
    };
    let base = Query {
        scope: &scope,
        expected_pid: pid,
        role: None,
        name: None,
        text: None,
        exact: true,
    };
    let target = Query {
        name: Some("Zuse reveal target"),
        ..base
    };
    let nodes = matches(&target);
    assert_eq!(nodes.len(), 1);
    let role = json::with_value(&nodes[0], |node| json::text(node.get("role").unwrap())).unwrap();
    let action = json::with_value(&nodes[0], |node| {
        json::text(
            node.get("actions")
                .unwrap()
                .elements()
                .next()
                .unwrap()
                .get("name")
                .unwrap(),
        )
    })
    .unwrap();
    let target = Query {
        role: Some(&role),
        ..target
    };
    assert!(
        !showing(&target),
        "fixture target must start below the scroll viewport"
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let guarded = Cancellation::with_dispatch_guard(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(()))
    });
    let error =
        controls::invoke(&target, Operation::Action(&action), None, 120, &guarded).unwrap_err();
    assert!(error.starts_with("timeout:"), "{error}");
    let duplicate = Query {
        name: Some("Zuse reveal duplicate"),
        ..target
    };
    let error = controls::invoke(&duplicate, Operation::Reveal, None, 1000, &guarded).unwrap_err();
    assert!(error.starts_with("ambiguous_target:"), "{error}");
    let disabled = Query {
        name: Some("Zuse reveal disabled"),
        ..target
    };
    let error = controls::invoke(&disabled, Operation::Reveal, None, 120, &guarded).unwrap_err();
    assert!(error.starts_with("timeout:"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let denied =
        Cancellation::with_dispatch_guard(|| Err("fixture reveal permission denied".into()));
    let error = controls::invoke(&target, Operation::Reveal, None, 1000, &denied).unwrap_err();
    assert!(error.contains("permission denied"), "{error}");
    assert!(!showing(&target));
    let permits = Arc::new(AtomicUsize::new(0));
    let delayed = permits.clone();
    let delayed_permission = Cancellation::with_dispatch_guard(move || {
        delayed.fetch_add(1, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(350));
        Ok(Box::new(()))
    });
    let error =
        controls::invoke(&target, Operation::Reveal, None, 300, &delayed_permission).unwrap_err();
    assert_eq!(
        permits.load(Ordering::SeqCst),
        1,
        "test must reach the delayed permission guard: {error}"
    );
    assert!(error.starts_with("timeout:"), "{error}");
    assert!(
        !showing(&target),
        "an expired permission guard must not scroll"
    );

    let result = controls::invoke(&target, Operation::Reveal, None, 3000, &guarded);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "native scrolling must have one dispatch only"
    );
    match result {
        Ok(receipt) => {
            json::with_value(&receipt, |root| {
                assert_eq!(root.get("scroll_submitted").unwrap().boolean(), Some(true));
                assert_eq!(
                    root.get("target")
                        .unwrap()
                        .get("showing")
                        .unwrap()
                        .boolean(),
                    Some(true)
                );
                Ok(())
            })
            .unwrap();
            assert!(showing(&target));
        }
        Err(error) => {
            // GTK 3 builds without a Component.scroll_to vfunc are a real
            // unsupported backend. They must return that failure, not fake a
            // showing receipt or activate/focus the button as a fallback.
            assert!(error.contains("unsupported or rejected"), "{error}");
            assert!(!showing(&target));
        }
    }
    let status = matches(&Query {
        name: Some("Zuse reveal status"),
        ..base
    });
    assert_eq!(status.len(), 1);
    json::with_value(&status[0], |node| {
        assert_eq!(
            json::text(node.get("text").unwrap())?,
            "Zuse reveal clicks:0"
        );
        Ok(())
    })
    .unwrap();
}
