#![cfg(feature = "std")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use zuse::{
    json, packages,
    runtime::{self, Options},
    script::{ModuleSession, ProgramMode},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "zuse-package-imports-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let project = root.join("app");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir(root.join("empty-hooks")).unwrap();
        packages::run_at(&project, &["init", "app"]).unwrap();
        Self { root, project }
    }

    fn git(&self, directory: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-c")
            .arg(format!(
                "core.hooksPath={}",
                self.root.join("empty-hooks").display()
            ))
            .args([
                "-c",
                "user.name=Zuse fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn package(
        &self,
        name: &str,
        files: &[(&str, &str)],
        exports: &str,
        dependencies: &str,
    ) -> PathBuf {
        let repository = self.root.join(name);
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("zuse.json"), format!(
            "{{\"schema\":1,\"name\":{},\"version\":\"0.1.0\",\"entry\":\"lib.zuse\",\"exports\":{exports},\"dependencies\":{dependencies}}}",
            json::quote(name)
        )).unwrap();
        for (path, source) in files {
            let path = repository.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        self.git(&repository, &["init"]);
        self.git(&repository, &["add", "."]);
        self.git(&repository, &["commit", "-m", "offline package fixture"]);
        self.git(&repository, &["tag", "v0.1.0"]);
        repository.canonicalize().unwrap()
    }

    fn add(&self, repository: &Path) {
        packages::run_at(
            &self.project,
            &["add", repository.to_str().unwrap(), "--rev", "v0.1.0"],
        )
        .unwrap();
    }

    fn prepare(&self, session: &ModuleSession, source: &str) -> zuse::script::Prepared {
        session
            .prepare_with_base(source, Options::default(), &self.project)
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn package_entry_exports_and_relative_helpers_expand_without_network() {
    let fixture = Fixture::new();
    let repository = fixture.package(
        "tools",
        &[
            (
                "lib.zuse",
                "import(path=\"helpers/read.zuse\",as=\"helper\"); fn inspect(){helper.inspect()}",
            ),
            ("helpers/read.zuse", "fn inspect(){timer.status()}"),
            ("drawing.zuse", "fn pause(){wait(ms=7)}"),
        ],
        r#"{"drawing":"drawing.zuse"}"#,
        "{}",
    );
    fixture.add(&repository);
    // The origin is unavailable after installation; imports use only the lock/cache.
    fs::rename(&repository, fixture.root.join("unavailable-origin")).unwrap();
    let prepared = fixture.prepare(
        &ModuleSession::default(),
        r#"
        import(path="pkg:tools"); import(path="pkg:tools/drawing");
        tools.inspect(); drawing.pause()
    "#,
    );
    assert_eq!(prepared.source, "timer.status();\nwait(ms=7);\n");
    assert_eq!(
        prepared.next_session.functions(),
        ["drawing.pause", "tools.helper.inspect", "tools.inspect"]
    );
    runtime::check(
        &prepared.source,
        Options {
            read_only: true,
            ..Options::default()
        },
    )
    .unwrap();
    let repeated = fixture.prepare(&prepared.next_session, "tools.inspect();drawing.pause()");
    assert_eq!(repeated.source, prepared.source);
}

#[test]
fn nested_packages_and_local_modules_keep_the_original_project_root() {
    let fixture = Fixture::new();
    let shared = fixture.package(
        "shared",
        &[("lib.zuse", "fn inspect(){timer.status()}")],
        "{}",
        "{}",
    );
    let dependencies = format!(
        "{{\"shared\":{{\"git\":{},\"rev\":\"v0.1.0\"}}}}",
        json::quote(shared.to_str().unwrap())
    );
    let outer = fixture.package(
        "outer",
        &[(
            "lib.zuse",
            "import(path=\"pkg:shared\",as=\"shared\"); fn inspect(){shared.inspect()}",
        )],
        "{}",
        &dependencies,
    );
    fixture.add(&outer);
    fs::create_dir(fixture.project.join("nested")).unwrap();
    fs::write(
        fixture.project.join("nested/zuse.json"),
        "this nested manifest must never be read",
    )
    .unwrap();
    fs::write(
        fixture.project.join("nested/start.zuse"),
        "import(path=\"pkg:outer\",as=\"outer\"); fn inspect(){outer.inspect()}",
    )
    .unwrap();
    let prepared = fixture.prepare(
        &ModuleSession::default(),
        r#"import(path="nested/start.zuse",as="start");start.inspect()"#,
    );
    assert_eq!(prepared.source, "timer.status();\n");
    // Project discovery still walks up from an entry subdirectory when needed.
    let entry = fixture.project.join("scripts");
    fs::create_dir(&entry).unwrap();
    let prepared = ModuleSession::default()
        .prepare_with_base(
            r#"import(path="pkg:outer",as="outer");outer.inspect()"#,
            Options::default(),
            &entry,
        )
        .unwrap();
    assert_eq!(prepared.source, "timer.status();\n");
}

#[test]
fn ordinary_imports_do_not_require_or_parse_a_package_manifest() {
    let fixture = Fixture::new();
    fs::write(fixture.project.join("zuse.json"), "malformed package data").unwrap();
    fs::write(
        fixture.project.join("local.zuse"),
        "fn inspect(){timer.status()}",
    )
    .unwrap();
    let prepared = fixture.prepare(
        &ModuleSession::default(),
        r#"
        import(path="local.zuse",as="local");
        import(source="fn pause(){wait(ms=1)}",as="inline");
        local.inspect();inline.pause()
    "#,
    );
    assert_eq!(prepared.source, "timer.status();\nwait(ms=1);\n");
}

#[test]
fn package_imports_keep_declaration_only_and_transactional_rules() {
    let fixture = Fixture::new();
    let tools = fixture.package(
        "tools",
        &[("lib.zuse", "fn inspect(){timer.status()}")],
        "{}",
        "{}",
    );
    fixture.add(&tools);
    let lock_path = fixture.project.join("zuse.lock.json");
    let original_lock = fs::read(&lock_path).unwrap();
    let actions = fixture.package(
        "actions",
        &[(
            "lib.zuse",
            "fn pending(){timer.status()} eval(source=\"timer.start()\")",
        )],
        "{}",
        "{}",
    );
    assert!(
        packages::run_at(
            &fixture.project,
            &["add", actions.to_str().unwrap(), "--rev", "v0.1.0"]
        )
        .is_err()
    );
    assert_eq!(fs::read(&lock_path).unwrap(), original_lock);
    let session = fixture
        .prepare(&ModuleSession::default(), "fn retained(){timer.status()}")
        .next_session;
    let error = session.prepare_with_base(
        r#"import(path="pkg:tools",as="tools");import(source="eval(source='timer.start()')",as="actions")"#,
        Options::default(), &fixture.project,
    ).unwrap_err();
    assert!(error.contains("only function declarations"), "{error}");
    assert_eq!(session.functions(), ["retained"]);
}

#[test]
fn package_imports_fail_for_missing_locks_unknown_exports_and_modified_cache() {
    let fixture = Fixture::new();
    let repository = fixture.package(
        "tools",
        &[("lib.zuse", "fn inspect(){timer.status()}")],
        "{}",
        "{}",
    );
    fixture.add(&repository);
    let source = r#"import(path="pkg:tools",as="tools")"#;
    let session = ModuleSession::default();
    let entry = packages::resolve_import(&fixture.project, "pkg:tools").unwrap();
    for invalid in ["pkg:missing", "pkg:tools/private", "pkg:tools/../lib.zuse"] {
        let error = session
            .prepare_with_base(
                &format!("import(path={})", json::quote(invalid)),
                Options::default(),
                &fixture.project,
            )
            .unwrap_err();
        assert!(!error.is_empty());
    }
    let lock_path = fixture.project.join("zuse.lock.json");
    let lock = fs::read(&lock_path).unwrap();
    fs::remove_file(&lock_path).unwrap();
    assert!(
        session
            .prepare_with_base(source, Options::default(), &fixture.project)
            .is_err()
    );
    fs::write(&lock_path, lock).unwrap();
    fs::write(entry, "fn inspect(){timer.start()}").unwrap();
    assert!(
        session
            .prepare_with_base(source, Options::default(), &fixture.project)
            .is_err()
    );
    assert!(session.functions().is_empty());
}

#[test]
fn package_functions_share_program_budgets_and_scoped_compilation() {
    let fixture = Fixture::new();
    let repository = fixture.package(
        "bounded",
        &[
            ("lib.zuse", "fn pause(){wait(ms=40)}"),
            ("scoped.zuse", "fn inspect(m){m.position()}"),
        ],
        r#"{"scoped":"scoped.zuse"}"#,
        "{}",
    );
    fixture.add(&repository);
    let source = r#"import(path="pkg:bounded",as="bounded");bounded.pause();bounded.pause()"#;
    let session = ModuleSession::default();
    let prepared = fixture.prepare(&session, source);
    assert!(
        runtime::check(
            &prepared.source,
            Options {
                max_wait_ms: 79,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        session
            .prepare_with_base(
                source,
                Options {
                    max_steps: 1,
                    ..Options::default()
                },
                &fixture.project
            )
            .is_err()
    );
    let prepared = fixture.prepare(
        &session,
        r#"import(path="pkg:bounded/scoped",as="helper");let m = user_mouse();helper.inspect(m)"#,
    );
    assert_eq!(prepared.mode, ProgramMode::Scoped);
    prepared.compile_scoped(Options::default()).unwrap();
}
