#![cfg(feature = "std")]
use zuse::runtime::{self, Options};

#[test]
fn clipboard_schema_and_preflight_distinguish_reading_from_mutation() {
    let read_only = Options {
        read_only: true,
        ..Options::default()
    };
    assert!(runtime::check("clipboard.read()", read_only).is_ok());
    for source in [
        "clipboard.write(text=\"中文\\n🙂\")",
        "clipboard.write(text=\"\")",
        "clipboard.restore(path=\"/explicit/original.txt\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_ok(),
            "{source}"
        );
        assert!(runtime::check(source, read_only).is_err(), "{source}");
    }
    for source in [
        "clipboard.write()",
        "clipboard.restore()",
        "clipboard.restore(path=\"\")",
        "clipboard.read(primary=true)",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    let too_long = format!(
        "clipboard.write(text=\"{}\")",
        "a".repeat(zuse::clipboard::MAX_BYTES + 1)
    );
    assert!(runtime::check(&too_long, Options::default()).is_err());
    assert!(runtime::schema().contains("\"name\":\"clipboard.read\",\"read_only\":true"));
    assert!(runtime::schema().contains("\"name\":\"clipboard.restore\",\"read_only\":false"));
}
