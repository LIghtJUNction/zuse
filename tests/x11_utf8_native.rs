#![cfg(all(feature = "std", target_os = "linux"))]

/// An isolated process driver for native_x11_utf8.py. The Python fixture owns
/// every display/bus/window; this ignored test never uses a user's desktop.
#[test]
#[ignore = "run python3 tests/native_x11_utf8.py"]
fn input() {
    assert_eq!(std::env::var("ZUSE_X11_UTF8_FIXTURE").as_deref(), Ok("1"));
    let target = std::env::var("ZUSE_X11_UTF8_TARGET").unwrap();
    let value = std::env::var("ZUSE_X11_UTF8_VALUE").unwrap();
    let action = std::env::var("ZUSE_X11_UTF8_ACTION").unwrap();
    let output = std::env::var("ZUSE_X11_UTF8_RESULT").unwrap();
    let result = (|| -> Result<(), String> {
        let mut desktop = zuse::platform::Desktop::connect()?;
        if desktop.backend() != "x11" {
            return Err("fixture did not use X11".into());
        }
        match action.as_str() {
            "type" => {
                desktop.focus(&target)?;
                if std::env::var("ZUSE_X11_UTF8_BARRIER").as_deref() == Ok("1") {
                    use std::io::Write;
                    println!("PROBE_READY");
                    std::io::stdout().flush().unwrap();
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).unwrap();
                    assert_eq!(line.trim(), "GO");
                }
                desktop.type_text(&value)
            }
            "background" => desktop.type_into(&target, &value),
            "key" => {
                desktop.focus(&target)?;
                desktop.key(&value)
            }
            _ => Err("unknown fixture action".into()),
        }
    })();
    std::fs::write(
        output,
        match result {
            Ok(()) => "ok".to_owned(),
            Err(error) => format!("error: {error}"),
        },
    )
    .unwrap();
}
