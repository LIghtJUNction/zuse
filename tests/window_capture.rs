#![cfg(feature = "std")]

use zuse::runtime::{self, Options, Runtime};

#[test]
fn an_explicit_empty_window_never_becomes_a_screen_capture() {
    for source in [
        "window.capture(id=\"\",path=\"unused.png\")",
        "window.capture(id=\" \",path=\"unused.png\")",
        "screen.capture(window=\"\",path=\"unused.png\")",
        "screen.capture(window=\" \",path=\"unused.png\")",
    ] {
        let error = runtime::check(source, Options::default()).unwrap_err();
        assert!(error.contains("nonempty window"), "{error}");
        // Whole-program validation also rejects the request in a dry run.
        assert!(
            Runtime::new()
                .execute(
                    source,
                    Options {
                        dry_run: true,
                        ..Options::default()
                    }
                )
                .is_err()
        );
    }
}

#[test]
fn capture_requests_keep_explicit_screen_and_window_scopes() {
    for source in [
        "screen.capture(path=\"unused.png\")",
        "screen.capture(window=\"0x1\",path=\"unused.png\")",
        "window.capture(id=\"0x1\",path=\"unused.png\")",
        "window.capture(id=\"active\",path=\"unused.png\")",
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
}
