#![cfg(feature = "std")]
use zuse::runtime::{self, Options};

#[test]
fn background_mode_requires_an_explicit_boolean_and_write_permission() {
    for value in ["true", "false"] {
        let source = format!("browser.background(enabled={value})");
        assert!(runtime::check(&source, Options::default()).is_ok());
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
    }
    for source in [
        "browser.background()",
        "browser.background(enabled=1)",
        "browser.background(enabled=\"true\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
}
