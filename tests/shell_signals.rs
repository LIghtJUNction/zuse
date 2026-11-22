#![cfg(all(feature = "std", unix))]

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zuse::{json, script::interrupt::InterruptGuard};

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

struct Shell {
    child: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    managed: Vec<i32>,
}

impl Shell {
    fn new() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zuse"))
            .args(["shell", "--idle-timeout-ms", "30000"])
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
            managed: Vec::new(),
        }
    }

    fn write(&mut self, source: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(source.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn start_process(&mut self, program: &str, args: &str) -> u64 {
        self.write(&format!(
            "process.start(program={},args={})\n",
            json::quote(program),
            json::quote(args)
        ));
        let receipt = self.lines.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(json::field(&receipt, "status").unwrap(), "completed");
        let (id, pid) = json::with_value(&receipt, |root| {
            let process = root
                .get("results")
                .unwrap()
                .elements()
                .next()
                .unwrap()
                .get("value")
                .unwrap();
            Ok((
                process.get("id").unwrap().integer().unwrap() as u64,
                process.get("pid").unwrap().integer().unwrap() as i32,
            ))
        })
        .unwrap();
        self.managed.push(pid);
        id
    }

    fn signal(&self, signal: i32) {
        // Only the CLI process created by this fixture receives the signal.
        assert_eq!(unsafe { kill(self.child.id() as i32, signal) }, 0);
    }

    fn finish(&mut self) -> (ExitStatus, Vec<String>, String) {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "interrupted shell did not exit with stdin still open"
            );
            thread::sleep(Duration::from_millis(5));
        };
        self.reader.take().unwrap().join().unwrap();
        let receipts = self.lines.try_iter().collect();
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (status, receipts, stderr)
    }

    fn assert_managed_processes_reaped(&mut self) {
        for &pid in &self.managed {
            // A reaped child has no process ID; a zombie would still exist.
            assert_eq!(unsafe { kill(pid, 0) }, -1, "managed child {pid} survived");
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(3));
        }
        self.managed.clear();
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Keep a failing regression from leaving the fixture's own process group.
        for &pid in &self.managed {
            unsafe { kill(-pid, 9) };
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn signals_close_idle_shell_and_reap_managed_processes() {
    if !InterruptGuard::is_supported() {
        return;
    }
    for signal in [2, 15] {
        for partial in ["", "timer.status("] {
            let mut shell = Shell::new();
            shell.start_process("/bin/sleep", "60");
            shell.write(partial);
            shell.signal(signal);
            let (status, receipts, stderr) = shell.finish();
            assert_eq!(status.code(), Some(1), "{stderr}");
            assert!(receipts.is_empty(), "{receipts:?}");
            assert!(stderr.contains("shell interrupted"), "{stderr}");
            shell.assert_managed_processes_reaped();
        }
    }
}

#[test]
fn signals_cancel_running_commands_and_discard_queued_input() {
    if !InterruptGuard::is_supported() {
        return;
    }
    for signal in [2, 15] {
        let mut shell = Shell::new();
        // The managed child waits for this shell's current command before
        // signalling its parent. No signal targets the test runner.
        let id = shell.start_process(
            "/bin/sh",
            &format!("-c 'read line; kill -{signal} \"$PPID\"; exec sleep 60'"),
        );
        shell.write(&format!(
            "process.write(id={id},text={}); wait(ms=30000)\ntimer.status()\n",
            json::quote("go\n")
        ));
        let (status, receipts, stderr) = shell.finish();
        assert_eq!(status.code(), Some(1), "{stderr}");
        assert_eq!(receipts.len(), 1, "{receipts:?}");
        assert!(
            receipts[0].contains("\"error_code\":\"cancelled\""),
            "{receipts:?}"
        );
        assert!(stderr.contains("shell interrupted"), "{stderr}");
        shell.assert_managed_processes_reaped();
    }
}
