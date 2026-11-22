#![cfg(feature = "std")]
use zuse::{
    json,
    runtime::{self, Options, Runtime},
};

#[test]
fn feedback_keys_preflight_the_complete_program_types_permissions_and_wait_budget() {
    let source = "browser.control_keys(allowed_keys=\"Up,Down,Left,Right\",policy=\"({keys:[],done:true})\",timeout=500)";
    assert!(runtime::check(source, Options::default()).is_ok());
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 499,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 500,
                ..Options::default()
            }
        )
        .is_ok()
    );
    assert!(
        runtime::check(
            source,
            Options {
                read_only: true,
                ..Options::default()
            }
        )
        .is_err()
    );
    let receipt = Runtime::new()
        .execute(
            source,
            Options {
                dry_run: true,
                ..Options::default()
            },
        )
        .unwrap();
    assert!(!runtime::failed(&receipt));
    for invalid in [
        source.replace("Up,Down,Left,Right", "Up,ArrowUp"),
        source.replace("Up,Down,Left,Right", "Up,Down,Left,Right,Up"),
        source.replace("Up,Down,Left,Right", "Control+Up"),
        source.replace("Up,Down,Left,Right", ""),
        source.replace("({keys:[],done:true})", " "),
        source.replace("({keys:[],done:true})", &"a".repeat(16385)),
        source.replace("timeout=500", "timeout=0"),
        source.replace("timeout=500", "timeout=10001"),
        source.replace("timeout=500", "timeout=\"500\""),
        source.replace(",timeout=500", ""),
        source.replace("timeout=500", "timeout=500,unexpected=true"),
        source.replace("allowed_keys=\"Up,Down,Left,Right\"", "allowed_keys=true"),
        source.replace("policy=\"({keys:[],done:true})\"", "policy=12"),
    ] {
        assert!(
            runtime::check(&invalid, Options::default()).is_err(),
            "{invalid}"
        );
    }
    let repeated = format!("{source};{source}");
    assert!(
        runtime::check(
            &repeated,
            Options {
                max_wait_ms: 999,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            &repeated,
            Options {
                max_wait_ms: 1000,
                ..Options::default()
            }
        )
        .is_ok()
    );
    assert!(runtime::schema().contains("\"name\":\"browser.control_keys\",\"read_only\":false"));

    // Offline checking parses the DSL, not arbitrary JavaScript. Actual JS is
    // compiled and evaluated under side-effect checks before the first key.
    let syntax_deferred = source.replace("({keys:[],done:true})", "this is not javascript");
    assert!(runtime::check(&syntax_deferred, Options::default()).is_ok());
}

#[test]
fn feedback_canvas_binding_is_optional_named_and_does_not_weaken_policy_bounds() {
    for name in ["parking".to_owned(), "x".repeat(64)] {
        let source = format!(
            "browser.control_keys(allowed_keys=\"Right\",policy=\"({{keys:[],done:state.available}})\",timeout=500,canvas={})",
            json::quote(&name)
        );
        assert!(runtime::check(&source, Options::default()).is_ok());
    }
    for name in [
        "".to_owned(),
        "x".repeat(65),
        "has space".into(),
        "../canvas".into(),
        "中文".into(),
        "canvas\0".into(),
    ] {
        let source = format!(
            "browser.control_keys(allowed_keys=\"Right\",policy=\"({{keys:[],done:true}})\",timeout=500,canvas={})",
            json::quote(&name)
        );
        assert!(
            runtime::check(&source, Options::default()).is_err(),
            "{source}"
        );
    }
    let full = format!(
        "browser.control_keys(allowed_keys=\"Right\",policy={},timeout=500,canvas=\"parking\")",
        json::quote(&"a".repeat(16384))
    );
    assert!(runtime::check(&full, Options::default()).is_ok());
}
