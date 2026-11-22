#![cfg(feature = "std")]
use zuse::{
    json,
    runtime::{self, Options},
};

#[test]
fn observable_hold_preflights_keys_condition_and_total_timeout_budget() {
    let source = "browser.hold_until(keys=\"Up,Right\",condition=\"document.body.dataset.ready === 'yes'\",timeout=500)";
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
    for invalid in [
        source.replace("Up,Right", "Up,Down"),
        source.replace("Up,Right", "Up,ArrowUp"),
        source.replace("Up,Right", "Up,Right,Left"),
        source.replace("Up,Right", "Control+Up"),
        source.replace("document.body.dataset.ready === 'yes'", " "),
        source.replace("timeout=500", "timeout=0"),
        source.replace("timeout=500", "timeout=10001"),
        source.replace(",timeout=500", ""),
        source.replace("document.body.dataset.ready === 'yes'", &"a".repeat(16_385)),
    ] {
        assert!(runtime::check(&invalid, Options::default()).is_err());
    }
    let paired = format!("{source}\n{source}");
    assert!(
        runtime::check(
            &paired,
            Options {
                max_wait_ms: 999,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(runtime::schema().contains("\"name\":\"browser.hold_until\",\"read_only\":false"));
}

#[test]
fn canvas_hold_preflight_uses_the_observer_name_and_envelope_limits() {
    let condition = "state.available && state.latest.some(draw => draw.age_ms < 100)";
    for name in ["parking".to_owned(), "x".repeat(64)] {
        let source = format!(
            "browser.hold_until(keys=\"Up\",canvas={},condition={},timeout=500)",
            json::quote(&name),
            json::quote(condition),
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
            "browser.hold_until(keys=\"Up\",canvas={},condition={},timeout=500)",
            json::quote(&name),
            json::quote(condition),
        );
        assert!(runtime::check(&source, Options::default()).is_err());
    }
    let full_condition = "x".repeat(16_384);
    let plain = format!(
        "browser.hold_until(keys=\"Up\",condition={},timeout=500)",
        json::quote(&full_condition),
    );
    assert!(runtime::check(&plain, Options::default()).is_ok());
    let bound = plain.replace("keys=\"Up\"", "keys=\"Up\",canvas=\"parking\"");
    assert!(
        runtime::check(&bound, Options::default())
            .unwrap_err()
            .contains("observer binding")
    );
}
