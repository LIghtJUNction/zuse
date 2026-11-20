#![cfg(feature = "std")]
use std::{
    io::Write,
    process::{Command, Stdio},
};
use zuse::{json, mcp::Engine, runtime::Options};

fn cli(source: &str, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(["script", "-"])
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn cli_script_reads_stdin_and_dry_run_never_opens_a_display() {
    let output = cli(
        "let p=mouse(); p.move_to(text=\"保存\",role=\"button\");",
        &["--dry-run"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = String::from_utf8(output.stdout).unwrap();
    assert_eq!(json::field(&result, "status").unwrap(), "planned");
    assert!(!result.contains("保存"));
}
#[test]
fn cli_checks_complete_script_and_preserves_read_only_in_dry_run() {
    for (source, args) in [
        ("let p=mouse(); p.click(); invalid();", vec![]),
        ("let p=mouse();", vec!["--read-only", "--dry-run"]),
        ("wait(ms=2);", vec!["--max-wait-ms", "1"]),
    ] {
        let result = cli(source, &args);
        assert!(!result.status.success());
        assert!(
            result.stdout.is_empty(),
            "preflight errors belong on stderr"
        );
        assert!(!String::from_utf8_lossy(&result.stderr).contains("DISPLAY"));
    }
    let result = cli("parallel { wait(ms=0); wait(ms=0); }", &["--read-only"]);
    assert!(result.status.success());
    assert_eq!(
        json::field(&String::from_utf8(result.stdout).unwrap(), "status").unwrap(),
        "completed"
    );
}

fn rpc(engine: &mut Engine, source: &str, dry_run: bool, read_only: bool) -> String {
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"zuse.script","arguments":{{"code":{},"dry_run":{},"read_only":{}}},"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}}}}}"#,
        json::quote(source),
        dry_run,
        read_only
    );
    String::from_utf8(engine.handle(request.as_bytes()).unwrap().unwrap()).unwrap()
}
fn is_error(response: &str) -> bool {
    json::with_value(response, |root| {
        Ok(root
            .get("result")
            .unwrap()
            .get("isError")
            .and_then(|v| v.boolean())
            .unwrap_or(false))
    })
    .unwrap()
}
#[test]
fn mcp_script_is_discoverable_and_uses_same_preflight_as_cli() {
    let mut engine = Engine::new(Options::default());
    let listed=String::from_utf8(engine.handle(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#).unwrap().unwrap()).unwrap();
    assert!(listed.contains("zuse.script"));
    let valid = rpc(&mut engine, "let p=mouse(); p.move(x=2,y=3);", true, false);
    assert!(!is_error(&valid), "{valid}");
    assert!(valid.contains("planned"));
    let invalid = rpc(
        &mut engine,
        "let p=mouse(); parallel {p.click();p.click();}",
        true,
        false,
    );
    assert!(is_error(&invalid), "{invalid}");
    let ordinary = rpc(&mut engine, "wait(ms=0);", false, false);
    assert!(!is_error(&ordinary), "{ordinary}");
}
#[test]
fn mcp_script_cannot_weaken_server_read_only_policy() {
    let mut engine = Engine::new(Options {
        read_only: true,
        ..Options::default()
    });
    for dry_run in [false, true] {
        let result = rpc(
            &mut engine,
            "let p=user_mouse(appearance=\"system\");p.click();",
            dry_run,
            false,
        );
        assert!(is_error(&result), "{result}");
        assert!(result.contains("read-only"));
    }
}
