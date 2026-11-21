#![cfg(feature = "std")]

use zuse::{
    json,
    pointer::MoveOptions,
    runtime::{self, Options, Runtime},
    target::TargetQuery,
};

fn assert_no_owned_mouse(runtime: &mut Runtime) {
    let output = runtime
        .execute(
            "mouse.position()",
            Options {
                read_only: true,
                ..Options::default()
            },
        )
        .unwrap();
    assert!(runtime::failed(&output), "{output}");
    assert!(output.contains("runtime mouse is unavailable"), "{output}");
}

#[test]
fn observation_and_release_do_not_create_or_borrow_a_pointer() {
    let mut runtime = Runtime::new();
    assert_no_owned_mouse(&mut runtime);
    assert_no_owned_mouse(&mut runtime);
    let released = runtime
        .execute("mouse.release();mouse.release()", Options::default())
        .unwrap();
    assert!(!runtime::failed(&released), "{released}");
    assert_no_owned_mouse(&mut runtime);
}

#[test]
fn mouse_modes_and_styles_are_validated_without_native_side_effects() {
    let mut runtime = Runtime::new();
    for source in [
        "mouse.use()",
        r#"mouse.use(kind="independent",appearance="glow")"#,
        r#"mouse.use(kind="user")"#,
        r#"mouse.use(kind="user",appearance="system")"#,
        r##"mouse.use(kind="user",color="#FF884488",shape="ring",size=16,glow=0)"##,
        r##"mouse.use(name="agent one",color="#00aaff",shape="crosshair",size=96,glow=1)"##,
        r#"mouse.use(shape="dot");mouse.move(x=2,y=3,duration=0);mouse.release()"#,
    ] {
        let options = Options {
            dry_run: true,
            max_wait_ms: 0,
            ..Options::default()
        };
        assert!(runtime::check(source, options).is_ok(), "{source}");
        let result = runtime.execute(source, options).unwrap();
        assert!(!runtime::failed(&result), "{source}: {result}");
        assert_no_owned_mouse(&mut runtime);
    }
}

#[test]
fn invalid_mouse_selection_fails_whole_program_preflight() {
    let mut runtime = Runtime::new();
    for source in [
        r#"mouse.use(kind="shared")"#,
        r#"mouse.use(kind="")"#,
        r#"mouse.use(kind=false)"#,
        r#"mouse.use(appearance="hidden")"#,
        r#"mouse.use(appearance="system")"#,
        r#"mouse.use(kind="independent",appearance="system")"#,
        r##"mouse.use(kind="user",appearance="system",color="#00ffff")"##,
        r#"mouse.use(kind="user",appearance="system",shape="arrow")"#,
        r#"mouse.use(kind="user",appearance="system",size=32)"#,
        r#"mouse.use(kind="user",appearance="system",glow=0)"#,
        r#"mouse.use(color="cyan")"#,
        r##"mouse.use(color="#0011223")"##,
        r#"mouse.use(shape="triangle")"#,
        "mouse.use(size=15)",
        "mouse.use(size=97)",
        "mouse.use(size=32.5)",
        "mouse.use(glow=-0.1)",
        "mouse.use(glow=1.1)",
        "mouse.use(glow=1e309)",
        r#"mouse.use(name="")"#,
        r#"mouse.use(name="bad\u0000name")"#,
        r#"mouse.use(unknown=true)"#,
        r#"mouse.release(kind="user")"#,
        "mouse.move(x=16777217,y=0)",
        "mouse.move(x=0,y=-16777217)",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
        // A later invalid step is a preflight error rather than a partial
        // execution result. The preceding operation has no native effects.
        let program = format!("wait(ms=0);{source}");
        assert!(runtime.execute(&program, Options::default()).is_err());
        assert_no_owned_mouse(&mut runtime);
    }
    let long_name = format!("mouse.use(name={})", json::quote(&"x".repeat(129)));
    assert!(runtime::check(&long_name, Options::default()).is_err());
}

#[test]
fn selection_and_release_obey_read_only_and_program_budgets() {
    for source in [
        "mouse.use()",
        r#"mouse.use(kind="user",appearance="system")"#,
        "mouse.release()",
    ] {
        for dry_run in [false, true] {
            let options = Options {
                dry_run,
                read_only: true,
                ..Options::default()
            };
            let error = Runtime::new().execute(source, options).unwrap_err();
            assert!(error.contains("read-only"), "{source}: {error}");
        }
    }
    assert!(
        runtime::check(
            "mouse.use();mouse.release()",
            Options {
                max_steps: 1,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            "mouse.use();mouse.move(x=2,y=3,duration=20);mouse.release();wait(ms=10)",
            Options {
                max_wait_ms: 29,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            "mouse.use();mouse.move(x=2,y=3,duration=20);mouse.release();wait(ms=10)",
            Options {
                max_wait_ms: 30,
                ..Options::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn typed_target_preflight_does_not_create_a_mouse() {
    let mut runtime = Runtime::new();
    assert!(
        runtime
            .move_to(&TargetQuery::text(""), &MoveOptions::default())
            .is_err()
    );
    let invalid = MoveOptions {
        duration_ms: Some(60_001),
        ..MoveOptions::default()
    };
    let mut callback_called = false;
    assert!(
        runtime
            .move_to_with(&TargetQuery::button("Save"), &invalid, |_, _, to| {
                callback_called = true;
                to
            })
            .is_err()
    );
    assert!(!callback_called);
    assert_no_owned_mouse(&mut runtime);
}

#[test]
fn schema_exposes_mouse_ownership_and_style_controls() {
    json::with_value(&runtime::schema(), |root| {
        let commands = root.get("commands").unwrap();
        for name in ["mouse.use", "mouse.release", "mouse.position"] {
            let command = commands
                .elements()
                .find(|entry| {
                    entry
                        .get("name")
                        .and_then(|value| value.string())
                        .is_some_and(|value| value.matches(name))
                })
                .unwrap();
            assert_eq!(
                command.get("read_only").unwrap().boolean(),
                Some(name == "mouse.position")
            );
            if name == "mouse.use" {
                let parameters = command.get("parameters").unwrap();
                for expected in [
                    "kind",
                    "appearance",
                    "color",
                    "shape",
                    "size",
                    "glow",
                    "name",
                ] {
                    assert!(parameters.elements().any(|entry| {
                        entry
                            .get("name")
                            .and_then(|value| value.string())
                            .is_some_and(|value| value.matches(expected))
                    }));
                }
            }
        }
        Ok(())
    })
    .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn legacy_mouse_actions_never_fall_back_to_a_user_pointer() {
    // Force an unsupported independent-pointer backend in child processes.
    // A correct runtime rejects this before opening any native desktop.
    for source in [
        "mouse.use()",
        r#"mouse.use(kind="user")"#,
        "mouse.move(x=1,y=1)",
        "mouse.circle(x=10,y=10,radius=2,duration=1,steps=1)",
        "mouse.click()",
        "mouse.down()",
        "mouse.up()",
        "mouse.scroll(dy=1)",
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_zuse"))
            .args(["eval", source])
            .env("ZUSE_BACKEND", "wayland")
            .env("WAYLAND_DISPLAY", "zuse-does-not-exist")
            .env_remove("DISPLAY")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{source}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // An explicitly selected user marker is supported on Wayland, but it
        // must fail at this nonexistent private display instead of borrowing
        // another pointer or falling back to the host session.
        let expected = if source == r#"mouse.use(kind="user")"# {
            "cannot connect the operation marker to Wayland"
        } else {
            "no independent-pointer support"
        };
        assert!(
            stdout.contains(expected) || stderr.contains(expected),
            "{source}: {stdout}\n{stderr}"
        );
    }
}
