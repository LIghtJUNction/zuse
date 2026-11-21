#![cfg(feature = "std")]
use zuse::runtime::{self, Options, Runtime};

#[test]
fn key_holds_require_direction_duration_and_input_permission() {
    for name in ["keyboard.hold", "browser.hold"] {
        for args in [
            "key=\"Up\"",
            "key=\"Up\",duration=0",
            "key=\"Up\",duration=10001",
            "key=\"Ctrl+Up\",duration=100",
        ] {
            assert!(runtime::check(&format!("{name}({args})"), Options::default()).is_err());
        }
        let source = format!("{name}(key=\"ArrowLeft\",duration=100)");
        assert!(runtime::check(&source, Options::default()).is_ok());
        assert!(
            runtime::check(
                &source,
                Options {
                    read_only: true,
                    dry_run: true,
                    ..Options::default()
                }
            )
            .is_err()
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
    }
}

#[test]
fn key_holds_share_the_aggregate_program_wait_budget() {
    let source = "keyboard.hold(key=\"Up\",duration=70);browser.hold(key=\"Right\",duration=40)";
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 109,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 110,
                ..Options::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn native_shared_keyboard_is_explicit_typed_and_keeps_normal_preflight() {
    for key in ["Up", "ArrowRight", "slash", "/", "period", ".", "m", "N"] {
        let source = format!("keyboard.hold(key=\"{key}\",duration=80,allow_other_keys=true)");
        assert!(runtime::check(&source, Options::default()).is_ok(), "{key}");
        assert!(
            runtime::check(
                &source,
                Options {
                    read_only: true,
                    ..Options::default()
                }
            )
            .is_err()
        );
        assert!(
            runtime::check(
                &source,
                Options {
                    max_wait_ms: 79,
                    ..Options::default()
                }
            )
            .is_err()
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
    }
    for args in [
        "key=\"Up\",duration=80,allow_other_keys=1",
        "key=\"Up\",duration=80,allow_other_keys=\"true\"",
        "key=\"Control_L\",duration=80,allow_other_keys=true",
        "key=\"Shift_L\",duration=80,allow_other_keys=true",
        "key=\"Up+slash\",duration=80,allow_other_keys=true",
        "key=\"Num_Lock\",duration=80,allow_other_keys=true",
    ] {
        assert!(
            runtime::check(&format!("keyboard.hold({args})"), Options::default()).is_err(),
            "{args}"
        );
    }
    assert!(
        runtime::check(
            "keyboard.hold(key=\"Up\",duration=80,allow_other_keys=false)",
            Options::default()
        )
        .is_ok()
    );
    assert!(
        runtime::check(
            "browser.hold(key=\"slash\",duration=80)",
            Options::default()
        )
        .is_err()
    );
    assert!(
        runtime::check(
            "browser.hold(key=\"Up\",duration=80,allow_other_keys=true)",
            Options::default()
        )
        .is_err()
    );
}

#[test]
fn key_hold_schema_marks_input_commands_and_required_duration() {
    zuse::json::with_value(&runtime::schema(), |root| {
        for name in ["keyboard.hold", "browser.hold"] {
            let command = root
                .get("commands")
                .unwrap()
                .elements()
                .find(|command| {
                    command
                        .get("name")
                        .and_then(|value| value.string())
                        .is_some_and(|value| value.matches(name))
                })
                .unwrap();
            assert_eq!(command.get("read_only").unwrap().boolean(), Some(false));
        }
        Ok(())
    })
    .unwrap();
}
