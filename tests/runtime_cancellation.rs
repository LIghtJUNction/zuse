#![cfg(feature = "std")]
use std::{
    thread,
    time::{Duration, Instant},
};
use zuse::{
    observation::Cancellation,
    runtime::{Options, Runtime},
};

#[test]
fn cancelling_a_wait_preserves_progress_and_stops_remaining_commands() {
    let mut runtime = Runtime::new();
    let token = runtime.cancellation();
    let external = token.clone();
    let stop = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        external.cancel();
    });
    let began = Instant::now();
    let result = runtime
        .execute(
            "timer.start(); wait(ms=30000); timer.reset()",
            Options::default(),
        )
        .unwrap();
    stop.join().unwrap();
    assert!(began.elapsed() < Duration::from_secs(1));
    assert!(result.contains("\"error_code\":\"cancelled\""), "{result}");
    assert!(result.contains("\"completed\":1"), "{result}");
    assert!(result.contains("\"timing\""));
    let stopped = runtime
        .execute("timer.status()", Options::default())
        .unwrap();
    assert!(stopped.contains("\"completed\":0"));
    runtime.set_cancellation(Cancellation::default());
    let resumed = runtime
        .execute("timer.status()", Options::default())
        .unwrap();
    assert!(resumed.contains("\"status\":\"completed\""));
}

#[cfg(unix)]
#[test]
fn cli_sigint_wakes_wait_and_returns_a_cancelled_receipt() {
    use std::process::{Command, Stdio};
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let child = Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(["eval", "timer.start(); wait(ms=30000); timer.reset()"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    let began = Instant::now();
    assert_eq!(unsafe { kill(child.id() as i32, 2) }, 0);
    let output = child.wait_with_output().unwrap();
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(output.status.code(), Some(1));
    let receipt = String::from_utf8(output.stdout).unwrap();
    assert!(
        receipt.contains("\"error_code\":\"cancelled\""),
        "{receipt}"
    );
    assert!(receipt.contains("\"completed\":1"));
}
