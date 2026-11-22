#![cfg(feature = "std")]

use zuse::{
    accessibility, json,
    runtime::{self, Options, Runtime},
};

#[test]
fn scoped_tree_is_read_only_and_keeps_legacy_limits() {
    let read_only = Options {
        read_only: true,
        ..Options::default()
    };
    for source in [
        "accessibility.tree()",
        "accessibility.tree(limit=256,depth=8)",
        "accessibility.tree(scope=\"0/1/0\",limit=4096,depth=64)",
        "accessibility.tree(scope=\"0\",limit=1,depth=1)",
    ] {
        assert!(runtime::check(source, read_only).is_ok(), "{source}");
    }
    let deepest = format!("0{}", "/0".repeat(64));
    assert!(
        runtime::check(
            &format!(
                "accessibility.tree(scope={},depth=64)",
                json::quote(&deepest)
            ),
            read_only,
        )
        .is_ok()
    );
    for source in [
        "accessibility.tree(scope=\"\")",
        "accessibility.tree(scope=\"0/-1\")",
        "accessibility.tree(scope=\"0//1\")",
        "accessibility.tree(scope=\"0/1000001\")",
        "accessibility.tree(scope=1)",
        "accessibility.tree(scope=\"0/1/0\",depth=0)",
        "accessibility.tree(scope=\"0/1/0\",depth=65)",
        "accessibility.tree(scope=\"0/1/0\",limit=0)",
        "accessibility.tree(scope=\"0/1/0\",limit=4097)",
    ] {
        assert!(runtime::check(source, read_only).is_err(), "{source}");
    }
    let too_deep = format!("0{}", "/0".repeat(65));
    let too_long = format!("0/{}", "0".repeat(1023));
    for scope in [too_deep, too_long] {
        assert!(
            runtime::check(
                &format!("accessibility.tree(scope={})", json::quote(&scope)),
                read_only,
            )
            .is_err()
        );
    }
}

#[test]
fn control_guards_are_optional_exact_and_preflighted() {
    for operation in ["accessibility.action", "accessibility.focus"] {
        for suffix in [
            "",
            ",expected_pid=42",
            ",expected_role=\"button\"",
            ",expected_name=\"\"",
            ",expected_name=\"选择 ChatGPT 模型\"",
            ",expected_pid=4294967295,expected_role=\"entry\",expected_name=\"询问 ChatGPT\"",
        ] {
            let source = format!("{operation}(path=\"0/1/0/3\"{suffix})");
            assert!(
                runtime::check(&source, Options::default()).is_ok(),
                "{source}"
            );
            let result = Runtime::new()
                .execute(
                    &source,
                    Options {
                        dry_run: true,
                        ..Options::default()
                    },
                )
                .unwrap();
            assert!(!runtime::failed(&result), "{result}");
            assert!(
                runtime::check(
                    &source,
                    Options {
                        read_only: true,
                        dry_run: true,
                        ..Options::default()
                    },
                )
                .is_err()
            );
        }
        for suffix in [
            ",expected_pid=0",
            ",expected_pid=-1",
            ",expected_pid=4294967296",
            ",expected_pid=\"42\"",
            ",expected_role=\"\"",
            ",expected_role=\"but\\u0000ton\"",
            ",expected_name=\"\\u0000\"",
            ",expected_name=true",
        ] {
            let source = format!("{operation}(path=\"0/1/0/3\"{suffix})");
            assert!(
                runtime::check(&source, Options::default()).is_err(),
                "{source}"
            );
        }
        assert!(
            runtime::check(&format!("{operation}(path=\"0/-1\")"), Options::default(),).is_err()
        );
    }
    assert!(
        runtime::check(
            "accessibility.action(path=\"0/1/0/3\",index=63)",
            Options::default(),
        )
        .is_ok()
    );
    for index in [-1, 64, 128] {
        assert!(
            runtime::check(
                &format!("accessibility.action(path=\"0/1/0/3\",index={index})"),
                Options::default(),
            )
            .is_err()
        );
    }
}

#[test]
fn guard_string_limits_count_utf8_bytes_and_library_calls_validate_first() {
    for (field, value) in [
        ("expected_role", format!("{}a", "中".repeat(1365))),
        ("expected_name", "🙂".repeat(16384)),
    ] {
        for operation in ["accessibility.action", "accessibility.focus"] {
            let source = format!(
                "{operation}(path=\"0/1/0/3\",{field}={})",
                json::quote(&value),
            );
            assert!(runtime::check(&source, Options::default()).is_ok());
            let too_long = format!("{value}x");
            let source = format!(
                "{operation}(path=\"0/1/0/3\",{field}={})",
                json::quote(&too_long),
            );
            assert!(runtime::check(&source, Options::default()).is_err());
        }
    }
    for (pid, role, name) in [
        (Some(0), None, None),
        (None, Some(""), None),
        (None, Some("button\0"), None),
        (None, None, Some("name\0")),
    ] {
        assert!(accessibility::action_checked("0/1/0", 0, pid, role, name).is_err());
        assert!(accessibility::focus_checked("0/1/0", pid, role, name).is_err());
    }
    assert!(accessibility::action_checked("0/1/0", 64, None, None, None).is_err());
}

#[test]
fn invalid_control_guards_stop_the_whole_program_before_side_effects() {
    let marker = std::env::temp_dir().join(format!(
        "zuse-control-preflight-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    for invalid in [
        "accessibility.tree(scope=\"0/-1\",depth=64)",
        "accessibility.action(path=\"0/1/0/3\",expected_pid=0)",
        "accessibility.focus(path=\"0/1/0/3\",expected_role=\"\")",
    ] {
        let source = format!(
            "process.start(program=\"sh\",args={});{invalid}",
            json::quote(&format!("-c 'touch {}'", marker.display())),
        );
        assert!(Runtime::new().execute(&source, Options::default()).is_err());
        assert!(!marker.exists());
    }
}

#[test]
fn schema_exposes_optional_identity_guards_and_scope() {
    json::with_value(&runtime::schema(), |root| {
        for (command, parameters) in [
            ("accessibility.tree", &["scope"][..]),
            (
                "accessibility.action",
                &["expected_pid", "expected_role", "expected_name"][..],
            ),
            (
                "accessibility.focus",
                &["expected_pid", "expected_role", "expected_name"][..],
            ),
        ] {
            let spec = root
                .get("commands")
                .unwrap()
                .elements()
                .find(|value| {
                    value
                        .get("name")
                        .and_then(|name| name.string())
                        .is_some_and(|name| name.matches(command))
                })
                .unwrap();
            for parameter in parameters {
                let field = spec
                    .get("parameters")
                    .unwrap()
                    .elements()
                    .find(|value| {
                        value
                            .get("name")
                            .and_then(|name| name.string())
                            .is_some_and(|name| name.matches(parameter))
                    })
                    .unwrap();
                assert_eq!(field.get("required").unwrap().boolean(), Some(false));
            }
        }
        Ok(())
    })
    .unwrap();
}
