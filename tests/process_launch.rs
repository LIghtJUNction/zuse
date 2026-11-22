#![cfg(feature = "std")]

use zuse::{
    integrations, json,
    observation::Cancellation,
    runtime::{self, Options, Runtime},
};

#[test]
fn independent_launch_is_explicit_mutating_and_preflighted() {
    let valid = "process.launch(program=\"chatgpt\",args=\"--force-renderer-accessibility=complete --ozone-platform=wayland\")";
    assert!(runtime::check(valid, Options::default()).is_ok());
    assert!(runtime::check("process.launch(program=\"chatgpt\")", Options::default()).is_ok());
    assert!(
        runtime::check(
            valid,
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
            valid,
            Options {
                dry_run: true,
                ..Options::default()
            },
        )
        .unwrap();
    assert!(!runtime::failed(&result));
    for source in [
        "process.launch()",
        "process.launch(program=\"\")",
        "process.launch(program=true)",
        "process.launch(program=\"x\\u0000\")",
        "process.launch(program=\"x\",args=\"a\\u0000b\")",
        "process.launch(program=\"x\",args=\"'unterminated\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    for (program, args, valid) in [
        ("x".repeat(4096), String::new(), true),
        ("x".repeat(4097), String::new(), false),
        ("x".into(), "a".repeat(65536), true),
        ("x".into(), "a".repeat(65537), false),
        ("x".into(), "a ".repeat(128), true),
        ("x".into(), "a ".repeat(129), false),
    ] {
        let source = format!(
            "process.launch(program={},args={})",
            json::quote(&program),
            json::quote(&args)
        );
        assert_eq!(runtime::check(&source, Options::default()).is_ok(), valid);
    }
}

#[test]
fn cancelled_or_denied_launch_never_reaches_the_executable() {
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(
        integrations::launch("zuse-launch-must-not-exist", "", &cancelled)
            .unwrap_err()
            .starts_with("cancelled:")
    );
    #[cfg(unix)]
    {
        let denied = Cancellation::with_dispatch_guard(|| Err("launch permission denied".into()));
        assert!(
            integrations::launch("zuse-launch-must-not-exist", "", &denied)
                .unwrap_err()
                .contains("launch permission denied")
        );
    }
}

#[test]
fn launch_schema_does_not_change_owned_process_start() {
    json::with_value(&runtime::schema(), |root| {
        let commands = root.get("commands").unwrap();
        for name in ["process.start", "process.launch"] {
            let command = commands
                .elements()
                .find(|command| {
                    command
                        .get("name")
                        .and_then(|value| value.string())
                        .is_some_and(|value| value.matches(name))
                })
                .unwrap();
            assert_eq!(command.get("read_only").unwrap().boolean(), Some(false));
            let parameters = command.get("parameters").unwrap();
            assert_eq!(parameters.elements().count(), 2);
            assert!(parameters.elements().any(|value| {
                value
                    .get("name")
                    .and_then(|value| value.string())
                    .is_some_and(|value| value.matches("program"))
                    && value.get("required").and_then(|value| value.boolean()) == Some(true)
            }));
        }
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn getpgid(pid: i32) -> i32;
}

#[cfg(unix)]
struct FixtureProcess(i32);
#[cfg(unix)]
impl FixtureProcess {
    fn alive(&self) -> bool {
        self.0 > 0 && unsafe { kill(self.0, 0) } == 0
    }
    fn cleanup(&mut self) -> bool {
        if self.0 == 0 {
            return true;
        }
        // This guard receives only the PID returned by our own test launch.
        unsafe {
            kill(-self.0, 9);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while self.alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let reaped = !self.alive();
        if reaped {
            self.0 = 0;
        }
        reaped
    }
}
#[cfg(unix)]
impl Drop for FixtureProcess {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(unix)]
fn pid_from_result(result: &str) -> i32 {
    assert!(!runtime::failed(result), "{result}");
    json::with_value(result, |root| {
        let process = root
            .get("results")
            .unwrap()
            .elements()
            .next()
            .unwrap()
            .get("value")
            .unwrap();
        Ok(i32::try_from(process.get("pid").unwrap().integer().unwrap()).unwrap())
    })
    .unwrap()
}

#[cfg(unix)]
fn fixture_args() -> &'static str {
    "--ignored --exact independent_application_fixture --nocapture"
}

#[cfg(unix)]
#[test]
#[ignore = "private child entry point used by the Runtime lifecycle test"]
fn independent_application_fixture() {
    // No shell or external helper: run this test binary as the user application.
    // The parent's cleanup guard normally kills it immediately after checks.
    std::thread::sleep(std::time::Duration::from_secs(30));
}

#[cfg(unix)]
#[test]
fn launched_application_survives_runtime_drop_and_has_null_streams() {
    let executable = std::env::current_exe().unwrap();
    let mut runtime = Runtime::new();
    let result = runtime
        .execute(
            &format!(
                "process.launch(program={},args={})",
                json::quote(executable.to_str().unwrap()),
                json::quote(fixture_args()),
            ),
            Options::default(),
        )
        .unwrap();
    let mut fixture = FixtureProcess(pid_from_result(&result));
    json::with_value(&result, |root| {
        let value = root
            .get("results")
            .unwrap()
            .elements()
            .next()
            .unwrap()
            .get("value")
            .unwrap();
        assert_eq!(json::text(value.get("lifecycle").unwrap())?, "independent");
        assert_eq!(json::text(value.get("stdio").unwrap())?, "null");
        assert!(
            value.get("id").is_none(),
            "independent launches must not claim a manager ID"
        );
        assert_eq!(
            value.get("process_group").unwrap().integer().unwrap(),
            i64::from(fixture.0)
        );
        Ok(())
    })
    .unwrap();
    let listed = runtime
        .execute("process.list()", Options::default())
        .unwrap();
    json::with_value(&listed, |root| {
        let value = root
            .get("results")
            .unwrap()
            .elements()
            .next()
            .unwrap()
            .get("value")
            .unwrap();
        assert_eq!(value.elements().count(), 0);
        Ok(())
    })
    .unwrap();
    drop(runtime);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        fixture.alive(),
        "Runtime drop terminated the independent application"
    );
    assert_eq!(unsafe { getpgid(fixture.0) }, fixture.0);
    #[cfg(target_os = "linux")]
    for descriptor in 0..=2 {
        assert_eq!(
            std::fs::read_link(format!("/proc/{}/fd/{descriptor}", fixture.0)).unwrap(),
            std::path::Path::new("/dev/null"),
        );
    }
    assert!(
        fixture.cleanup(),
        "fixture child was not reaped after cleanup"
    );
}

#[cfg(unix)]
#[test]
fn process_start_remains_owned_and_reaped_on_runtime_drop() {
    let executable = std::env::current_exe().unwrap();
    let mut runtime = Runtime::new();
    let result = runtime
        .execute(
            &format!(
                "process.start(program={},args={})",
                json::quote(executable.to_str().unwrap()),
                json::quote(fixture_args()),
            ),
            Options::default(),
        )
        .unwrap();
    let mut fixture = FixtureProcess(pid_from_result(&result));
    assert!(fixture.alive());
    drop(runtime);
    assert!(
        !fixture.alive(),
        "process.start lost its owned cleanup behavior"
    );
    fixture.0 = 0;
}
