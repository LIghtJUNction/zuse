#![cfg(feature = "std")]
use zuse::{
    human::{Queue, Request},
    json,
    mcp::Engine,
    runtime::{self, Options, Runtime},
};
fn unique(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "zuse-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
fn id(result: &str) -> String {
    json::with_value(result, |v| {
        json::text(v.get("request").unwrap().get("id").unwrap())
    })
    .unwrap()
}
#[test]
fn validates_whole_program_before_any_side_effect() {
    let marker = unique("preflight");
    let source = format!(
        "process.start(program=\"sh\",args={}); unknown.operation()",
        json::quote(&format!("-c 'touch {}'", marker.display()))
    );
    assert!(Runtime::new().execute(&source, Options::default()).is_err());
    assert!(!marker.exists());
}
#[test]
fn read_only_denies_mutation_even_in_dry_run() {
    let opt = Options {
        read_only: true,
        dry_run: true,
        ..Options::default()
    };
    assert!(runtime::check("keyboard.type(text=\"hi\")", opt).is_err());
    assert!(runtime::check("mouse.position(); window.list()", opt).is_ok());
}
#[test]
fn semantic_errors_are_caught_before_execution() {
    for source in [
        "wait(ms=-1)",
        "wait(ms=60001)",
        "wait(min=50,max=1)",
        "mouse.move(x=1,y=2,duration=1,steps=3)",
        "mouse.move(x=1,y=2,curve=\"bad\")",
        "mouse.click(button=\"invalid\")",
        "screen.capture()",
        "keyboard.type(text=true)",
        "human.request(kind=\"password\",message=\"password\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
}
#[test]
fn budgets_are_aggregate_and_explicit() {
    assert!(runtime::check("wait(ms=40000);wait(ms=40000)", Options::default()).is_err());
    assert!(
        runtime::check(
            "wait();wait()",
            Options {
                max_steps: 1,
                ..Options::default()
            }
        )
        .is_err()
    );
    let mut r = Runtime::new();
    let out = r
        .execute("wait(min=1,max=2,seed=1)", Options::default())
        .unwrap();
    assert!(!runtime::failed(&out));
    assert!(out.contains("waited_ms"));
}
#[test]
fn human_handoff_requires_approval_and_is_consumed_once() {
    let path = unique("human");
    let queue = Queue::at(path.clone()).unwrap();
    let record = queue
        .request(
            Request {
                kind: "yubikey",
                message: "Touch your security key",
                url: "",
                window: "",
                timeout_secs: 60,
            },
            "wait(ms=1)",
            false,
        )
        .unwrap();
    let id = id(&record);
    assert_eq!(queue.status(&id).unwrap(), "pending");
    assert!(queue.take(&id).is_err());
    queue.respond(&id, true).unwrap();
    assert_eq!(queue.status(&id).unwrap(), "approved");
    assert_eq!(queue.take(&id).unwrap(), "wait(ms=1)");
    assert!(queue.take(&id).is_err());
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn human_denial_cannot_resume_and_paths_are_bounded() {
    let path = unique("deny");
    let q = Queue::at(path.clone()).unwrap();
    let record = q
        .request(
            Request {
                kind: "approval",
                message: "Continue",
                url: "",
                window: "",
                timeout_secs: 60,
            },
            "",
            false,
        )
        .unwrap();
    let id = id(&record);
    q.respond(&id, false).unwrap();
    assert_eq!(q.status(&id).unwrap(), "denied");
    assert!(q.take(&id).is_err());
    assert!(q.record("../../escape").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(path.join(format!("{id}.json")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn json_roundtrip_preserves_control_and_unicode() {
    let data = "你好\n\"quoted\"\\path\t\u{1}";
    let encoded = json::quote(data);
    assert_eq!(json::with_value(&encoded, json::text).unwrap(), data);
}
fn rpc(engine: &mut Engine, request: &str) -> String {
    String::from_utf8(engine.handle(request.as_bytes()).unwrap().unwrap()).unwrap()
}
#[test]
fn modern_mcp_lists_tools_and_executes_checks() {
    let mut engine = Engine::new(Options::default());
    let response = rpc(
        &mut engine,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
    );
    assert!(response.contains("zuse.execute"), "{response}");
    assert!(response.contains("zuse.screenshot"));
    let response = rpc(
        &mut engine,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"zuse.execute","arguments":{"code":"wait(ms=0)"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
    );
    assert!(response.contains("completed"), "{response}");
    assert!(response.contains("\"isError\":false"), "{response}");
}
#[test]
fn legacy_mcp_negotiates_and_rejects_invalid_tools() {
    let mut engine = Engine::new(Options::default());
    let response = rpc(
        &mut engine,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"tests","version":"1"}}}"#,
    );
    assert!(response.contains("2025-11-25"), "{response}");
    assert!(
        engine
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .unwrap()
            .is_none()
    );
    let response = rpc(
        &mut engine,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"zuse.execute","arguments":{"code":"nonsense()"}}}"#,
    );
    assert!(response.contains("\"isError\":true"), "{response}");
    let response = rpc(
        &mut engine,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"does_not_exist","arguments":{}}}"#,
    );
    assert!(response.contains("-32602"), "{response}");
}
