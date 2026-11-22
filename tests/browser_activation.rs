#![cfg(feature = "std")]
use zuse::runtime::{self, Options, Runtime};

#[test]
fn activation_is_explicit_argument_free_and_requires_write_permission() {
    assert!(runtime::check("browser.activate()", Options::default()).is_ok());
    assert!(
        runtime::check(
            "browser.activate()",
            Options {
                read_only: true,
                ..Options::default()
            }
        )
        .is_err()
    );
    for source in [
        "browser.activate(id=\"another-tab\")",
        "browser.activate(enabled=true)",
        "browser.activate(timeout=1000)",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    assert!(runtime::schema().contains(r#""name":"browser.activate","read_only":false"#));
    let error = Runtime::new()
        .execute(
            "browser.key(key=\"Enter\");browser.activate(id=\"another-tab\")",
            Options::default(),
        )
        .unwrap_err();
    assert!(
        error.contains("id"),
        "the whole program must fail before any browser dispatch: {error}"
    );
}
