#![cfg(feature = "std")]

use zuse::{
    accessibility::controls::{self, Operation, Query},
    observation::Cancellation,
    runtime::{self, Options, Runtime},
};

fn query() -> Query<'static> {
    Query {
        scope: "0/1/0",
        expected_pid: 42,
        role: None,
        name: None,
        text: None,
        exact: true,
    }
}

#[test]
fn semantic_queries_require_a_bounded_window_scope_and_native_pid() {
    assert!(query().validate().is_ok());
    for scope in ["", "0", "0/1", "1/1/0", "0/-1/0", "0//0", "0/1000001/0"] {
        assert!(Query { scope, ..query() }.validate().is_err(), "{scope}");
    }
    let deepest = format!("0{}", "/0".repeat(64));
    assert!(
        Query {
            scope: &deepest,
            ..query()
        }
        .validate()
        .is_ok()
    );
    for scope in [format!("{deepest}/0"), format!("0/0/{}", "0".repeat(1021))] {
        assert!(
            Query {
                scope: &scope,
                ..query()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        Query {
            expected_pid: 0,
            ..query()
        }
        .validate()
        .is_err()
    );
    assert!(
        Query {
            name: Some(""),
            ..query()
        }
        .validate()
        .is_ok()
    );
    for invalid in [
        Query {
            role: Some(""),
            ..query()
        },
        Query {
            role: Some("button\0"),
            ..query()
        },
        Query {
            name: Some("name\0"),
            ..query()
        },
        Query {
            text: Some("text\0"),
            ..query()
        },
    ] {
        assert!(invalid.validate().is_err());
    }
    let cancellation = Cancellation::default();
    let error = controls::invoke(
        &query(),
        Operation::Action("press"),
        None,
        3000,
        &cancellation,
    )
    .unwrap_err();
    assert!(error.contains("role") || error.contains("name"), "{error}");
    let error = controls::wait(&query(), 3000, &cancellation).unwrap_err();
    assert!(error.contains("filter"), "{error}");
}

#[test]
fn semantic_commands_preflight_defaults_and_mutation_permissions() {
    for source in [
        "accessibility.find(scope=\"0/1/0\",expected_pid=42)",
        "accessibility.find(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",text=\"draft\",exact=false)",
        "accessibility.wait(scope=\"0/1/0\",expected_pid=42,text=\"ready\",timeout_ms=1)",
        "accessibility.wait(scope=\"0/1/0\",expected_pid=42,name=\"ready\",timeout_ms=10000)",
    ] {
        assert!(
            runtime::check(
                source,
                Options {
                    read_only: true,
                    ..Options::default()
                }
            )
            .is_ok(),
            "{source}"
        );
    }
    for source in [
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",action=\"press\",index=1,exact=false,timeout_ms=10000)",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"entry\",name=\"询问 ChatGPT\",operation=\"focus\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"聊天侧边栏选项\",operation=\"reveal\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_ok(),
            "{source}"
        );
        assert!(
            runtime::check(
                source,
                Options {
                    read_only: true,
                    dry_run: true,
                    ..Options::default()
                }
            )
            .is_err(),
            "{source}"
        );
        let result = Runtime::new()
            .execute(
                source,
                Options {
                    dry_run: true,
                    ..Options::default()
                },
            )
            .unwrap();
        assert!(!runtime::failed(&result), "{result}");
    }
}

#[test]
fn semantic_argument_errors_are_rejected_without_connecting_to_a_desktop() {
    for source in [
        "accessibility.find(expected_pid=42)",
        "accessibility.find(scope=\"0/1/0\")",
        "accessibility.find(scope=\"0/1\",expected_pid=42)",
        "accessibility.find(scope=\"0/1/0\",expected_pid=0)",
        "accessibility.find(scope=\"0/1/0\",expected_pid=-1)",
        "accessibility.find(scope=\"0/1/0\",expected_pid=4294967296)",
        "accessibility.find(scope=\"0/1/0\",expected_pid=\"42\")",
        "accessibility.find(scope=\"0/1/0\",expected_pid=42,exact=1)",
        "accessibility.find(scope=\"0/1/0\",expected_pid=42,name=\"\\u0000\")",
        "accessibility.wait(scope=\"0/1/0\",expected_pid=42)",
        "accessibility.wait(scope=\"0/1/0\",expected_pid=42,text=\"ready\",timeout_ms=0)",
        "accessibility.wait(scope=\"0/1/0\",expected_pid=42,text=\"ready\",timeout_ms=10001)",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,name=\"保存\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",operation=\"focus\",action=\"press\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",operation=\"reveal\",action=\"press\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",operation=\"reveal\",action=\"\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",operation=\"tap\")",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",index=-1)",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",timeout_ms=0)",
        "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"保存\",timeout_ms=10001)",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn waits_and_invokes_share_the_program_wait_budget() {
    for (source, budget) in [
        (
            "accessibility.wait(scope=\"0/1/0\",expected_pid=42,text=\"ready\",timeout_ms=4000);accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"Run\",timeout_ms=3000)",
            7000,
        ),
        (
            "accessibility.wait(scope=\"0/1/0\",expected_pid=42,text=\"ready\");accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"Run\")",
            6000,
        ),
        (
            "accessibility.invoke(scope=\"0/1/0\",expected_pid=42,role=\"button\",name=\"Run\",operation=\"reveal\",timeout_ms=700)",
            700,
        ),
    ] {
        assert!(
            runtime::check(
                source,
                Options {
                    max_wait_ms: budget - 1,
                    ..Options::default()
                }
            )
            .is_err()
        );
        assert!(
            runtime::check(
                source,
                Options {
                    max_wait_ms: budget,
                    ..Options::default()
                }
            )
            .is_ok()
        );
    }
    assert!(
        runtime::check(
            "accessibility.find(scope=\"0/1/0\",expected_pid=42)",
            Options {
                max_wait_ms: 0,
                read_only: true,
                ..Options::default()
            },
        )
        .is_ok()
    );
}

#[test]
fn cancelled_semantic_calls_do_not_open_native_interfaces() {
    let query = Query {
        role: Some("button"),
        name: Some("Save"),
        ..query()
    };
    let cancellation = Cancellation::default();
    cancellation.cancel();
    for result in [
        controls::find(&query, &cancellation),
        controls::invoke(
            &query,
            Operation::Action("press"),
            None,
            3000,
            &cancellation,
        ),
        controls::invoke(&query, Operation::Focus, None, 3000, &cancellation),
        controls::invoke(&query, Operation::Reveal, None, 3000, &cancellation),
        controls::wait(&query, 3000, &cancellation),
    ] {
        let error = result.unwrap_err();
        assert!(error.starts_with("cancelled:"), "{error}");
    }
}
