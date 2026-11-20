#![cfg(feature = "std")]

use zmcpio::json::{Document, Limits, Token};
use zuse::browser::Browser;

#[test]
#[ignore = "requires a local sandboxed Chromium; run explicitly with --ignored"]
fn native_selection_preserves_document_and_input() {
    let mut browser = Browser::launch(true).unwrap();
    browser
        .navigate("data:text/html,<title>zuse selection fixture</title><input id=entry>")
        .unwrap();
    browser.fill("#entry", "preserve 中文 🙂").unwrap();
    browser.evaluate("window.fixtureState = 37").unwrap();

    let tabs = browser.tabs().unwrap();
    let mut tokens = vec![Token::EMPTY; 4096];
    let document = Document::parse(
        tabs.as_bytes(),
        &mut tokens,
        Limits {
            max_bytes: 65536,
            max_depth: 16,
            max_members: 4096,
            max_string_bytes: 65536,
        },
    )
    .unwrap();
    let target = document
        .root()
        .get("targetInfos")
        .unwrap()
        .elements()
        .find_map(|tab| {
            let title: String = tab.get("title")?.string()?.chars().collect();
            (title == "zuse selection fixture").then(|| {
                tab.get("targetId")
                    .unwrap()
                    .string()
                    .unwrap()
                    .chars()
                    .collect::<String>()
            })
        })
        .unwrap();

    for _ in 0..2 {
        browser.select(&target).unwrap();
        assert_eq!(
            browser.evaluate(
                "document.title === 'zuse selection fixture' && window.fixtureState === 37 && document.querySelector('#entry').value === 'preserve 中文 🙂'"
            ).unwrap(),
            "true",
        );
    }
    assert!(browser.select("zuse-missing-fixture-target").is_err());
    assert_eq!(browser.evaluate("window.fixtureState").unwrap(), "37");
    browser.close().unwrap();
    assert!(browser.select(&target).is_err());
}
