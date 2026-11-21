#![cfg(feature = "std")]
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use zuse::{
    language::Parser,
    runtime::{self, Options},
    script::{self, ModuleSession, ProgramMode},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zuse-modules-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, source: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, source).unwrap();
        path
    }
    fn import(&self, name: &str, namespace: &str) -> String {
        format!(
            "import(path={:?},as={namespace:?})",
            self.0.join(name).to_str().unwrap()
        )
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn definitions_are_transactional_and_reusable_across_calls() {
    let session = ModuleSession::default();
    let prepared = session
        .prepare("fn inspect(){window.list()}", Options::default())
        .unwrap();
    assert_eq!(prepared.mode, ProgramMode::Flat);
    assert!(prepared.source.is_empty());
    assert!(session.functions().is_empty(), "prepare must not commit");
    runtime::check(&prepared.source, Options::default()).unwrap();
    let session = prepared.next_session;
    assert_eq!(session.functions(), ["inspect"]);
    for _ in 0..2 {
        let call = session
            .prepare("inspect(); inspect()", Options::default())
            .unwrap();
        assert_eq!(call.source, "window.list();\nwindow.list();\n");
        runtime::check(&call.source, Options::default()).unwrap();
    }
}

#[test]
fn literal_arguments_cannot_inject_an_extra_operation() {
    let source =
        r#"fn compose(text){keyboard.type(text=text)} compose(text="hello\");mouse.click();#")"#;
    let prepared = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap();
    let calls = Parser::new(&prepared.source)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "keyboard.type");
    let value = calls[0].get("text").unwrap();
    let mut bytes = vec![0; value.decoded_len().unwrap()];
    assert_eq!(
        value.decode_text(&mut bytes).unwrap(),
        "hello\");mouse.click();#"
    );
    runtime::check(
        &prepared.source,
        Options {
            read_only: true,
            ..Options::default()
        },
    )
    .unwrap_err();
}

#[test]
fn relative_file_imports_resolve_in_the_module_directory() {
    let files = Files::new();
    files.write("shared.zuse", "fn inspect(){window.list()}");
    files.write(
        "wechat.zuse",
        "import(path=\"shared.zuse\",as=\"shared\"); fn read(){shared.inspect()}",
    );
    let source = format!(
        "{};wechat.read();wechat.read()",
        files.import("wechat.zuse", "wechat")
    );
    let prepared = ModuleSession::default()
        .prepare(&source, Options::default())
        .unwrap();
    assert_eq!(prepared.source, "window.list();\nwindow.list();\n");
    assert_eq!(
        prepared.next_session.functions(),
        ["wechat.read", "wechat.shared.inspect"]
    );
    runtime::check(
        &prepared.source,
        Options {
            read_only: true,
            ..Options::default()
        },
    )
    .unwrap();
    let cwd = std::env::current_dir().unwrap();
    let relative = ModuleSession::default()
        .prepare_with_base(
            "import(path=\"wechat.zuse\",as=\"wechat\");wechat.read()",
            Options::default(),
            &files.0,
        )
        .unwrap();
    assert_eq!(relative.source, "window.list();\n");
    assert_eq!(
        std::env::current_dir().unwrap(),
        cwd,
        "module loading must never chdir"
    );
}

#[test]
fn import_does_not_execute_top_level_actions_even_through_eval() {
    let session = ModuleSession::default();
    for source in [
        r#"import(source="window.list()",as="lib")"#,
        r#"import(source="keyboard.type(text='unexpected')",as="lib")"#,
        r#"import(source="eval(source='window.list()')",as="lib")"#,
    ] {
        let error = session.prepare(source, Options::default()).unwrap_err();
        assert!(error.contains("only function declarations"), "{error}");
        assert!(session.functions().is_empty());
    }
    let evaluated = session
        .prepare(
            r#"eval(source="fn inspect(){window.list()} inspect();inspect()")"#,
            Options::default(),
        )
        .unwrap();
    assert_eq!(evaluated.source, "window.list();\nwindow.list();\n");
    assert_eq!(evaluated.next_session.functions(), ["inspect"]);
}

#[test]
fn expansion_shares_step_wait_and_read_only_limits() {
    let session = ModuleSession::default();
    let source = "fn slow(){wait(ms=40)} slow();slow()";
    let options = Options {
        max_steps: 1,
        ..Options::default()
    };
    assert!(
        session
            .prepare(source, options)
            .unwrap_err()
            .contains("step limit")
    );
    let options = Options {
        max_wait_ms: 70,
        ..Options::default()
    };
    let prepared = session.prepare(source, options).unwrap();
    assert!(
        runtime::check(&prepared.source, options)
            .unwrap_err()
            .contains("wait budget")
    );
    let options = Options {
        read_only: true,
        ..Options::default()
    };
    let prepared = session.prepare(r#"import(source="fn send(text){keyboard.type(text=text)}",as="wechat");wechat.send(text="draft only")"#, options).unwrap();
    assert!(
        runtime::check(&prepared.source, options)
            .unwrap_err()
            .contains("read-only")
    );
    assert!(
        session.functions().is_empty(),
        "failed downstream preflight must leave the original session untouched"
    );
}

#[test]
fn cycles_missing_paths_and_nonliteral_imports_are_visible_errors() {
    let files = Files::new();
    files.write(
        "a.zuse",
        "import(path=\"b.zuse\",as=\"b\");fn a(){window.list()}",
    );
    files.write(
        "b.zuse",
        "import(path=\"a.zuse\",as=\"a\");fn b(){window.list()}",
    );
    let session = ModuleSession::default();
    let error = session
        .prepare(&files.import("a.zuse", "a"), Options::default())
        .unwrap_err();
    assert!(
        error.contains("cyclic import") && error.contains("a.zuse") && error.contains("b.zuse"),
        "{error}"
    );
    let error = session
        .prepare(&files.import("missing.zuse", "absent"), Options::default())
        .unwrap_err();
    assert!(error.contains("missing.zuse"), "{error}");
    for source in [
        "import(path=module_path)",
        "eval(source=dynamic_code)",
        "fn load(code){eval(source=code)}",
    ] {
        assert!(
            session.prepare(source, Options::default()).is_err(),
            "{source}"
        );
    }
    let error = session
        .prepare("fn a(){b()} fn b(){a()}", Options::default())
        .unwrap_err();
    assert!(error.contains("recursive function cycle"), "{error}");
    assert!(session.functions().is_empty());
}

#[test]
fn module_source_and_empty_repeat_expansion_are_bounded() {
    let files = Files::new();
    files.write(
        "too-large.zuse",
        vec![b' '; zuse::language::MAX_SCRIPT_BYTES + 1],
    );
    let error = ModuleSession::default()
        .prepare(&files.import("too-large.zuse", "large"), Options::default())
        .unwrap_err();
    assert!(error.contains("exceeds 1 MiB"), "{error}");
    let error = ModuleSession::default()
        .prepare("repeat 4096 {repeat 4096 {}}", Options::default())
        .unwrap_err();
    assert!(error.contains("expanded module work"), "{error}");
    let mut functions = String::new();
    for index in 0..257 {
        functions.push_str(&format!("fn f{index}(){{}}\n"));
    }
    assert!(
        ModuleSession::default()
            .prepare(&functions, Options::default())
            .unwrap_err()
            .contains("function count")
    );
}

#[test]
fn explicit_replacement_is_transactional_and_changes_only_the_namespace() {
    let session = ModuleSession::default().prepare(r#"import(source="fn inspect(){window.list()}",as="lib");fn unrelated(){window.list()}"#, Options::default()).unwrap().next_session;
    let update = r#"import(source="fn inspect(){system.capabilities()}",as="lib")"#;
    assert!(
        session
            .prepare(update, Options::default())
            .unwrap_err()
            .contains("already exists")
    );
    let prepared = session.prepare(r#"import(source="fn inspect(){system.capabilities()}",as="lib",replace=true);lib.inspect()"#, Options::default()).unwrap();
    assert_eq!(prepared.source, "system.capabilities();\n");
    assert_eq!(
        session
            .prepare("lib.inspect()", Options::default())
            .unwrap()
            .source,
        "window.list();\n"
    );
    assert!(
        prepared
            .next_session
            .functions()
            .contains(&"unrelated".into())
    );
    assert!(
        session
            .prepare("fn wait(){window.list()}", Options::default())
            .is_err(),
        "built-ins cannot be replaced"
    );
}

#[test]
fn module_functions_use_their_own_namespace_and_validate_arguments() {
    let source = r#"fn inner(text){notify(message=text)} import(source="fn inner(text){keyboard.type(text=text)} fn outer(text){inner(text=text)}",as="lib");lib.outer(text="draft")"#;
    let prepared = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap();
    assert_eq!(prepared.source, "keyboard.type(text=\"draft\");\n");
    for call in ["f()", "f(extra=1)", "f(1,x=2)", "f(x=window.list())"] {
        let source = format!("fn f(x){{keyboard.type(text=x)}} {call}");
        assert!(
            ModuleSession::default()
                .prepare(&source, Options::default())
                .is_err(),
            "{call}"
        );
    }
}

#[test]
fn scoped_import_preserves_borrowed_mouse_identity_and_parallel_conflicts() {
    let source = r#"import(source="fn tap(m){m.click(button='left')}",as="ops");let pointer=mouse();ops.tap(pointer);ops.tap(m=pointer)"#;
    let prepared = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap();
    assert_eq!(prepared.mode, ProgramMode::Scoped);
    let plan = prepared.compile_scoped(Options::default()).unwrap();
    assert_eq!(plan.summary.mice, 1);
    assert_eq!(plan.summary.steps, 3);
    assert_eq!(
        script::compile(source, Options::default()).unwrap().summary,
        plan.summary
    );

    let conflicting = r#"import(source="fn tap(m){m.click()}",as="ops");let pointer=mouse();parallel{ops.tap(pointer);ops.tap(pointer)}"#;
    let error = script::compile(conflicting, Options::default()).unwrap_err();
    assert!(error.contains("parallel branches both use"), "{error}");
    let leak =
        r#"import(source="fn leak(){outside.click()}",as="ops");let outside=mouse();ops.leak()"#;
    assert!(
        script::compile(leak, Options::default())
            .unwrap_err()
            .contains("out-of-scope")
    );
}

#[test]
fn syntax_selection_ignores_strings_and_keeps_existing_flat_spellings() {
    assert!(
        !script::uses_scoped_syntax(r#"keyboard.type(text="let m=mouse();parallel{m.click()}")"#)
            .unwrap()
    );
    assert!(script::uses_scoped_syntax("fn tap(m){m.click()} let m=mouse();tap(m)").unwrap());
    let source = "android.browser.sockets(serial=\"fixture\"); keyboard.type(text=fn); keyboard.type(text=left-handed); serial.ports()";
    let prepared = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap();
    assert_eq!(Parser::new(&prepared.source).count(), 4);
    runtime::check(
        &prepared.source,
        Options {
            dry_run: true,
            ..Options::default()
        },
    )
    .unwrap();
    let legacy = "fn inspect(m){m.position()} let pointer=mouse();inspect(pointer)";
    let plan = script::compile(legacy, Options::default()).unwrap();
    assert_eq!(plan.summary.mice, 1);
    assert_eq!(plan.summary.steps, 2);
}

#[test]
fn scoped_module_aliases_follow_block_and_declaration_order() {
    let import = r#"import(source="fn tap(m){m.click()}",as="ops");let pointer=mouse();"#;
    for (body, steps) in [
        (
            "ops.tap(pointer);{let ops=mouse();ops.click();}ops.tap(pointer);",
            5,
        ),
        (
            "ops.tap(pointer);sequence{let ops=mouse();ops.click();}ops.tap(pointer);",
            5,
        ),
        (
            "ops.tap(pointer);repeat 1{let ops=mouse();ops.click();}ops.tap(pointer);",
            5,
        ),
        ("ops.tap(pointer);let ops=mouse();ops.click();", 4),
    ] {
        let source = format!("{import}{body}");
        let prepared = ModuleSession::default()
            .prepare(&source, Options::default())
            .unwrap();
        let plan = prepared.compile_scoped(Options::default()).unwrap();
        assert_eq!(plan.summary.mice, 2, "{body}");
        assert_eq!(plan.summary.steps, steps, "{body}");
    }
}

#[test]
fn scoped_module_aliases_are_not_shadowed_by_parallel_siblings() {
    let import = r#"import(source="fn tap(m){m.click()}",as="ops");let pointer=mouse();"#;
    for branches in [
        "{let ops=mouse();ops.click();} ops.tap(pointer);",
        "ops.tap(pointer); {let ops=mouse();ops.click();}",
        "let ops=mouse(); ops.tap(pointer);",
    ] {
        let source = format!("{import}parallel{{{branches}}}");
        let prepared = ModuleSession::default()
            .prepare(&source, Options::default())
            .unwrap();
        let plan = prepared.compile_scoped(Options::default()).unwrap();
        assert_eq!(plan.summary.mice, 2, "{branches}");
        assert_eq!(plan.summary.parallel_groups, 1, "{branches}");
        assert_eq!(
            plan.summary.steps,
            if branches.starts_with("let") { 3 } else { 4 },
            "{branches}"
        );
    }
}

#[test]
fn scoped_module_functions_keep_local_and_parameter_shadowing() {
    let source = r#"
        import(source="fn tap(m){m.click()}",as="ops");
        fn invoke(m){
            ops.tap(m);
            {let ops=mouse();ops.click();}
            ops.tap(m);
        }
        let pointer=mouse();
        invoke(pointer);
    "#;
    let prepared = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap();
    let plan = prepared.compile_scoped(Options::default()).unwrap();
    assert_eq!(plan.summary.mice, 2);
    assert_eq!(plan.summary.steps, 5);

    let source = r#"
        import(source="fn click(m){m.down();m.up()}",as="ops");
        fn invoke(ops){ops.click();}
        let pointer=mouse();
        invoke(pointer);
    "#;
    let prepared = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap();
    let plan = prepared.compile_scoped(Options::default()).unwrap();
    assert_eq!(plan.summary.mice, 1);
    assert_eq!(plan.summary.steps, 2);
}

#[test]
fn module_alias_shadowing_does_not_hide_function_cycles() {
    let source = r#"
        import(source="fn b(){a()}",as="ops");
        fn a(){ops.b();{let ops=mouse();}}
    "#;
    let error = ModuleSession::default()
        .prepare(source, Options::default())
        .unwrap_err();
    assert!(error.contains("recursive function cycle"), "{error}");
}
