#![cfg(feature = "std")]

use std::{
    io::Write,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use zuse::json;

fn shell(timeout_ms: u64) -> Child {
    Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(["shell", "--idle-timeout-ms", &timeout_ms.to_string()])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn finish(mut child: Child) -> Output {
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(3) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("shell did not exit before the test deadline");
        }
        // Keep stdin open until the process exits, so EOF cannot pass an idle test.
        thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

fn write(child: &mut Child, source: &str) {
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
}

fn assert_idle_exit(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("idle timeout"));
}

#[test]
fn idle_shell_exits_with_stdin_open_even_with_an_incomplete_line() {
    for partial in ["", "timer.status("] {
        let mut child = shell(50);
        write(&mut child, partial);
        let output = finish(child);
        assert_idle_exit(&output);
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn successive_commands_refresh_the_shell_idle_timeout() {
    let mut child = shell(250);
    for index in 0..5 {
        if index > 0 {
            thread::sleep(Duration::from_millis(80));
        }
        write(&mut child, "timer.status()\n");
    }
    let output = finish(child);
    assert_idle_exit(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 5, "{stdout}");
    for receipt in stdout.lines() {
        assert_eq!(json::field(receipt, "status").unwrap(), "completed");
    }
}

#[test]
fn running_commands_and_queued_input_are_not_idle() {
    let mut child = shell(40);
    let began = Instant::now();
    write(&mut child, "wait(ms=180)\ntimer.status()\n");
    let output = finish(child);
    assert_idle_exit(&output);
    assert!(began.elapsed() >= Duration::from_millis(180));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
    for receipt in stdout.lines() {
        assert_eq!(json::field(receipt, "status").unwrap(), "completed");
    }
}

#[test]
fn explicit_exit_does_not_wait_for_idle_or_execute_queued_commands() {
    let mut child = shell(1000);
    write(&mut child, ":exit\ntimer.status()\n");
    let output = finish(child);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("idle timeout"));
}

#[test]
fn idle_timeout_requires_a_positive_shell_only_value() {
    for (args, expected) in [
        (vec!["shell", "--idle-timeout-ms", "0"], "positive integer"),
        (
            vec!["shell", "--idle-timeout-ms", "invalid"],
            "positive integer",
        ),
        (
            vec!["-c", "timer.status()", "--idle-timeout-ms", "10"],
            "only valid for zuse shell",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_zuse"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
    }
}
