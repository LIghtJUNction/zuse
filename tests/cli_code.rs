#![cfg(feature = "std")]

use std::process::{Command, Output, Stdio};
use zuse::json;

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn successful(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn values(receipt: &str) -> Vec<String> {
    json::with_value(receipt, |root| {
        Ok(root
            .get("results")
            .unwrap()
            .elements()
            .map(|result| result.get("value").unwrap().raw().to_owned())
            .collect())
    })
    .unwrap()
}

#[test]
fn inline_code_reuses_eval_for_multiline_functions_and_global_options() {
    let source = "fn inspect(){timer.status()}\ninspect();inspect()";
    let expected = successful(cli(&["eval", source, "--read-only", "--max-steps", "2"]));
    for flag in ["-c", "--code"] {
        let receipt = successful(cli(&["--read-only", flag, source, "--max-steps", "2"]));
        assert_eq!(json::field(&receipt, "status").unwrap(), "completed");
        assert_eq!(values(&receipt).len(), 2);
        assert_eq!(values(&receipt), values(&expected));
    }
}

#[test]
fn inline_code_preserves_dry_run_and_read_only_before_desktop_input() {
    let source = r#"mouse.use(kind="user",appearance="system"); mouse.move(x=400,y=240)"#;
    let expected = successful(cli(&["eval", source, "--dry-run"]));
    let planned = successful(cli(&["--dry-run", "-c", source]));
    assert_eq!(planned, expected);
    let rejected = cli(&["-c", source, "--dry-run", "--read-only"]);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("read-only"));
}

#[test]
fn inline_code_preserves_statement_and_wait_budgets() {
    for (source, flag, expected) in [
        ("timer.status();timer.status()", "--max-steps", "step limit"),
        ("wait(ms=2)", "--max-wait-ms", "wait budget"),
    ] {
        let rejected = cli(&["-c", source, flag, "1"]);
        assert!(!rejected.status.success());
        assert!(rejected.stdout.is_empty());
        let error = String::from_utf8_lossy(&rejected.stderr);
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn inline_code_rejects_missing_duplicate_and_conflicting_arguments() {
    for (args, expected) in [
        (vec!["-c"], "value"),
        (vec!["-c", "timer.status()", "-c", "timer.status()"], "code"),
        (
            vec!["--code", "timer.status()", "-c", "timer.status()"],
            "code",
        ),
        (
            vec!["schema", "-c", "timer.status()"],
            "command or positional",
        ),
        (
            vec!["-c", "timer.status()", "eval", "timer.status()"],
            "command or positional",
        ),
        (
            vec!["-c", "timer.status()", "extra"],
            "command or positional",
        ),
        (vec!["-c", "timer.status()", "--stdio"], "transport options"),
        (
            vec!["-c", "timer.status()", "--check"],
            "only valid for zuse update",
        ),
    ] {
        let rejected = cli(&args);
        assert!(!rejected.status.success(), "{args:?}");
        assert!(rejected.stdout.is_empty(), "{args:?}");
        let error = String::from_utf8_lossy(&rejected.stderr);
        assert!(error.contains(expected), "{args:?}: {error}");
    }
}
