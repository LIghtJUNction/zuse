#![cfg(feature = "std")]

use std::{
    io::Write,
    process::{Command, Stdio},
};
use zuse::{
    json,
    mcp::Engine,
    runtime::{self, Options, Runtime},
    script,
};

fn cli(source: &str, scoped: bool, dry_run: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zuse"));
    if scoped {
        command.args(["script", "-"]);
    } else {
        command.args(["eval", source]);
    }
    if dry_run {
        command.arg("--dry-run");
    }
    let mut child = command
        .env("ZUSE_BACKEND", "x11")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if scoped {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

#[test]
fn flat_runtime_schema_and_capabilities_do_not_claim_native_support() {
    let schema = runtime::schema();
    for name in ["capabilities", "swipe", "pinch", "rotate", "cancel"] {
        assert!(schema.contains(&format!("touchpad.{name}")), "{schema}");
    }
    let output = Runtime::new()
        .execute(
            "touchpad.capabilities();",
            Options {
                read_only: true,
                ..Options::default()
            },
        )
        .unwrap();
    assert_eq!(json::field(&output, "status").unwrap(), "completed");
    assert!(output.contains("\"native\":false"), "{output}");
    assert!(
        output.contains("\"mouse_wheel_fallback\":false"),
        "{output}"
    );
}

#[test]
fn flat_runtime_plans_valid_gestures_and_rejects_invalid_arguments() {
    for source in [
        "touchpad.swipe(dx=120,dy=-20,fingers=3,duration=100,steps=10);",
        "touchpad.pinch(scale=1.2);",
        "touchpad.rotate(degrees=-90);",
        "touchpad.cancel();",
    ] {
        let output = Runtime::new()
            .execute(
                source,
                Options {
                    dry_run: true,
                    ..Options::default()
                },
            )
            .unwrap();
        assert_eq!(json::field(&output, "status").unwrap(), "valid");
        let error = Runtime::new()
            .execute(source, Options::default())
            .unwrap_err();
        assert!(error.starts_with("unsupported_capability:"), "{error}");
        for dry_run in [false, true] {
            let error = runtime::check(
                source,
                Options {
                    read_only: true,
                    dry_run,
                    ..Options::default()
                },
            )
            .unwrap_err();
            assert!(error.contains("read-only"), "{error}");
        }
    }
    for source in [
        "touchpad.swipe(fingers=1);",
        "touchpad.swipe(dx=1000001);",
        "touchpad.swipe(duration=0,steps=2);",
        "touchpad.pinch(scale=0);",
        "touchpad.rotate(degrees=3601);",
        "touchpad.pinch();",
        "touchpad.rotate();",
        "touchpad.capabilities(dx=1);",
        "touchpad.cancel(ms=1);",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn cli_rejects_late_native_gestures_before_any_mouse_creation_or_input() {
    for (source, scoped) in [
        (
            "let p=mouse();p.move(x=12,y=34,duration=0);touchpad.swipe(dx=10);",
            true,
        ),
        (
            "mouse.move(x=12,y=34,duration=0);touchpad.pinch(scale=1.2);",
            false,
        ),
        ("let p=mouse();p.click();touchpad.cancel();", true),
        ("mouse.move(x=12,y=34);touchpad.rotate(degrees=45);", false),
    ] {
        let output = cli(source, scoped, false);
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "native preflight error must precede a run receipt"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("unsupported_capability: touchpad."),
            "{error}"
        );
        assert!(!error.contains("DISPLAY"), "{error}");
        let output = cli(source, scoped, true);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn scoped_capability_queries_execute_without_a_display() {
    let output = cli("touchpad.capabilities();", true, false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    assert_eq!(json::field(&output, "status").unwrap(), "completed");
    assert!(output.contains("\"native\":false"), "{output}");
    let dry = script::execute(
        "touchpad.swipe(dx=100);",
        Options {
            dry_run: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(json::field(&dry, "status").unwrap(), "planned");
}

#[test]
fn flat_planning_charges_gesture_duration_and_total_samples() {
    let error = runtime::check(
        "touchpad.swipe(dx=1,duration=10);",
        Options {
            max_wait_ms: 9,
            ..Options::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("wait budget"), "{error}");
    let source = "touchpad.swipe(dx=1,duration=60000,steps=4096);".repeat(16);
    let error = runtime::check(
        &source,
        Options {
            max_wait_ms: u64::MAX,
            ..Options::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("touchpad samples exceed 65536"), "{error}");
}

#[test]
fn mcp_uses_the_same_dry_run_and_unsupported_native_paths() {
    let mut engine = Engine::new(Options::default());
    for name in ["zuse.execute", "zuse.script"] {
        for dry_run in [false, true] {
            let request = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":{},"arguments":{{"code":"touchpad.pinch(scale=1.2);","dry_run":{}}},"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}}}}}"#,
                json::quote(name),
                dry_run,
            );
            let response =
                String::from_utf8(engine.handle(request.as_bytes()).unwrap().unwrap()).unwrap();
            let is_error = json::with_value(&response, |value| {
                Ok(value
                    .get("result")
                    .and_then(|result| result.get("isError"))
                    .and_then(|value| value.boolean())
                    .unwrap_or(false))
            })
            .unwrap();
            assert_eq!(is_error, !dry_run, "{response}");
            if !dry_run {
                assert!(response.contains("unsupported_capability"), "{response}");
            }
        }
    }
}
