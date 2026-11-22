#![cfg(feature = "std")]
use zuse::runtime::{self, Options};

#[test]
fn browser_hold_keys_preflights_the_complete_bounded_direction_set() {
    for keys in [
        "",
        "Up,",
        ",Right",
        "Up,Up",
        "Up,ArrowUp",
        "Up,Down",
        "Right,Left",
        "Down,Up",
        "Left,Right",
        "Up,Right,Down",
        "Up,Control",
        "a",
        "Control+Up",
        "Up;Right",
    ] {
        let source = format!("browser.hold_keys(keys=\"{keys}\",duration=500)");
        assert!(
            runtime::check(&source, Options::default()).is_err(),
            "{source}"
        );
    }
    for keys in [
        "ArrowUp,ArrowRight",
        " Left , Up ",
        "Down,Right",
        "Left,Down",
        "ArrowUp",
    ] {
        let source = format!("browser.hold_keys(keys=\"{keys}\",duration=500)");
        assert!(
            runtime::check(&source, Options::default()).is_ok(),
            "{source}"
        );
    }
    for source in [
        "browser.hold_keys(keys=\"Up,Right\",duration=0)",
        "browser.hold_keys(keys=\"Up,Right\",duration=10001)",
        "browser.hold_keys(keys=\"Up,Right\")",
        "browser.hold(key=\"Up,Right\",duration=500)",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    let source = "browser.hold_keys(keys=\"Up,Right\",duration=500)";
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
    assert!(runtime::schema().contains("\"name\":\"browser.hold_keys\",\"read_only\":false"));
}
