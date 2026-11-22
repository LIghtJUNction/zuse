#![cfg(feature = "std")]

use zuse::{
    accessibility,
    observation::Cancellation,
    runtime::{self, Options, Runtime},
};

#[test]
fn windows_requires_an_explicit_native_pid_and_is_read_only() {
    for source in [
        "accessibility.windows()",
        "accessibility.windows(expected_pid=0)",
        "accessibility.windows(expected_pid=-1)",
        "accessibility.windows(expected_pid=4294967296)",
        "accessibility.windows(expected_pid=\"42\")",
        "accessibility.windows(expected_pid=42,scope=\"0\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    for pid in [1, u32::MAX] {
        let source = format!("accessibility.windows(expected_pid={pid})");
        let options = Options {
            read_only: true,
            dry_run: true,
            max_wait_ms: 0,
            ..Options::default()
        };
        assert!(runtime::check(&source, options).is_ok());
        let result = Runtime::new().execute(&source, options).unwrap();
        assert!(!runtime::failed(&result), "{result}");
    }
}

#[test]
fn windows_validates_and_cancels_before_opening_a_native_bus() {
    let cancellation = Cancellation::default();
    assert!(
        accessibility::windows(0, &cancellation)
            .unwrap_err()
            .contains("positive u32")
    );
    cancellation.cancel();
    let error = accessibility::windows(42, &cancellation).unwrap_err();
    assert!(error.starts_with("cancelled:"), "{error}");
}
