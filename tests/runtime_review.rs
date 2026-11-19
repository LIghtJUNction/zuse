#![cfg(feature = "std")]
use zuse::{
    human::{Queue, Request},
    json,
    runtime::{self, Options},
};
fn directory() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "zuse-review-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
fn request_id(record: &str) -> String {
    json::with_value(record, |v| {
        json::text(v.get("request").unwrap().get("id").unwrap())
    })
    .unwrap()
}
#[test]
fn platform_argument_errors_fail_preflight() {
    for source in [
        r#"keyboard.type(text="\u0000")"#,
        r#"keyboard.key(key="Ctrl++a")"#,
        r#"mouse.scroll(dy=1001)"#,
        r#"android.swipe(x1=0,y1=0,x2=1,y2=1,duration=10001)"#,
        r#"human.request(kind="password",message="Enter locally",window="active")"#,
        r#"process.start(program="echo",args="'unterminated")"#,
        r#"harness.start(name="unknown-harness")"#,
        r#"browser.viewport(width=0,height=800)"#,
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    assert!(
        runtime::check(
            r#"mouse.click(button="1");mouse.click(button="back")"#,
            Options::default()
        )
        .is_ok()
    );
    assert!(
        runtime::check(
            r#"browser.viewport(width=1280,height=720)"#,
            Options::default()
        )
        .is_ok()
    );
}
#[test]
fn android_swipes_spend_the_wait_budget() {
    let source = "android.swipe(x1=0,y1=0,x2=1,y2=1,duration=1000)";
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 999,
                ..Options::default()
            }
        )
        .is_err()
    );
}
#[test]
fn handoff_preserves_execution_limits_and_read_only() {
    let path = directory();
    let q = Queue::at(path.join("human")).unwrap();
    let record = q
        .request_with_options(
            Request {
                kind: "approval",
                message: "Continue",
                url: "",
                window: "",
                timeout_secs: 60,
            },
            r#"keyboard.type(text="example-private-value")"#,
            false,
            Options {
                read_only: true,
                max_steps: 1,
                max_wait_ms: 5,
                ..Options::default()
            },
        )
        .unwrap();
    let id = request_id(&record);
    let saved = q.options(&id).unwrap();
    assert!(saved.read_only);
    assert_eq!(saved.max_steps, 1);
    assert_eq!(saved.max_wait_ms, 5);
    q.respond(&id, true).unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(["resume", &id])
        .env("ZUSE_STATE_DIR", &path)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        q.status(&id).unwrap(),
        "approved",
        "invalid continuation must not consume approval"
    );
    assert!(!String::from_utf8_lossy(&result.stdout).contains("example-private-value"));
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn dry_run_results_omit_text_payloads() {
    let result = runtime::check(
        r#"keyboard.type(text="example-private-value")"#,
        Options::default(),
    )
    .unwrap();
    assert!(!result.contains("example-private-value"));
}

#[cfg(target_os = "linux")]
#[test]
fn password_target_is_checked_before_opening_a_prompt() {
    use std::os::unix::fs::PermissionsExt;
    let path = directory();
    let q = Queue::at(path.join("human")).unwrap();
    let bin = path.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = path.join("prompt-started");
    let helper = bin.join("zenity");
    std::fs::write(
        &helper,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let record = q
        .request(
            Request {
                kind: "password",
                message: "Enter locally",
                url: "",
                window: "0xdead",
                timeout_secs: 60,
            },
            "",
            false,
        )
        .unwrap();
    let id = request_id(&record);
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(["human", "assist", &id])
        .env("ZUSE_STATE_DIR", &path)
        .env("ZUSE_BACKEND", "unsupported")
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        !marker.exists(),
        "unavailable backend must be detected before collecting a password"
    );
    assert_eq!(q.status(&id).unwrap(), "pending");
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn operator_cli_respects_read_only_and_dry_run() {
    let root = std::env::temp_dir().join(format!(
        "zuse-operator-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let queue = zuse::human::Queue::at(root.join("human")).unwrap();
    let result = queue
        .request(
            zuse::human::Request {
                kind: "approval",
                message: "test",
                url: "",
                window: "",
                timeout_secs: 60,
            },
            "",
            false,
        )
        .unwrap();
    let id = zuse::json::with_value(&result, |v| {
        zuse::json::text(v.get("request").unwrap().get("id").unwrap())
    })
    .unwrap();
    for flag in ["--read-only", "--dry-run"] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_zuse"))
            .args([flag, "human", "approve", &id])
            .env("ZUSE_STATE_DIR", &root)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert_eq!(queue.status(&id).unwrap(), "pending");
    }
    std::fs::remove_dir_all(root).unwrap();
}
