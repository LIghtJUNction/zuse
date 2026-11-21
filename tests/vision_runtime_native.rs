#![cfg(all(feature = "std", target_os = "linux"))]
//! Run only through tests/run_vision_native.py on its authenticated, owned Xvfb.
#[cfg(feature = "vision")]
use std::path::Path;
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zuse::{json, platform::Desktop, target::TargetWindow};

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

fn private_workspace() -> PathBuf {
    assert_eq!(
        std::env::var("ZUSE_PRIVATE_X11_FIXTURE").as_deref(),
        Ok("1"),
        "run tests/run_vision_native.py; never point this test at a desktop"
    );
    assert_eq!(std::env::var("ZUSE_BACKEND").as_deref(), Ok("x11"));
    assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
    let display = std::env::var("DISPLAY").unwrap();
    let number: u32 = display.strip_prefix(':').unwrap().parse().unwrap();
    assert!((10000..30000).contains(&number));
    let root = PathBuf::from(std::env::var_os("ZUSE_VISION_NATIVE_WORKSPACE").unwrap())
        .canonicalize()
        .unwrap();
    let authority = PathBuf::from(std::env::var_os("XAUTHORITY").unwrap())
        .canonicalize()
        .unwrap();
    assert!(
        authority.starts_with(&root),
        "Xauthority must belong to this runner"
    );
    root
}

fn raw(input: &str, path: &[&str]) -> String {
    json::with_value(input, |mut value| {
        for key in path {
            value = value
                .get(key)
                .ok_or_else(|| format!("missing {key}: {input}"))?;
        }
        Ok(value.raw().to_owned())
    })
    .unwrap()
}
fn text(input: &str, path: &[&str]) -> String {
    json::with_value(&raw(input, path), json::text).unwrap()
}
fn integer(input: &str, path: &[&str]) -> u64 {
    raw(input, path).parse().unwrap()
}
fn boolean(input: &str, path: &[&str]) -> bool {
    raw(input, path).parse().unwrap()
}
#[cfg(feature = "vision")]
fn real(input: &str, path: &[&str]) -> f64 {
    raw(input, path).parse().unwrap()
}
#[cfg(feature = "vision")]
fn elements(input: &str, path: &[&str]) -> Vec<String> {
    json::with_value(&raw(input, path), |value| {
        Ok(value.elements().map(|v| v.raw().to_owned()).collect())
    })
    .unwrap()
}
fn last_value(input: &str) -> String {
    json::with_value(input, |value| {
        value
            .get("results")
            .and_then(|v| v.elements().last())
            .and_then(|v| v.get("value"))
            .map(|v| v.raw().to_owned())
            .ok_or_else(|| format!("missing CLI result: {input}"))
    })
    .unwrap()
}

struct Peer {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Result<String, String>>,
    reader: Option<JoinHandle<()>>,
    log: PathBuf,
    stop: &'static str,
}
impl Peer {
    fn spawn(mut command: Command, log: PathBuf, stop: &'static str) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.map_err(|error| error.to_string())).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output,
            reader: Some(reader),
            log,
            stop,
        }
    }
    fn send(&mut self, code: &str) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{code}").unwrap();
        input.flush().unwrap();
    }
    fn receive(&mut self) -> String {
        self.output
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|error| {
                panic!(
                    "child response: {error}; status={:?}; stderr={}",
                    self.child.try_wait(),
                    fs::read_to_string(&self.log).unwrap_or_default()
                )
            })
            .unwrap()
    }
    fn request(&mut self, code: &str) -> String {
        self.send(code);
        self.receive()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            if let Some(mut input) = self.input.take() {
                let _ = writeln!(input, "{}", self.stop);
                let _ = input.flush();
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct Fixture {
    peer: Peer,
    directory: PathBuf,
    window: String,
    #[cfg(feature = "vision")]
    template: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = private_workspace().join(format!(
            "case-{}-{}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let template = directory.join("template-20.png");
        let mut command = Command::new(
            std::env::var_os("ZUSE_NATIVE_VISION_FIXTURE")
                .expect("runner must compile the GTK fixture"),
        );
        command.arg(&template);
        let mut peer = Peer::spawn(command, directory.join("fixture.log"), "quit");
        let ready = peer.receive();
        assert_eq!(text(&ready, &["reply"]), "ready", "{ready}");
        assert_eq!(integer(&ready, &["pid"]), u64::from(peer.child.id()));
        assert_eq!(
            (
                integer(&ready, &["x"]),
                integer(&ready, &["y"]),
                integer(&ready, &["width"]),
                integer(&ready, &["height"])
            ),
            (80, 70, 320, 240)
        );
        assert!(template.is_file());
        Self {
            window: text(&ready, &["window"]),
            peer,
            directory,
            #[cfg(feature = "vision")]
            template,
        }
    }
    fn command(&mut self, command: &str) -> String {
        let reply = self.peer.request(command);
        assert_eq!(text(&reply, &["reply"]), "ok", "command={command}; {reply}");
        reply
    }
    fn desktop(&self) -> Desktop {
        let mut desktop = Desktop::connect().unwrap();
        assert_eq!(desktop.backend(), "x11");
        desktop.focus(&self.window).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let target =
                TargetWindow::from_windows(&desktop.windows().unwrap(), &self.window).unwrap();
            if target.focused {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned window did not receive focus"
            );
            thread::sleep(Duration::from_millis(5));
        }
        desktop
    }
    #[cfg(feature = "vision")]
    fn wait_clicks(&mut self, expected: u64) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = self.command("state");
            let observed = integer(&state, &["clicks"]);
            assert!(observed <= expected, "duplicate click: {state}");
            if observed == expected {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "application did not receive click: {state}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

struct Shell {
    peer: Peer,
}
impl Shell {
    fn new(fixture: &Fixture) -> Self {
        let binary = std::env::var_os("ZUSE_NATIVE_ZUSE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_zuse")));
        let mut command = Command::new(binary);
        command.arg("shell");
        Self {
            peer: Peer::spawn(command, fixture.directory.join("cli.log"), ":quit"),
        }
    }
    fn completed(&mut self, code: &str) -> String {
        let response = self.peer.request(code);
        assert_eq!(
            text(&response, &["status"]),
            "completed",
            "{code}: {response}"
        );
        last_value(&response)
    }
    #[cfg(feature = "vision")]
    fn stale(&mut self, code: &str) -> String {
        let response = self.peer.request(code);
        assert_eq!(text(&response, &["status"]), "failed", "{response}");
        assert_eq!(
            text(&response, &["error_code"]),
            "stale_target",
            "{response}"
        );
        assert_eq!(integer(&response, &["completed"]), 0, "{response}");
        response
    }
}

#[test]
#[ignore = "requires run_vision_native.py and its owned authenticated Xvfb"]
fn native_x11_window_hit_test_respects_invisible_input_cover_without_moving_pointer_or_focus() {
    let mut fixture = Fixture::new();
    let desktop = fixture.desktop();
    let before_pointer = desktop.position().unwrap();
    let before_window =
        TargetWindow::from_windows(&desktop.windows().unwrap(), &fixture.window).unwrap();
    assert!(desktop.window_hit_test(&fixture.window, 133, 133).unwrap());
    assert!(!desktop.window_hit_test(&fixture.window, 5, 5).unwrap());
    fixture.command("cover");
    assert!(
        !desktop.window_hit_test(&fixture.window, 133, 133).unwrap(),
        "InputOnly cover must block a click"
    );
    assert!(
        desktop.window_hit_test(&fixture.window, 250, 220).unwrap(),
        "uncovered target area remains reachable"
    );
    assert_eq!(desktop.position().unwrap(), before_pointer);
    assert_eq!(
        TargetWindow::from_windows(&desktop.windows().unwrap(), &fixture.window).unwrap(),
        before_window
    );
    fixture.command("uncover");
    assert!(desktop.window_hit_test(&fixture.window, 133, 133).unwrap());
    let framed = fixture.command("frame");
    assert_ne!(text(&framed, &["frame"]), "0x0");
    assert!(desktop.window_hit_test(&fixture.window, 133, 133).unwrap());
    assert!(
        !desktop.window_hit_test(&fixture.window, 75, 65).unwrap(),
        "frame decoration is outside the client"
    );
    fixture.command("cover");
    assert!(
        !desktop.window_hit_test(&fixture.window, 133, 133).unwrap(),
        "a sibling InputOnly cover must block input even when the root child is the target's frame"
    );
    assert_eq!(desktop.position().unwrap(), before_pointer);
    assert_eq!(
        TargetWindow::from_windows(&desktop.windows().unwrap(), &fixture.window).unwrap(),
        before_window
    );
    fixture.command("uncover");
    assert!(desktop.window_hit_test(&fixture.window, 133, 133).unwrap());
    fixture.command("unframe");
}

#[test]
#[ignore = "requires run_vision_native.py and its owned authenticated Xvfb"]
fn native_x11_num_lock_cli_changes_only_the_requested_bit_and_reads_back() {
    let mut fixture = Fixture::new();
    let desktop = fixture.desktop();
    let before = fixture.command("caps 1");
    let num = integer(&before, &["num_mask"]);
    let caps = integer(&before, &["caps_mask"]);
    assert_ne!(num, 0);
    assert_ne!(caps, 0);
    assert_eq!(num & caps, 0);
    let other_locks = integer(&before, &["locked_mods"]) & !num;
    assert_ne!(
        other_locks & caps,
        0,
        "Caps Lock must be on to detect collateral changes"
    );
    let mut shell = Shell::new(&fixture);
    for (action, expected) in [
        ("on", true),
        ("on", true),
        ("off", false),
        ("toggle", true),
        ("off", false),
    ] {
        let result = shell.completed(&format!("keyboard.num_lock(state={})", json::quote(action)));
        assert_eq!(boolean(&result, &["current", "numLock"]), expected);
        assert!(boolean(&result, &["current", "capsLock"]));
        let state = fixture.command("state");
        assert_eq!(
            integer(&state, &["locked_mods"]) & !num,
            other_locks,
            "action={action}; {state}"
        );
        assert_eq!(integer(&state, &["locked_mods"]) & num != 0, expected);
        assert_eq!(integer(&state, &["group"]), integer(&before, &["group"]));
        assert_eq!(integer(&state, &["base_mods"]), 0);
        assert_eq!(integer(&state, &["latched_mods"]), 0);
        let readback = desktop.num_lock("read").unwrap();
        assert_eq!(boolean(&readback, &["current", "numLock"]), expected);
        assert!(boolean(&readback, &["current", "capsLock"]));
        assert_eq!(text(&readback, &["scope"]), "core_keyboard");
        assert!(!boolean(&readback, &["other_devices_observed"]));
        assert!(!boolean(
            &shell.completed("keyboard.state()"),
            &["keys_held"]
        ));
    }
    // Fixture shutdown restores the initial full lock mask on this private server.
}

#[cfg(feature = "vision")]
fn locate(shell: &mut Shell, fixture: &Fixture) -> String {
    shell.completed(&format!(
        "target.locate(name=\"icon\",window={},template={},mode=\"edges\",scales=\"1,1.25,1.5\")",
        json::quote(&fixture.window),
        json::quote(&fixture.template.to_string_lossy())
    ))
}

#[cfg(feature = "vision")]
#[test]
#[ignore = "requires the vision feature and run_vision_native.py"]
fn native_cli_scaled_template_rejects_ambiguity_geometry_changes_and_input_cover() {
    let mut fixture = Fixture::new();
    let desktop = fixture.desktop();
    let mut shell = Shell::new(&fixture);
    shell.completed("mouse.use(kind=\"user\",appearance=\"system\")");
    let found = locate(&mut shell, &fixture);
    assert_eq!(text(&found, &["status"]), "unique", "{found}");
    assert_eq!(text(&found, &["coordinate_space"]), "native_desktop");
    assert_eq!(integer(&found, &["recognition_passes"]), 1);
    assert_eq!(integer(&found, &["images_returned"]), 0);
    let candidates = elements(&found, &["candidates"]);
    assert_eq!(candidates.len(), 1);
    let best = &candidates[0];
    assert_eq!(real(best, &["template_scale"]), 1.25);
    assert_eq!(
        (
            integer(best, &["image_bounds", "x"]),
            integer(best, &["image_bounds", "y"]),
            integer(best, &["image_bounds", "width"]),
            integer(best, &["image_bounds", "height"])
        ),
        (40, 50, 25, 25)
    );
    assert_eq!((real(best, &["x"]), real(best, &["y"])), (132.5, 132.5));
    shell.completed("mouse.click(target=\"icon\")");
    let clicked = fixture.wait_clicks(1);
    assert!((real(&clicked, &["click_x"]) - 52.5).abs() <= 1.0);
    assert!((real(&clicked, &["click_y"]) - 62.5).abs() <= 1.0);

    fixture.command("duplicate");
    let ambiguous = locate(&mut shell, &fixture);
    assert_eq!(text(&ambiguous, &["status"]), "ambiguous", "{ambiguous}");
    assert_eq!(elements(&ambiguous, &["candidates"]).len(), 2);
    shell.stale("mouse.click(target=\"icon\")");
    assert_eq!(integer(&fixture.command("state"), &["clicks"]), 1);

    fixture.command("single");
    assert_eq!(text(&locate(&mut shell, &fixture), &["status"]), "unique");
    fixture.command("geometry 105 85 320 240");
    let moved = shell.stale("mouse.click(target=\"icon\")");
    assert!(text(&moved, &["error"]).contains("moved"), "{moved}");
    assert_eq!(integer(&fixture.command("state"), &["clicks"]), 1);

    fixture.command("geometry 80 70 320 240");
    assert_eq!(text(&locate(&mut shell, &fixture), &["status"]), "unique");
    fixture.command("geometry 80 70 360 260");
    shell.stale("mouse.click(target=\"icon\")");
    assert_eq!(integer(&fixture.command("state"), &["clicks"]), 1);

    fixture.command("geometry 80 70 320 240");
    assert_eq!(text(&locate(&mut shell, &fixture), &["status"]), "unique");
    fixture.command("frame");
    let before = fixture.directory.join("before-cover.png");
    let after = fixture.directory.join("after-cover.png");
    desktop.screenshot(&before, Some(&fixture.window)).unwrap();
    fixture.command("cover");
    desktop.screenshot(&after, Some(&fixture.window)).unwrap();
    let before = zuse::vision::GrayImage::load_png(&before).unwrap();
    let after = zuse::vision::GrayImage::load_png(&after).unwrap();
    assert_eq!(
        before.pixels(),
        after.pixels(),
        "input-only cover must leave recognition pixels unchanged"
    );
    let covered = shell.stale("mouse.click(target=\"icon\")");
    assert!(
        text(&covered, &["error"]).contains("input surface"),
        "{covered}"
    );
    let state = fixture.command("state");
    assert_eq!(integer(&state, &["clicks"]), 1);
    assert_eq!(
        integer(&state, &["covered_clicks"]),
        0,
        "no click may reach the covering surface"
    );
    fixture.command("uncover");
    shell.completed("mouse.click(target=\"icon\")");
    fixture.wait_clicks(2);
    shell.completed("mouse.release()");
    fixture.command("unframe");
}

#[cfg(feature = "vision")]
fn assert_no_svg(directory: &Path) {
    assert!(!fs::read_dir(directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "svg")
    }));
}

#[cfg(feature = "vision")]
#[test]
#[ignore = "requires the vision feature and run_vision_native.py"]
fn native_cli_timeline_returns_first_appearance_then_keeps_two_episodes_until_explicit_svg_render()
{
    let mut fixture = Fixture::new();
    let _desktop = fixture.desktop();
    fixture.command("blank");
    let mut shell = Shell::new(&fixture);
    let started = shell.completed(&format!(
        "timeline.start(name=\"moles\",window={},columns=4,rows=3)",
        json::quote(&fixture.window)
    ));
    assert_eq!(integer(&started, &["captured_frames"]), 1);
    assert_eq!(integer(&started, &["timeline", "total_episodes"]), 0);
    assert_eq!(integer(&started, &["images_returned"]), 0);
    assert_no_svg(&fixture.directory);

    shell
        .peer
        .send("timeline.wait_until(name=\"moles\",timeout=3000,interval=16)");
    let visible = fixture.command("show");
    assert!(boolean(&visible, &["visible"]));
    let observed = Instant::now();
    let response = shell.peer.receive();
    let appearance_latency = observed.elapsed();
    assert_eq!(text(&response, &["status"]), "completed", "{response}");
    let first = last_value(&response);
    assert_eq!(text(&first, &["status"]), "satisfied");
    assert!(
        appearance_latency < Duration::from_secs(1),
        "first event waited {appearance_latency:?}"
    );
    let events = elements(&first, &["events", "events"]);
    assert_eq!(events.len(), 1);
    assert_eq!(text(&events[0], &["event"]), "appeared");
    assert_eq!(
        (
            integer(&events[0], &["episode", "column"]),
            integer(&events[0], &["episode", "row"])
        ),
        (2, 1)
    );
    assert!(boolean(&events[0], &["episode", "active"]));
    assert!(
        boolean(&fixture.command("state"), &["visible"]),
        "wait must finish before disappearance"
    );
    assert_eq!(integer(&first, &["state", "images_returned"]), 0);
    assert_no_svg(&fixture.directory);

    for _ in 0..11 {
        let sample = shell.completed("timeline.sample(name=\"moles\")");
        assert!(elements(&sample, &["events", "events"]).is_empty());
        assert_eq!(
            integer(&sample, &["state", "timeline", "total_episodes"]),
            1
        );
        assert_eq!(integer(&sample, &["state", "images_returned"]), 0);
    }
    let continuous = shell.completed("timeline.observe(name=\"moles\")");
    let episodes = elements(&continuous, &["timeline", "episodes"]);
    assert_eq!(episodes.len(), 1);
    assert_eq!(
        integer(&episodes[0], &["visible_frames"]),
        12,
        "frames must not become episodes"
    );

    fixture.command("hide");
    let ended = shell.completed("timeline.sample(name=\"moles\")");
    let events = elements(&ended, &["events", "events"]);
    assert_eq!(events.len(), 1);
    assert_eq!(text(&events[0], &["event"]), "disappeared");
    fixture.command("show");
    let second = shell.completed("timeline.wait_until(name=\"moles\",timeout=3000,interval=16)");
    let events = elements(&second, &["events", "events"]);
    assert_eq!(events.len(), 1);
    assert_eq!(text(&events[0], &["event"]), "appeared");
    assert_eq!(integer(&events[0], &["episode", "id"]), 2);
    fixture.command("hide");
    shell.completed("timeline.sample(name=\"moles\")");
    let final_state = shell.completed("timeline.observe(name=\"moles\")");
    let episodes = elements(&final_state, &["timeline", "episodes"]);
    assert_eq!(integer(&final_state, &["timeline", "total_episodes"]), 2);
    assert_eq!(episodes.len(), 2);
    assert_eq!(
        integer(&episodes[0], &["cell"]),
        integer(&episodes[1], &["cell"])
    );
    assert!(integer(&episodes[1], &["appeared_ms"]) > integer(&episodes[0], &["appeared_ms"]));
    for episode in &episodes {
        assert!(!boolean(episode, &["active"]));
        assert!(integer(episode, &["duration_ms"]) > 0);
    }
    assert_no_svg(&fixture.directory);
    fs::write(fixture.directory.join("timeline.json"), &final_state).unwrap();
    let path = fixture.directory.join("timeline.svg");
    let rendered = shell.completed(&format!(
        "timeline.render(name=\"moles\",path={})",
        json::quote(&path.to_string_lossy())
    ));
    assert_eq!(integer(&rendered, &["episodes"]), 2);
    assert_eq!(integer(&rendered, &["background_frames"]), 1);
    let svg = fs::read_to_string(&path).unwrap();
    assert!(svg.starts_with("<svg"));
    assert_eq!(svg.matches("data:image/png;base64,").count(), 1);
    assert!(svg.contains("①") && svg.contains("②"));
    for episode in &episodes {
        assert!(svg.contains(&format!("+{}ms", integer(episode, &["appeared_ms"]))));
    }
    let after = shell.completed("timeline.observe(name=\"moles\")");
    assert_eq!(
        integer(&after, &["captured_frames"]),
        integer(&final_state, &["captured_frames"]),
        "render must reuse retained data"
    );
    shell.completed("timeline.close(name=\"moles\")");
    println!(
        "first appearance to wait response: {} us; 12 visible frames -> 1 episode; reappearance -> 2",
        appearance_latency.as_micros()
    );
}
