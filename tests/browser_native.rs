#![cfg(feature = "std")]
use zuse::browser::Browser;

#[test]
#[ignore = "requires a local sandboxed Chromium; run explicitly with --ignored"]
fn native_chromium_isolated_profile() {
    let root = std::env::temp_dir().join(format!("zuse-browser-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let page = root.join("page.html");
    let screenshot = root.join("capture.png");
    std::fs::write(&page, r#"<!doctype html><html><head><meta charset="utf-8"><title>zuse fixture</title></head><body><h1>Native zuse</h1><label for="entry">Agent input</label><input id="entry"><button id="button" onclick="document.getElementById('result').textContent='clicked'">Run fixture</button><p id="result">waiting</p><input id="password" type="password" value="never-expose-this"><script>document.addEventListener('keydown',e=>window.lastKey=e.key)</script></body></html>"#).unwrap();
    let mut browser = Browser::launch(true).unwrap();
    browser.viewport(640, 480).unwrap();
    browser
        .navigate(&format!("file://{}", page.display()))
        .unwrap();
    assert_eq!(
        browser.evaluate("document.title").unwrap(),
        "\"zuse fixture\""
    );
    browser.fill("#entry", "zuse 中文 🙂").unwrap();
    assert_eq!(
        browser
            .evaluate("document.querySelector('#entry').value === 'zuse 中文 🙂'")
            .unwrap(),
        "true"
    );
    browser.key("Enter").unwrap();
    assert_eq!(browser.evaluate("window.lastKey").unwrap(), "\"Enter\"");
    browser.click("#button").unwrap();
    assert_eq!(
        browser
            .evaluate("document.querySelector('#result').textContent")
            .unwrap(),
        "\"clicked\""
    );
    let snapshot = browser.snapshot().unwrap();
    assert!(snapshot.contains("Native zuse"));
    assert!(!snapshot.contains("never-expose-this"));
    assert!(browser.fill("#password", "secret").is_err());
    assert!(browser.text().unwrap().contains("Native zuse"));
    assert!(browser.tabs().unwrap().contains("targetInfos"));
    let capture = browser.screenshot(screenshot.to_str().unwrap()).unwrap();
    zuse::json::with_value(&capture, |root| {
        assert_eq!(root.get("width").unwrap().integer().unwrap(), 640);
        assert_eq!(root.get("height").unwrap().integer().unwrap(), 480);
        let viewport = root.get("cssViewport").unwrap();
        assert_eq!(viewport.get("width").unwrap().integer().unwrap(), 640);
        assert_eq!(viewport.get("height").unwrap().integer().unwrap(), 480);
        let ratio = root.get("pixelsPerCssUnit").unwrap();
        assert_eq!(ratio.get("x").unwrap().raw(), "1");
        assert_eq!(ratio.get("y").unwrap().raw(), "1");
        Ok(())
    })
    .unwrap();
    let png = std::fs::read(&screenshot).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 640);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 480);
    browser.close().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
