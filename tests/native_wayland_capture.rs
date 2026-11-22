#![cfg(all(feature = "std", feature = "vision", target_os = "linux"))]

//! Owned-window regression for surface-bound Hyprland capture. Copy this file
//! into tests/ and compile with --features vision. Run the ignored parent test
//! only inside the existing private, headless Hyprland runner. The runner must
//! additionally set ZUSE_NATIVE_WINDOW_CAPTURE_FIXTURE=1. This test starts GTK
//! fixture children, never a compositor or an alternative control program.
use std::{
    ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong, c_void},
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zuse::{
    json,
    runtime::{self, Options, Runtime},
    vision::GrayImage,
};

type Obj = *mut c_void;
const DARK: u8 = 32;
const LIGHT: u8 = 224;

fn private_runtime() -> PathBuf {
    for flag in [
        "ZUSE_PRIVATE_WAYLAND_FIXTURE",
        "ZUSE_PRIVATE_WAYLAND_HEADLESS",
        "ZUSE_NATIVE_WINDOW_CAPTURE_FIXTURE",
    ] {
        assert_eq!(
            std::env::var(flag).as_deref(),
            Ok("1"),
            "{flag}=1 is required; never use a host desktop"
        );
    }
    assert_eq!(std::env::var("ZUSE_BACKEND").as_deref(), Ok("wayland"));
    assert_eq!(
        std::env::var("XDG_CURRENT_DESKTOP").as_deref(),
        Ok("zuse-private-headless")
    );
    assert!(
        std::env::var_os("DISPLAY").is_none(),
        "no X11 fallback is allowed"
    );
    assert!(
        std::env::var_os("WAYLAND_SOCKET").is_none(),
        "an inherited socket FD is not an owned-display proof"
    );
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("private runtime directory"))
            .canonicalize()
            .unwrap();
    let metadata = fs::metadata(&runtime).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(
        metadata.permissions().mode() & 0o077,
        0,
        "private runtime must not be shared"
    );
    assert_eq!(metadata.uid(), fs::metadata("/proc/self").unwrap().uid());
    let display =
        PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").expect("private Wayland socket"));
    let display = if display.is_absolute() {
        display
    } else {
        runtime.join(display)
    }
    .canonicalize()
    .unwrap();
    assert!(
        display.starts_with(&runtime),
        "Wayland socket escaped the private runtime"
    );
    assert!(fs::metadata(display).unwrap().file_type().is_socket());
    let signature =
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE").expect("private Hyprland instance");
    assert!(
        !signature.is_empty() && !signature.contains('/') && signature != "." && signature != ".."
    );
    let ipc = runtime
        .join("hypr")
        .join(signature)
        .join(".socket.sock")
        .canonicalize()
        .unwrap();
    assert!(
        ipc.starts_with(&runtime),
        "Hyprland IPC escaped the private runtime"
    );
    assert!(fs::metadata(ipc).unwrap().file_type().is_socket());
    runtime
}

unsafe fn symbol<T: Copy>(library: &zsys::dylib::Library, name: &CStr) -> T {
    let pointer = library.symbol(name).unwrap();
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&pointer));
    unsafe { std::mem::transmute_copy(&pointer) }
}

struct Paint {
    shade: f64,
    set_rgb: unsafe extern "C" fn(Obj, f64, f64, f64),
    paint: unsafe extern "C" fn(Obj),
    ready: bool,
}

unsafe extern "C" fn draw(_: Obj, cairo: Obj, data: Obj) -> c_int {
    let state = unsafe { &mut *data.cast::<Paint>() };
    unsafe {
        (state.set_rgb)(cairo, state.shade, state.shade, state.shade);
        (state.paint)(cairo);
    }
    if !state.ready {
        state.ready = true;
        println!("ZUSE_CAPTURE_PAINTED");
        let _ = std::io::stdout().flush();
    }
    1
}

#[test]
#[ignore = "helper child for the explicitly enabled private window-capture fixture"]
fn native_window_capture_fixture_child() {
    if std::env::var("ZUSE_WINDOW_CAPTURE_CHILD").as_deref() != Ok("1") {
        return;
    }
    private_runtime();
    assert_eq!(std::env::var("GDK_BACKEND").as_deref(), Ok("wayland"));
    let title = CString::new(std::env::var("ZUSE_WINDOW_CAPTURE_TITLE").unwrap()).unwrap();
    let shade: u8 = std::env::var("ZUSE_WINDOW_CAPTURE_SHADE")
        .unwrap()
        .parse()
        .unwrap();
    assert!([DARK, LIGHT].contains(&shade));
    let gtk = unsafe { zsys::dylib::Library::open(c"libgtk-3.so.0") }.unwrap();
    let cairo = unsafe { zsys::dylib::Library::open(c"libcairo.so.2") }.unwrap();
    unsafe {
        let init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int =
            symbol(&gtk, c"gtk_init_check");
        let window_new: unsafe extern "C" fn(c_int) -> Obj = symbol(&gtk, c"gtk_window_new");
        let set_title: unsafe extern "C" fn(Obj, *const c_char) =
            symbol(&gtk, c"gtk_window_set_title");
        let set_size: unsafe extern "C" fn(Obj, c_int, c_int) =
            symbol(&gtk, c"gtk_window_set_default_size");
        let decorate: unsafe extern "C" fn(Obj, c_int) = symbol(&gtk, c"gtk_window_set_decorated");
        let drawing_new: unsafe extern "C" fn() -> Obj = symbol(&gtk, c"gtk_drawing_area_new");
        let add: unsafe extern "C" fn(Obj, Obj) = symbol(&gtk, c"gtk_container_add");
        let show: unsafe extern "C" fn(Obj) = symbol(&gtk, c"gtk_widget_show_all");
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
        set_title(window, title.as_ptr());
        set_size(window, 320, 240);
        decorate(window, 0);
        let area = drawing_new();
        add(window, area);
        let mut state = Box::new(Paint {
            shade: f64::from(shade) / 255.0,
            set_rgb: symbol(&cairo, c"cairo_set_source_rgb"),
            paint: symbol(&cairo, c"cairo_paint"),
            ready: false,
        });
        let callback = std::mem::transmute::<
            unsafe extern "C" fn(Obj, Obj, Obj) -> c_int,
            unsafe extern "C" fn(),
        >(draw);
        connect(
            area,
            c"draw".as_ptr(),
            Some(callback),
            (&mut *state as *mut Paint).cast(),
            None,
            0,
        );
        show(window);
        main_loop();
        drop(state);
    }
}

struct Fixture {
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    pid: u32,
    title: String,
}

impl Fixture {
    fn start(title: String, shade: u8) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_window_capture_fixture_child",
                "--ignored",
                "--nocapture",
            ])
            .env("ZUSE_WINDOW_CAPTURE_CHILD", "1")
            .env("ZUSE_WINDOW_CAPTURE_TITLE", &title)
            .env("ZUSE_WINDOW_CAPTURE_SHADE", shade.to_string())
            .env("GDK_BACKEND", "wayland")
            .env("GTK_CSD", "0")
            .env("NO_AT_BRIDGE", "1")
            .env("GTK_A11Y", "none")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let pid = child.id();
        let stdout = child.stdout.take().unwrap();
        let (sender, ready) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line.is_ok_and(|line| line.ends_with("ZUSE_CAPTURE_PAINTED")) {
                    let _ = sender.send(());
                    break;
                }
            }
        });
        let fixture = Self {
            child: Some(child),
            reader: Some(reader),
            pid,
            title,
        };
        ready
            .recv_timeout(Duration::from_secs(8))
            .expect("owned GTK window did not paint");
        fixture
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Workspace(PathBuf);
impl Workspace {
    fn new(runtime: &Path, nonce: &str) -> Self {
        let path = runtime.join(format!("capture-fixture-{nonce}"));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn execute(runtime: &mut Runtime, source: &str, read_only: bool) -> Result<String, String> {
    let output = runtime.execute(
        source,
        Options {
            read_only,
            ..Options::default()
        },
    )?;
    if runtime::failed(&output) {
        return Err(output);
    }
    json::with_value(&output, |root| {
        root.get("results")
            .and_then(|value| value.elements().last())
            .and_then(|value| value.get("value"))
            .map(|value| value.raw().to_owned())
            .ok_or_else(|| format!("missing runtime value: {output}"))
    })
}

#[derive(Debug)]
struct Window {
    id: String,
    focused: bool,
    mapped: bool,
    geometry: [i64; 4],
    workspace: String,
    monitor: Option<String>,
    scale: String,
}

fn window(runtime: &mut Runtime, fixture: &Fixture) -> Option<Window> {
    let snapshot = execute(runtime, "window.list()", true).unwrap();
    json::with_value(&snapshot, |root| {
        let matches = root
            .elements()
            .filter(|row| {
                row.get("pid").and_then(|pid| pid.integer().ok()) == Some(i64::from(fixture.pid))
                    && row
                        .get("title")
                        .and_then(|name| name.string())
                        .is_some_and(|name| name.matches(&fixture.title))
            })
            .collect::<Vec<_>>();
        assert!(
            matches.len() <= 1,
            "owned PID+title must identify one window: {snapshot}"
        );
        matches
            .first()
            .map(|row| {
                Ok(Window {
                    id: json::text(row.get("id").unwrap())?,
                    focused: row.get("focused").and_then(|value| value.boolean()) == Some(true),
                    mapped: row.get("mapped").and_then(|value| value.boolean()) == Some(true),
                    geometry: ["x", "y", "width", "height"]
                        .map(|key| row.get(key).unwrap().integer().unwrap()),
                    workspace: row.get("workspace").unwrap().raw().to_owned(),
                    monitor: row
                        .get("monitor")
                        .filter(|value| value.raw() != "null")
                        .map(|value| value.raw().to_owned()),
                    scale: row.get("scale").unwrap().raw().to_owned(),
                })
            })
            .transpose()
    })
    .unwrap()
}

fn wait_window(runtime: &mut Runtime, fixture: &Fixture, focused: bool) -> Window {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(window) = window(runtime, fixture)
            && window.mapped
            && (!focused || window.focused)
        {
            return window;
        }
        assert!(
            Instant::now() < deadline,
            "owned window did not reach mapped/focused state: {}",
            fixture.title
        );
        thread::sleep(Duration::from_millis(30));
    }
}

fn focus(runtime: &mut Runtime, fixture: &Fixture, id: &str) {
    execute(
        runtime,
        &format!("window.focus(id={})", json::quote(id)),
        false,
    )
    .unwrap();
    assert_eq!(
        wait_window(runtime, fixture, true).id,
        id,
        "focus must preserve window identity"
    );
}

fn capture(runtime: &mut Runtime, id: &str, path: &Path) -> Result<String, String> {
    execute(
        runtime,
        &format!(
            "window.capture(id={},path={})",
            json::quote(id),
            json::quote(path.to_str().unwrap())
        ),
        true,
    )
}

fn assert_owned_image(receipt: &str, path: &Path, id: &str, expected: u8) {
    let image = GrayImage::load_png(path).unwrap();
    assert!(
        image.width() >= 64 && image.height() >= 64,
        "fixture image must be a real window buffer"
    );
    json::with_value(receipt, |root| {
        assert_eq!(
            json::text(root.get("captureMode").unwrap())?,
            "hyprland-toplevel-export"
        );
        assert_eq!(
            json::text(root.get("coordinateSpace").unwrap())?,
            "window-local"
        );
        assert_eq!(json::text(root.get("window").unwrap())?, id);
        assert_eq!(
            root.get("width").unwrap().integer().unwrap(),
            i64::from(image.width())
        );
        assert_eq!(
            root.get("height").unwrap().integer().unwrap(),
            i64::from(image.height())
        );
        let region = root.get("logicalRegion").unwrap();
        assert_eq!(region.get("x").unwrap().integer().unwrap(), 0);
        assert_eq!(region.get("y").unwrap().integer().unwrap(), 0);
        let width = region.get("width").unwrap().raw().parse::<f64>().unwrap();
        let height = region.get("height").unwrap().raw().parse::<f64>().unwrap();
        assert!(width > 0.0 && height > 0.0);
        let ratio = root.get("pixelsPerLogicalUnit").unwrap();
        for (key, expected) in [
            ("x", f64::from(image.width()) / width),
            ("y", f64::from(image.height()) / height),
        ] {
            let actual = ratio.get(key).unwrap().raw().parse::<f64>().unwrap();
            assert!(actual.is_finite() && (actual - expected).abs() < 1e-6);
        }
        Ok(())
    })
    .unwrap();
    // These are image assertions only; window selection and focus never use
    // pixel or desktop coordinates. Ignore a narrow edge and possible cursor.
    let width = image.width() as usize;
    let height = image.height() as usize;
    let (mut matching, mut opposite, mut sampled) = (0usize, 0usize, 0usize);
    let other = if expected == DARK { LIGHT } else { DARK };
    for y in height / 10..height * 9 / 10 {
        for x in width / 10..width * 9 / 10 {
            let value = image.pixels()[y * width + x];
            matching += usize::from(value.abs_diff(expected) <= 12);
            opposite += usize::from(value.abs_diff(other) <= 12);
            sampled += 1;
        }
    }
    assert!(
        matching * 100 >= sampled * 95,
        "wrong surface color: only {matching}/{sampled} pixels match {expected}; {opposite} match the other window"
    );
    assert!(
        opposite * 100 < sampled,
        "capture included the other window's surface"
    );
}

#[test]
#[ignore = "requires an explicitly enabled private headless Hyprland with surface-export protocols"]
fn native_wayland_capture_preserves_window_identity_across_focus_and_close() {
    let private = private_runtime();
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let workspace = Workspace::new(&private, &nonce);
    let mut runtime = Runtime::new();
    let mut a = Fixture::start(format!("zuse-window-capture-A-{nonce}"), DARK);
    let b = Fixture::start(format!("zuse-window-capture-B-{nonce}"), LIGHT);
    let aid = wait_window(&mut runtime, &a, false).id;
    let bid = wait_window(&mut runtime, &b, false).id;
    assert_ne!(aid, bid);
    for id in [&aid, &bid] {
        assert!(u64::from_str_radix(id.strip_prefix("0x").unwrap(), 16).unwrap() > 0);
    }

    focus(&mut runtime, &a, &aid);
    let visible_a = window(&mut runtime, &a).unwrap();
    let first = workspace.file("a-focused.png");
    let receipt = capture(&mut runtime, &aid, &first).expect(
        "a mapped, focused A must be captured successfully; refusing every capture is not a pass",
    );
    assert_owned_image(&receipt, &first, &aid, DARK);
    let original_a = fs::read(&first).unwrap();

    focus(&mut runtime, &b, &bid);
    let bright = workspace.file("b-focused.png");
    let receipt = capture(&mut runtime, &bid, &bright)
        .expect("focused B must capture its own distinct color");
    assert_owned_image(&receipt, &bright, &bid, LIGHT);
    let background_a =
        window(&mut runtime, &a).expect("owned A must remain registered while B is focused");
    assert_eq!(background_a.id, aid);
    assert_eq!(
        background_a.workspace, visible_a.workspace,
        "private fixture workspace unexpectedly changed"
    );
    assert_eq!(
        background_a.scale, visible_a.scale,
        "private monitor configuration must stay fixed"
    );
    // The private runner has one tiling output and disables animations. Only
    // an observed placement/monitor change can justify the offscreen failure
    // branch. Older window.list receipts omit monitor; None == None keeps the
    // stricter success requirement when geometry is unchanged.
    let still_on_screen =
        background_a.geometry == visible_a.geometry && background_a.monitor == visible_a.monitor;
    let background = workspace.file("a-while-b-focused.png");
    fs::write(&background, &original_a).unwrap();
    let started = Instant::now();
    let captured = capture(&mut runtime, &aid, &background);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "background capture exceeded its bounded native deadline"
    );
    match captured {
        Ok(receipt) => assert_owned_image(&receipt, &background, &aid, DARK),
        Err(error) => {
            assert!(
                !still_on_screen,
                "A retained its visible placement; background surface capture must succeed: {error}"
            );
            assert!(
                error.contains("timeout")
                    || error.contains("timed out")
                    || error.contains("unsupported_state")
                    || error.contains("stale_target"),
                "unavailable background surface must report an explicit capture failure: {error}"
            );
            assert_eq!(
                fs::read(&background).unwrap(),
                original_a,
                "failed capture must not publish partial or foreign pixels"
            );
        }
    }
    assert!(
        window(&mut runtime, &b).unwrap().focused,
        "capture A must not steal focus from B"
    );

    a.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    while window(&mut runtime, &a).is_some() {
        assert!(Instant::now() < deadline, "closed A remained registered");
        thread::sleep(Duration::from_millis(30));
    }
    assert!(
        window(&mut runtime, &b).unwrap().mapped,
        "closing A must leave owned B alive"
    );
    let stale = workspace.file("closed-a-existing.png");
    fs::write(&stale, &original_a).unwrap();
    let started = Instant::now();
    let error = capture(&mut runtime, &aid, &stale)
        .expect_err("closed A must not resolve to a different live window");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "stale capture exceeded its bounded native deadline"
    );
    assert!(
        error.contains("stale_target")
            || error.contains("not found")
            || error.contains("unavailable"),
        "closed-window failure must identify a stale/unavailable target: {error}"
    );
    assert_eq!(
        fs::read(stale).unwrap(),
        original_a,
        "stale capture overwrote the existing output"
    );
}
