#![cfg(feature = "std")]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use zuse::{
    json,
    mcp::Engine,
    observation::Cancellation,
    runtime::{Options, Runtime},
    script::ModuleSession,
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zuse-metadata-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, source).unwrap();
        path
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn import(source: &str, namespace: &str, replace: bool) -> String {
    format!(
        "import(source={},as={},replace={replace})",
        json::quote(source),
        json::quote(namespace)
    )
}
fn read_only() -> Options {
    Options {
        read_only: true,
        ..Options::default()
    }
}
fn count(catalog: &str) -> usize {
    json::with_value(catalog, |root| {
        Ok(root.get("functions").unwrap().elements().count())
    })
    .unwrap()
}
fn stopped_timer(runtime: &mut Runtime) {
    let status = runtime.execute("timer.status()", read_only()).unwrap();
    assert!(status.contains("\"running\":false"), "{status}");
}

#[test]
fn nested_file_metadata_keeps_descriptions_parameters_and_source_without_reloading() {
    let files = Files::new();
    let nested = files.write(
        "nested.zuse",
        "/// Inspect the timer.\nfn inspect(){timer.status()}",
    );
    let library = files.write(
        "library.zuse",
        "import(path=\"nested.zuse\",as=\"nested\")\n/// Prepare \"你好\" without sending.\n/// Keep the selected editor explicit.\n  fn prepare(message, editor){accessibility.set_text(path=editor,text=message)}",
    );
    let mut runtime = Runtime::new();
    let code = format!(
        "import(path={},as=\"chat\")",
        json::quote(library.to_str().unwrap())
    );
    let receipt = runtime.execute(&code, read_only()).unwrap();
    assert!(receipt.contains("\"completed\":0"), "{receipt}");
    let metadata = runtime.function_metadata();
    assert_eq!(metadata.len(), 2);
    assert_eq!(metadata[0].name, "chat.nested.inspect");
    let prepare = &metadata[1];
    assert_eq!(prepare.name, "chat.prepare");
    assert_eq!(prepare.parameters, ["message", "editor"]);
    assert_eq!(
        prepare.description,
        "Prepare \"你好\" without sending.\nKeep the selected editor explicit."
    );
    assert_eq!(
        prepare.source,
        library.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!((prepare.line, prepare.column), (4, 3));
    fs::remove_file(library).unwrap();
    fs::remove_file(nested).unwrap();
    assert_eq!(runtime.function_metadata(), metadata);
    let catalog = runtime.function_catalog("chat");
    assert_eq!(count(&catalog), 2);
    json::with_value(&catalog, |root| {
        let function = root.get("functions").unwrap().elements().nth(1).unwrap();
        assert_eq!(
            json::text(function.get("description").unwrap())?,
            prepare.description
        );
        assert!(function.get("body").is_none());
        Ok(())
    })
    .unwrap();
    stopped_timer(&mut runtime);
}

#[test]
fn documentation_is_lexical_adjacent_and_preserves_utf8_and_crlf() {
    let source = concat!(
        "\u{feff}/// Read 状态.\r\n/// Second line.\r\nfn described(){}\r\n",
        "/// Detached by an empty line.\r\n\r\nfn blank(){}\r\n",
        "/// Detached by ordinary comment.\n// unrelated\nfn ordinary(){}\n",
        "/// Detached by a block comment.\n/* unrelated */\nfn block(){}\n",
        "//// Not a documentation comment.\nfn slashes(){}\n",
        "fn preceding(){} /// A trailing comment is not attached.\nfn following(){}\n",
        "/* /// not documentation\n */\nfn commented(){}\n",
        "fn literal(){keyboard.type(text=\"/// not documentation\")}\n",
        "# ignored comment\nfn hash(){}\n",
    );
    let prepared = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap();
    let metadata = prepared.next_session.function_metadata();
    assert_eq!(metadata.len(), 10);
    for function in &metadata {
        let expected = if function.name == "described" {
            "Read 状态.\nSecond line."
        } else {
            ""
        };
        assert_eq!(function.description, expected, "{}", function.name);
    }
}

#[test]
fn catalog_filter_uses_namespace_boundaries_and_declaration_parameter_order() {
    let mut runtime = Runtime::new();
    for namespace in ["tools", "tools_extra"] {
        runtime
            .execute(
                &import(
                    "/// Delay by the first parameter.\nfn pause(second,first){wait(ms=second)}",
                    namespace,
                    false,
                ),
                read_only(),
            )
            .unwrap();
    }
    runtime
        .execute("fn root(){}; functions(namespace=\"tools\")", read_only())
        .unwrap();
    assert_eq!(count(&runtime.function_catalog("")), 3);
    assert_eq!(count(&runtime.function_catalog("tools")), 1);
    assert_eq!(count(&runtime.function_catalog("tool")), 0);
    assert_eq!(count(&runtime.function_catalog("missing")), 0);
    assert_eq!(
        runtime.function_metadata()[1].parameters,
        ["second", "first"]
    );
    let result = runtime
        .execute("functions(namespace=\"tools\")", read_only())
        .unwrap();
    assert!(result.contains("\"operation\":\"functions\""), "{result}");
    assert!(result.contains("\"name\":\"tools.pause\""), "{result}");
    assert!(!result.contains("tools_extra"));
    assert_eq!(
        runtime.functions(),
        ["root", "tools.pause", "tools_extra.pause"]
    );
}

#[test]
fn metadata_replacement_is_transactional_for_preflight_dry_run_and_cancellation() {
    let old = import(
        "/// Old description.\nfn inspect(){timer.status()}",
        "lib",
        false,
    );
    let new = import(
        "/// New description.\nfn inspect(){timer.start()}",
        "lib",
        true,
    );
    let mut runtime = Runtime::new();
    runtime.execute(&old, read_only()).unwrap();
    let before = runtime.function_metadata();
    assert!(
        runtime
            .execute(&format!("{new};unknown_operation()"), read_only())
            .is_err()
    );
    assert_eq!(runtime.function_metadata(), before);
    runtime
        .execute(
            &new,
            Options {
                dry_run: true,
                ..read_only()
            },
        )
        .unwrap();
    assert_eq!(runtime.function_metadata(), before);
    runtime.cancellation().cancel();
    let cancelled = runtime.execute(&new, read_only()).unwrap();
    assert!(
        cancelled.contains("\"error_code\":\"cancelled\""),
        "{cancelled}"
    );
    assert_eq!(runtime.function_metadata(), before);
    runtime.set_cancellation(Cancellation::default());
    runtime.execute(&new, read_only()).unwrap();
    assert_eq!(
        runtime.function_metadata()[0].description,
        "New description."
    );
    assert!(runtime.execute(&old, read_only()).is_err());
    stopped_timer(&mut runtime);
}

#[test]
fn changed_documentation_requires_explicit_replacement() {
    let mut runtime = Runtime::new();
    runtime
        .execute("/// Original.\nfn inspect(){}", read_only())
        .unwrap();
    assert!(
        runtime
            .execute("/// Changed.\nfn inspect(){}", read_only())
            .is_err()
    );
    assert_eq!(runtime.function_metadata()[0].description, "Original.");
    runtime
        .execute("/// Original.\nfn inspect(){}", read_only())
        .unwrap();
    assert_eq!(runtime.function_metadata().len(), 1);
}

#[test]
fn oversized_description_fails_before_effects_and_counts_utf8_bytes() {
    let mut runtime = Runtime::new();
    let source = format!("timer.start();\n/// {}\nfn inspect(){{}}", "é".repeat(2049));
    let error = runtime.execute(&source, read_only()).unwrap_err();
    assert!(error.contains("description exceeds 4096 bytes"), "{error}");
    assert!(runtime.function_metadata().is_empty());
    stopped_timer(&mut runtime);
    let source = format!("/// {}\nfn inspect(){{}}", "é".repeat(2048));
    runtime.execute(&source, read_only()).unwrap();
    assert_eq!(runtime.function_metadata()[0].description.len(), 4096);
}

#[test]
fn cli_reports_documented_functions_without_calling_them() {
    let output = Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(["--read-only", "eval", "/// Start only when explicitly called.\nfn begin(){timer.start()}\nfunctions();timer.status()"])
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\"description\":\"Start only when explicitly called.\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"parameters\":[]"), "{stdout}");
    assert!(stdout.contains("\"running\":false"), "{stdout}");
}

fn engine() -> Engine {
    let mut engine = Engine::new(read_only());
    engine.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"metadata-test","version":"1"}}}"#).unwrap();
    engine
        .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .unwrap();
    engine
}
fn rpc(engine: &mut Engine, id: usize, name: &str, arguments: &str) -> String {
    let request = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":{},\"arguments\":{arguments}}}}}",
        json::quote(name)
    );
    String::from_utf8(engine.handle(request.as_bytes()).unwrap().unwrap()).unwrap()
}
fn tool_text(reply: &str) -> String {
    json::with_value(reply, |root| {
        let result = root.get("result").ok_or("missing MCP result")?;
        assert_eq!(
            result.get("isError").unwrap().boolean(),
            Some(false),
            "{reply}"
        );
        json::text(
            result
                .get("content")
                .unwrap()
                .elements()
                .next()
                .unwrap()
                .get("text")
                .unwrap(),
        )
    })
    .unwrap()
}

#[test]
fn mcp_catalog_is_advertised_validated_and_isolated_to_its_session() {
    let mut engine = engine();
    let tools = String::from_utf8(
        engine
            .handle(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(tools.contains("\"name\":\"zuse.functions\""), "{tools}");
    let code = import(
        "/// Starts only on explicit invocation.\nfn begin(){timer.start()}",
        "tools",
        false,
    );
    let imported = rpc(
        &mut engine,
        3,
        "zuse.execute",
        &format!("{{\"code\":{}}}", json::quote(&code)),
    );
    assert!(
        tool_text(&imported).contains("\"completed\":0"),
        "{imported}"
    );
    let catalog = tool_text(&rpc(
        &mut engine,
        4,
        "zuse.functions",
        r#"{"namespace":"tools"}"#,
    ));
    assert_eq!(catalog, engine.runtime.function_catalog("tools"));
    assert_eq!(count(&catalog), 1);
    let invalid = rpc(&mut engine, 5, "zuse.functions", r#"{"namespace":7}"#);
    assert!(invalid.contains("\"code\":-32602"), "{invalid}");
    let invalid = rpc(&mut engine, 6, "zuse.functions", r#"{"execute":true}"#);
    assert!(invalid.contains("\"code\":-32602"), "{invalid}");
    stopped_timer(&mut engine.runtime);
    assert_eq!(
        count(&Engine::new(read_only()).runtime.function_catalog("")),
        0
    );
}
