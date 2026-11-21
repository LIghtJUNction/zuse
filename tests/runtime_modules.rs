#![cfg(feature = "std")]
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
use zuse::{
    json,
    observation::Cancellation,
    runtime::{Options, Runtime},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zuse-runtime-modules-{}-{}",
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

fn field(output: &str, key: &str) -> String {
    json::with_value(output, |root| Ok(root.get(key).unwrap().raw().to_owned())).unwrap()
}

fn stopped_timer(machine: &mut Runtime) {
    let status = machine
        .execute("timer.status()", Options::default())
        .unwrap();
    assert!(status.contains("\"running\":false"), "{status}");
}

#[test]
fn runtime_keeps_imported_functions_and_calls_them_again_without_reloading() {
    let mut machine = Runtime::new();
    let defined = machine
        .execute(
            r#"eval(source="fn begin(){timer.start()} fn inspect(){timer.status()}",as="tools")"#,
            Options::default(),
        )
        .unwrap();
    assert_eq!(field(&defined, "completed"), "0");
    assert_eq!(machine.functions(), ["tools.begin", "tools.inspect"]);
    stopped_timer(&mut machine);
    let first = machine
        .execute("tools.begin();tools.inspect()", Options::default())
        .unwrap();
    assert_eq!(field(&first, "completed"), "2");
    assert!(first.contains("\"running\":true"));
    let second = machine
        .execute("tools.inspect();tools.inspect()", Options::default())
        .unwrap();
    assert_eq!(field(&second, "completed"), "2");
    assert!(
        Runtime::new()
            .execute("tools.inspect()", Options::default())
            .is_err(),
        "definitions must not leak to another Runtime"
    );
}

#[test]
fn failed_whole_program_preflight_commits_neither_functions_nor_effects() {
    let mut machine = Runtime::new();
    machine
        .execute("fn retained(){timer.status()}", Options::default())
        .unwrap();
    let source = r#"import(source="fn begin(){timer.start()}",as="pending");pending.begin();operation_does_not_exist()"#;
    let error = machine.execute(source, Options::default()).unwrap_err();
    assert!(error.contains("unknown operation"), "{error}");
    assert_eq!(machine.functions(), ["retained"]);
    stopped_timer(&mut machine);

    let source =
        r#"eval(source="fn mutate(){timer.start();keyboard.type(text='draft only')} mutate()")"#;
    let error = machine
        .execute(
            source,
            Options {
                read_only: true,
                ..Options::default()
            },
        )
        .unwrap_err();
    assert!(error.contains("read-only"), "{error}");
    assert_eq!(machine.functions(), ["retained"]);
    stopped_timer(&mut machine);
}

#[test]
fn runtime_dry_run_does_not_register_or_execute_new_definitions() {
    let mut machine = Runtime::new();
    let result = machine
        .execute(
            "fn begin(){timer.start()} begin()",
            Options {
                dry_run: true,
                ..Options::default()
            },
        )
        .unwrap();
    assert_eq!(field(&result, "steps"), "1");
    assert!(machine.functions().is_empty());
    assert!(machine.execute("begin()", Options::default()).is_err());
    stopped_timer(&mut machine);
}

#[test]
fn runtime_uses_one_aggregate_budget_for_nested_eval_and_calls() {
    let mut machine = Runtime::new();
    let source =
        r#"eval(source="fn pause(ms){wait(ms=ms)}");timer.start();pause(ms=40);pause(ms=40)"#;
    let error = machine
        .execute(
            source,
            Options {
                max_wait_ms: 70,
                ..Options::default()
            },
        )
        .unwrap_err();
    assert!(error.contains("wait budget"), "{error}");
    assert!(machine.functions().is_empty());
    stopped_timer(&mut machine);
    let error = machine
        .execute(
            source,
            Options {
                max_steps: 2,
                ..Options::default()
            },
        )
        .unwrap_err();
    assert!(error.contains("step limit"), "{error}");
    stopped_timer(&mut machine);
}

#[test]
fn already_cancelled_preparation_keeps_definitions_and_effects_uncommitted() {
    let mut machine = Runtime::new();
    machine.cancellation().cancel();
    let definitions = machine
        .execute("fn begin(){timer.start()}", Options::default())
        .unwrap();
    assert_eq!(field(&definitions, "status"), "\"failed\"");
    assert_eq!(field(&definitions, "completed"), "0");
    assert!(definitions.contains("\"error_code\":\"cancelled\""));
    assert!(machine.functions().is_empty());
    let calls = machine
        .execute("fn begin(){timer.start()} begin()", Options::default())
        .unwrap();
    assert_eq!(field(&calls, "completed"), "0");
    assert!(machine.functions().is_empty());
    machine.set_cancellation(Cancellation::default());
    stopped_timer(&mut machine);
}

#[test]
fn runtime_entry_base_is_explicit_and_function_calls_do_not_reread_files() {
    let files = Files::new();
    let library = files.write("helpers.zuse", "fn inspect(){timer.status()}");
    let cwd = std::env::current_dir().unwrap();
    let mut machine = Runtime::new();
    machine
        .execute_with_base(
            "import(path=\"helpers.zuse\",as=\"helpers\")",
            Options::default(),
            &files.0,
        )
        .unwrap();
    fs::remove_file(library).unwrap();
    let result = machine
        .execute("helpers.inspect();helpers.inspect()", Options::default())
        .unwrap();
    assert_eq!(field(&result, "completed"), "2");
    assert_eq!(std::env::current_dir().unwrap(), cwd);
}

#[test]
fn cli_run_script_and_check_use_the_entry_directory_for_relative_imports() {
    let files = Files::new();
    let elsewhere = Files::new();
    files.write("helpers.zuse", "fn inspect(){timer.status()}");
    let entry = files.write(
        "main.zuse",
        "import(path=\"helpers.zuse\",as=\"helpers\");helpers.inspect();helpers.inspect()",
    );
    for command in ["run", "script", "check"] {
        let output = Command::new(env!("CARGO_BIN_EXE_zuse"))
            .arg(command)
            .arg(&entry)
            .current_dir(&elsewhere.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        if command == "check" {
            assert_eq!(field(text.trim(), "steps"), "2");
        } else {
            assert_eq!(field(text.trim(), "completed"), "2");
        }
    }
}

#[test]
fn cli_scoped_module_and_legacy_borrow_programs_preflight_without_a_desktop() {
    let files = Files::new();
    let elsewhere = Files::new();
    files.write("mouse_helpers.zuse", "fn inspect(m){m.position()}");
    let entry = files.write(
        "scoped.zuse",
        "import(path=\"mouse_helpers.zuse\",as=\"ops\");let pointer=mouse();ops.inspect(pointer)",
    );
    let legacy = files.write(
        "legacy.zuse",
        "fn inspect(m){m.position()} let pointer=mouse();inspect(pointer)",
    );
    for entry in [&entry, &legacy] {
        let output = Command::new(env!("CARGO_BIN_EXE_zuse"))
            .args(["--dry-run", "script"])
            .arg(entry)
            .current_dir(&elsewhere.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(field(text.trim(), "status"), "\"planned\"");
        assert!(
            text.contains("\"mice\":1") && text.contains("\"steps\":2"),
            "{text}"
        );
    }
}

#[test]
fn cli_shell_reuses_imported_functions_across_input_lines() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zuse"))
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"eval(source=\"fn inspect(){timer.status()}\")\ninspect();inspect()\n:quit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(field(lines[0], "completed"), "0");
    assert_eq!(field(lines[1], "completed"), "2");
}
