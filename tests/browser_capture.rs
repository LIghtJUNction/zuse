#![cfg(feature = "std")]
use zuse::{
    browser::Browser,
    json,
    runtime::{self, Options, Runtime},
};

#[test]
fn capture_format_and_quality_are_preflighted_for_the_whole_program() {
    for source in [
        r#"browser.capture(path="capture.png")"#,
        r#"browser.capture(path="capture.png",format="png")"#,
        r#"browser.capture(path="capture.jpg",format="jpeg")"#,
        r#"browser.capture(path="capture.jpg",format="jpeg",quality=0)"#,
        r#"browser.capture(path="capture.jpg",format="jpeg",quality=80)"#,
        r#"browser.capture(path="capture.jpg",format="jpeg",quality=100)"#,
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
    for source in [
        r#"browser.capture(path="")"#,
        r#"browser.capture(path="capture.jpg",format="jpg")"#,
        r#"browser.capture(path="capture.webp",format="webp")"#,
        r#"browser.capture(path="capture.jpg",format="JPEG")"#,
        r#"browser.capture(path="capture.jpg",quality=80)"#,
        r#"browser.capture(path="capture.png",format="png",quality=80)"#,
        r#"browser.capture(path="capture.jpg",format="jpeg",quality=-1)"#,
        r#"browser.capture(path="capture.jpg",format="jpeg",quality=101)"#,
        r#"browser.capture(path="capture.jpg",format="jpeg",quality=80.5)"#,
        r#"browser.capture(path="capture.jpg",format="jpeg",quality="80")"#,
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    // There is no connected browser: dispatching the first command would fail
    // for that reason. The later option error must reject the entire program.
    let error = Runtime::new().execute(
        r#"browser.key(key="Enter");browser.capture(path="capture.jpg",format="jpeg",quality=101)"#,
        Options::default(),
    ).unwrap_err();
    assert!(error.contains("quality must be 0..100"), "{error}");
}

#[test]
#[ignore = "requires installed sandboxed Chromium; uses an owned temporary profile"]
fn native_jpeg_capture_preserves_full_viewport_dimensions() {
    let directory = std::env::temp_dir().join(format!(
        "zuse-native-jpeg-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir(&directory).unwrap();
    let result = (|| -> Result<(), String> {
        let path = directory.join("capture.jpg");
        let mut browser = Browser::launch(true)?;
        browser.viewport(1000, 900)?;
        browser.navigate("data:text/html,<title>JPEG fixture</title><style>body{margin:0;background:rgb(32,160,96);color:white;font:24px monospace}</style><h1>Full viewport JPEG</h1><p>1000 by 900</p>")?;
        let receipt = browser.screenshot_with_format(path.to_str().unwrap(), "jpeg", Some(80))?;
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        if !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
            return Err("native capture did not return complete JPEG framing".into());
        }
        json::with_value(&receipt, |root| {
            assert_eq!(json::text(root.get("mimeType").unwrap())?, "image/jpeg");
            assert_eq!(
                root.get("bytes").unwrap().integer().unwrap(),
                bytes.len() as i64
            );
            assert_eq!(root.get("width").unwrap().integer().unwrap(), 1000);
            assert_eq!(root.get("height").unwrap().integer().unwrap(), 900);
            let ratio = root.get("pixelsPerCssUnit").unwrap();
            assert_eq!(ratio.get("x").unwrap().raw(), "1");
            assert_eq!(ratio.get("y").unwrap().raw(), "1");
            Ok(())
        })?;
        // Chromium decodes its actual output independently of Zuse's marker
        // parser. No committed JPEG binary or external converter is needed.
        let source = format!(
            "data:image/jpeg;base64,{}",
            zuse::device_io::encode_base64(&bytes)
        );
        let expression = format!(
            "(async()=>{{const image=new Image();image.src={};await image.decode();return [image.naturalWidth,image.naturalHeight]}})()",
            json::quote(&source)
        );
        assert_eq!(browser.evaluate(&expression)?, "[1000,900]");
        browser.close()?;
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&directory);
    result.unwrap();
    cleanup.unwrap();
}
