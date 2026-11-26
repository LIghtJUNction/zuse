#![cfg(all(feature = "std", unix))]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use zuse::json;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
const NAME: &str = "fixture";

fn number(receipt: &str, key: &str) -> u64 {
    json::with_value(receipt, |root| {
        root.get(key)
            .ok_or_else(|| format!("missing {key}"))?
            .raw()
            .parse::<u64>()
            .map_err(|error| error.to_string())
    })
    .unwrap()
}

fn field(receipt: &str, key: &str) -> String {
    json::field(receipt, key).unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn finish(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("session client exceeded its bounded test deadline");
        }
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

struct Fixture {
    root: PathBuf,
    close_on_drop: bool,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zs-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            close_on_drop: true,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zuse"));
        command
            .arg("session")
            .args(args)
            .env("ZUSE_SESSION_DIR", &self.root)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .current_dir(&self.root);
        command
    }

    fn cli(&self, args: &[&str], source: Option<&str>) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(source) = source {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(source.as_bytes())
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        finish(child, Duration::from_secs(8))
    }

    fn start(&self, idle_ms: u64) -> String {
        let ready = success(self.cli(
            &["start", NAME, "--idle-timeout-ms", &idle_ms.to_string()],
            None,
        ));
        assert_eq!(field(&ready, "status"), "started");
        assert_eq!(field(&ready, "state"), "idle");
        assert_eq!(field(&ready, "session"), NAME);
        let socket = PathBuf::from(field(&ready, "socket"));
        let root = self.root.canonicalize().unwrap_or_else(|e| {
            panic!(
                "could not resolve fixture root {}: {e}",
                self.root.display()
            )
        });
        assert!(
            socket.starts_with(&root),
            "reported socket {} is outside fixture root {} (resolved from {})",
            socket.display(),
            root.display(),
            self.root.display()
        );
        assert!(
            fs::symlink_metadata(socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        ready
    }

    fn status(&self) -> String {
        success(self.cli(&["status", NAME], None))
    }

    fn submit(&self, source: &str) -> u64 {
        let receipt = success(self.cli(&["script", NAME, "-"], Some(source)));
        assert_eq!(field(&receipt, "status"), "accepted");
        number(&receipt, "job_id")
    }

    fn result(&self, job: u64) -> String {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let receipt = success(self.cli(&["result", NAME, &job.to_string()], None));
            if field(&receipt, "status") != "running" {
                return receipt;
            }
            assert!(
                Instant::now() < deadline,
                "job {job} did not complete: {receipt}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn serve(&self, idle_ms: u64) -> (OwnedServer, String) {
        let mut child = self
            .command(&["serve", NAME, "--idle-timeout-ms", &idle_ms.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sent, received) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let result = reader.read_line(&mut line).map(|_| line);
            let _ = sent.send(result);
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let server = OwnedServer(child);
        let ready = received
            .recv_timeout(Duration::from_secs(3))
            .expect("serve readiness deadline")
            .unwrap();
        assert_eq!(field(&ready, "status"), "started");
        (server, ready)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.close_on_drop
            && let Ok(mut child) = self
                .command(&["close", NAME])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct OwnedServer(Child);
impl Drop for OwnedServer {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn wait_for_socket_removal(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while socket.exists() {
        assert!(
            Instant::now() < deadline,
            "idle/closed session left its socket behind"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

struct Wire {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Wire {
    fn connect(socket: &Path) -> Self {
        let write = UnixStream::connect(socket).unwrap();
        write
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let read = BufReader::new(write.try_clone().unwrap());
        Self { write, read }
    }

    fn send(&mut self, frame: &str) -> String {
        self.write.write_all(frame.as_bytes()).unwrap();
        self.write.write_all(b"\n").unwrap();
        let mut receipt = String::new();
        assert!(
            self.read.read_line(&mut receipt).unwrap() > 0,
            "server closed a persistent connection"
        );
        assert!(receipt.len() <= 2 * 1024 * 1024);
        assert_eq!(number(&receipt, "version"), 1);
        receipt
    }

    fn wait_job(&mut self, job_id: u64) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        for attempt in 0..200 {
            let receipt = self.send(&format!(
                r#"{{"version":1,"request_id":"poll-{attempt}","op":"result","job_id":{job_id}}}"#
            ));
            if field(&receipt, "status") != "running" {
                return receipt;
            }
            assert!(Instant::now() < deadline, "job {job_id} exceeded deadline");
            thread::sleep(Duration::from_millis(10));
        }
        panic!("job result was never ready");
    }
}

fn assert_wire_ok(receipt: &str) {
    json::with_value(receipt, |root| {
        assert_eq!(root.get("ok").unwrap().raw(), "true", "{receipt}");
        Ok(())
    })
    .unwrap();
}

fn wire_error(receipt: &str) -> String {
    json::with_value(receipt, |root| {
        assert_eq!(root.get("ok").unwrap().raw(), "false", "{receipt}");
        json::text(root.get("error").unwrap().get("code").unwrap())
    })
    .unwrap()
}

fn timer_elapsed(receipt: &str) -> String {
    json::with_value(receipt, |root| {
        let result = root.get("result").unwrap();
        let last = result.get("results").unwrap().elements().last().unwrap();
        Ok(last
            .get("value")
            .unwrap()
            .get("total_elapsed_us")
            .unwrap()
            .raw()
            .to_owned())
    })
    .unwrap()
}

#[test]
fn cli_readiness_and_separate_jobs_retain_runtime_functions() {
    let fixture = Fixture::new();
    fixture.start(5000);
    assert_eq!(field(&fixture.status(), "state"), "idle");
    let first = fixture.submit("fn remembered(){timer.status()}");
    assert_eq!(field(&fixture.result(first), "status"), "completed");
    let second = fixture.submit("remembered();functions()");
    assert!(second > first);
    let result = fixture.result(second);
    assert_eq!(field(&result, "status"), "completed");
    assert!(result.contains("\"name\":\"remembered\""), "{result}");
    assert_eq!(field(&fixture.status(), "state"), "idle");
}

#[test]
fn observation_and_failed_preflight_commit_neither_input_nor_new_functions() {
    let fixture = Fixture::new();
    fixture.start(5000);
    let first = fixture.submit("fn retained(){timer.status()}");
    fixture.result(first);
    // Even a broken read-only guard cannot click: this anchor was never created,
    // and the fixture removes DISPLAY and WAYLAND_DISPLAY from the server.
    let accepted = success(fixture.cli(
        &["observe", NAME, "-"],
        Some("timer.start();mouse.click_at(anchor=\"never-created\",x=1,y=1)"),
    ));
    let rejected = fixture.result(number(&accepted, "job_id"));
    assert_eq!(field(&rejected, "status"), "failed");
    assert!(rejected.contains("read-only"), "{rejected}");
    let bad = fixture.submit("fn pending(){timer.status()};timer.start();unknown_operation()");
    assert_eq!(field(&fixture.result(bad), "status"), "failed");
    let dry = success(fixture.cli(
        &["script", NAME, "-", "--dry-run"],
        Some("fn planned(){timer.start()}planned()"),
    ));
    assert_eq!(
        field(&fixture.result(number(&dry, "job_id")), "status"),
        "completed"
    );
    let inspect = fixture.submit("timer.status();functions()");
    let result = fixture.result(inspect);
    assert!(result.contains("\"running\":false"), "{result}");
    assert!(result.contains("\"name\":\"retained\""), "{result}");
    assert!(!result.contains("\"name\":\"pending\""), "{result}");
    assert!(!result.contains("\"name\":\"planned\""), "{result}");
}

#[test]
fn running_jobs_reject_concurrent_submissions_and_cancel_without_poisoning_next_job() {
    let fixture = Fixture::new();
    fixture.start(5000);
    let job = fixture.submit("wait(ms=6000)");
    assert_eq!(field(&fixture.status(), "state"), "running");
    for op in ["script", "observe"] {
        let output = fixture.cli(&[op, NAME, "-"], Some("timer.status()"));
        assert!(!output.status.success());
        assert!(
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .contains("busy")
        );
    }
    let began = Instant::now();
    let cancelled = success(fixture.cli(&["cancel", NAME], None));
    assert_eq!(field(&cancelled, "status"), "cancelling");
    assert_eq!(field(&cancelled, "state"), "cancelling");
    let result = fixture.result(job);
    assert_eq!(field(&result, "status"), "cancelled");
    assert!(began.elapsed() < Duration::from_secs(2));
    let next = fixture.submit("wait(ms=1);timer.status()");
    assert_eq!(field(&fixture.result(next), "status"), "completed");
}

#[test]
fn close_cancels_and_cleans_up_before_reporting_closed() {
    let mut fixture = Fixture::new();
    let ready = fixture.start(5000);
    let socket = PathBuf::from(field(&ready, "socket"));
    fixture.submit("wait(ms=6000)");
    let began = Instant::now();
    let closed = success(fixture.cli(&["close", NAME], None));
    assert_eq!(field(&closed, "status"), "closed");
    assert!(began.elapsed() < Duration::from_secs(2));
    assert!(
        !socket.exists(),
        "closed must acknowledge cleanup, not just a queued request"
    );
    fixture.close_on_drop = false;
}

#[test]
fn idle_timeout_excludes_running_work_and_ignores_partial_clients() {
    let mut fixture = Fixture::new();
    let ready = fixture.start(150);
    let socket = PathBuf::from(field(&ready, "socket"));
    let job = fixture.submit("wait(ms=500)");
    thread::sleep(Duration::from_millis(260));
    assert!(socket.exists());
    assert_eq!(field(&fixture.status(), "state"), "running");
    assert_eq!(field(&fixture.result(job), "status"), "completed");
    // No controller request is made here: status/result would legitimately renew TTL.
    wait_for_socket_removal(&socket);
    fixture.close_on_drop = false;

    let mut partial_fixture = Fixture::new();
    let ready = partial_fixture.start(150);
    let socket = PathBuf::from(field(&ready, "socket"));
    let mut partial = UnixStream::connect(&socket).unwrap();
    partial.write_all(b"{\"version\":1").unwrap();
    wait_for_socket_removal(&socket);
    partial_fixture.close_on_drop = false;
}

#[test]
fn oversized_source_and_unsafe_session_roots_are_rejected() {
    let fixture = Fixture::new();
    let started = fixture.start(5000);
    let too_large = "#".to_owned() + &"x".repeat(256 * 1024);
    let rejected = fixture.cli(&["script", NAME, "-"], Some(&too_large));
    assert!(!rejected.status.success());
    let ready = fixture.status();
    assert_eq!(field(&ready, "state"), "idle");
    let socket = PathBuf::from(field(&started, "socket"));
    let mut wire = Wire::connect(&socket);
    let large_source = wire.send(&format!(
        r#"{{"version":1,"request_id":"oversized-source","op":"script","source":{}}}"#,
        json::quote(&too_large)
    ));
    assert_eq!(wire_error(&large_source), "invalid_request");
    assert!(large_source.contains("256 KiB"));
    drop(wire);
    let mut wire = Wire::connect(&socket);
    // Send the overlong frame without a newline: the size bound must terminate
    // reading rather than wait for a peer to finish its request.
    let sent = wire.write.write_all(&vec![b' '; 2 * 1024 * 1024 + 1]);
    if let Err(error) = sent {
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ));
    }
    let mut oversized = String::new();
    assert!(wire.read.read_line(&mut oversized).unwrap() > 0);
    assert_eq!(wire_error(&oversized), "frame_too_large");
    assert_eq!(field(&fixture.status(), "state"), "idle");

    let mut wide = Fixture::new();
    fs::set_permissions(&wide.root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!wide.cli(&["start", NAME], None).status.success());
    wide.close_on_drop = false;

    let mut linked = Fixture::new();
    let alias = linked.root.join("alias");
    std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
    let output = linked
        .command(&["start", "other"])
        .env("ZUSE_SESSION_DIR", &alias)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    linked.close_on_drop = false;
    for name in ["../escape", "/absolute", "with/slash"] {
        assert!(!fixture.cli(&["start", name], None).status.success());
    }
}

#[test]
fn startup_never_unlinks_an_unrecognized_foreign_socket() {
    let mut fixture = Fixture::new();
    let ready = fixture.start(5000);
    let socket = PathBuf::from(field(&ready, "socket"));
    success(fixture.cli(&["close", NAME], None));
    let _foreign = UnixListener::bind(&socket).unwrap();
    fixture.close_on_drop = false;
    assert!(!fixture.cli(&["start", NAME], None).status.success());
    assert!(
        fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
}

#[test]
fn shutdown_removes_only_the_socket_owned_by_that_server_instance() {
    let mut fixture = Fixture::new();
    let (mut server, ready) = fixture.serve(200);
    let socket = PathBuf::from(field(&ready, "socket"));
    fs::remove_file(&socket).unwrap();
    let foreign = UnixListener::bind(&socket).unwrap();
    fixture.close_on_drop = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "original server did not expire");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        socket.exists(),
        "cleanup removed another listener's replacement socket"
    );
    let _client = UnixStream::connect(&socket).unwrap();
    foreign.set_nonblocking(true).unwrap();
    assert!(foreign.accept().is_ok());
}

#[test]
fn raw_external_client_reuses_connection_and_request_id_without_repeating_work() {
    let fixture = Fixture::new();
    let ready = fixture.start(5000);
    let mut wire = Wire::connect(Path::new(&field(&ready, "socket")));
    let status = wire.send(r#"{"version":1,"request_id":"inspect","op":"status"}"#);
    assert_wire_ok(&status);
    // A controller may keep its connection while another controller reads status.
    assert_eq!(field(&fixture.status(), "state"), "idle");
    let revision = number(&status, "state_revision");
    let frame = format!(
        r#"{{"version":1,"request_id":"run-once","op":"script","expected_revision":{revision},"source":"timer.reset();wait(ms=120);timer.stop()"}}"#
    );
    let accepted = wire.send(&frame);
    assert_wire_ok(&accepted);
    let job = number(&accepted, "job_id");
    let replay = wire.send(&frame);
    assert_wire_ok(&replay);
    assert_eq!(number(&replay, "job_id"), job);
    assert!(replay.contains("\"replayed\":true"));
    let result = wire.wait_job(job);
    assert_eq!(field(&result, "status"), "completed");
    let elapsed = timer_elapsed(&result);
    let replay_after_completion = wire.send(&frame);
    assert_wire_ok(&replay_after_completion);
    assert_eq!(number(&replay_after_completion, "job_id"), job);
    assert!(replay_after_completion.contains("\"replayed\":true"));
    let status = wire.send(r#"{"version":1,"request_id":"after-replay","op":"status"}"#);
    assert_eq!(number(&status, "last_job_id"), job);
    assert_eq!(field(&status, "state"), "idle");
    let observed = wire.send(
        r#"{"version":1,"request_id":"timer-check","op":"observe","source":"timer.status()"}"#,
    );
    assert_wire_ok(&observed);
    let observed = wire.wait_job(number(&observed, "job_id"));
    assert_eq!(
        timer_elapsed(&observed),
        elapsed,
        "replay reset or reran the stopped timer"
    );
}

#[test]
fn raw_protocol_rejects_stale_revisions_conflicting_ids_and_bad_versions() {
    let fixture = Fixture::new();
    let ready = fixture.start(5000);
    let mut wire = Wire::connect(Path::new(&field(&ready, "socket")));
    let before = wire.send(r#"{"version":1,"request_id":"before","op":"status"}"#);
    let revision = number(&before, "state_revision");
    let again = wire.send(r#"{"version":1,"request_id":"read-again","op":"status"}"#);
    assert_eq!(number(&again, "state_revision"), revision);
    let accepted = wire.send(&format!(r#"{{"version":1,"request_id":"unique","op":"script","expected_revision":{revision},"source":"wait(ms=1)"}}"#));
    assert_wire_ok(&accepted);
    wire.wait_job(number(&accepted, "job_id"));
    let after = wire.send(r#"{"version":1,"request_id":"after","op":"status"}"#);
    assert!(number(&after, "state_revision") > revision);
    let stale = wire.send(&format!(r#"{{"version":1,"request_id":"old-view","op":"script","expected_revision":{revision},"source":"timer.start()"}}"#));
    assert_eq!(wire_error(&stale), "stale_revision");
    let conflict =
        wire.send(r#"{"version":1,"request_id":"unique","op":"script","source":"timer.start()"}"#);
    assert_eq!(wire_error(&conflict), "request_id_conflict");
    let unsupported = wire.send(r#"{"version":99,"request_id":"future","op":"status"}"#);
    assert_eq!(wire_error(&unsupported), "unsupported_version");
    let invalid = wire.send(r#"{"version":1,"request_id":"unknown","op":"not-an-operation"}"#);
    assert_eq!(wire_error(&invalid), "invalid_request");
    let status = wire.send(r#"{"version":1,"request_id":"unchanged","op":"status"}"#);
    assert_eq!(
        number(&status, "state_revision"),
        number(&after, "state_revision")
    );
}

#[test]
fn external_observe_cannot_disable_read_only_or_bypass_whole_program_budgets() {
    let fixture = Fixture::new();
    let ready = fixture.start(5000);
    let mut wire = Wire::connect(Path::new(&field(&ready, "socket")));
    let observed = wire.send(r#"{"version":1,"request_id":"forced-read-only","op":"observe","source":"timer.start();mouse.click_at(anchor='never-created',x=1,y=1)","options":{"read_only":false}}"#);
    assert_wire_ok(&observed);
    let result = wire.wait_job(number(&observed, "job_id"));
    assert_eq!(field(&result, "status"), "failed");
    assert!(result.contains("read-only"));
    let budget = wire.send(r#"{"version":1,"request_id":"small-budget","op":"script","source":"timer.start();wait(ms=2)","options":{"max_wait_ms":1}}"#);
    assert_wire_ok(&budget);
    assert_eq!(
        field(&wire.wait_job(number(&budget, "job_id")), "status"),
        "failed"
    );
    let timer = wire.send(
        r#"{"version":1,"request_id":"still-stopped","op":"observe","source":"timer.status()"}"#,
    );
    let result = wire.wait_job(number(&timer, "job_id"));
    assert_eq!(field(&result, "status"), "completed");
    assert!(result.contains("\"running\":false"), "{result}");
}
