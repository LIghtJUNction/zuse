#![cfg(feature = "std")]
use std::{
    thread,
    time::{Duration, Instant},
};
use zuse::{
    browser::Browser,
    observation::{Cancellation, ClickWatchOptions},
};

fn fixture() -> String {
    let mut url = String::from("data:text/html,");
    for byte in include_bytes!("browser_click_fixture.html") {
        use std::fmt::Write;
        write!(url, "%{byte:02X}").unwrap();
    }
    url
}

fn options(max_clicks: u32, timeout_ms: u64) -> ClickWatchOptions {
    ClickWatchOptions {
        scope: "#arena".into(),
        selector: ".mole".into(),
        expected_origin: Some("null".into()),
        max_clicks,
        timeout_ms,
    }
}

#[test]
fn explicit_click_watch_requires_a_target_and_finite_bounds() {
    assert!(ClickWatchOptions::default().validate().is_err());
    assert_eq!(ClickWatchOptions::default().max_clicks, 1);
    assert_eq!(ClickWatchOptions::default().timeout_ms, 30000);
    for (count, timeout) in [(0, 1000), (65, 1000), (1, 0), (1, 60001)] {
        assert!(options(count, timeout).validate().is_err());
    }
    assert!(options(2, 2000).validate().is_ok());
}

#[test]
#[ignore = "requires installed sandboxed Chromium; run in a private test session"]
fn browser_click_watch_only_clicks_once_per_visible_episode() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&fixture()).unwrap();
    browser.evaluate("fixture.start('hold',1)").unwrap();
    let result = browser
        .watch_until_click(&options(2, 450), &Cancellation::default())
        .unwrap_err();
    assert!(result.starts_with("timeout:"), "{result}");
    assert!(
        result.contains("\"confirmed_clicks\":1") && result.contains("\"dispatch_attempts\":1"),
        "{result}"
    );
    assert_eq!(browser.evaluate("fixture.clicks.length").unwrap(), "1");
    assert_eq!(browser.evaluate("fixture.outsideClicks").unwrap(), "0");
    assert_eq!(
        browser.evaluate("fixture.queries").unwrap(),
        "1",
        "only the original scope is queried"
    );

    browser.navigate(&fixture()).unwrap();
    browser.evaluate("fixture.start('repeat',2)").unwrap();
    let result = browser
        .watch_until_click(&options(2, 2000), &Cancellation::default())
        .unwrap();
    assert!(
        result.contains("\"confirmed_clicks\":2") && result.contains("\"screenshots\":0"),
        "{result}"
    );
    assert_eq!(browser.evaluate("fixture.clicks.length===2 && fixture.clicks[0].round===1 && fixture.clicks[1].round===2 && fixture.clicks.every(click=>click.trusted)").unwrap(), "true");
    assert_eq!(
        browser
            .evaluate("fixture.outsideClicks===0 && fixture.queries===1")
            .unwrap(),
        "true"
    );
    browser.close().unwrap();
}

#[test]
#[ignore = "requires installed sandboxed Chromium; run in a private test session"]
fn browser_click_watch_cancellation_and_navigation_stop_before_input() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&fixture()).unwrap();
    browser.evaluate("fixture.start('hold',1,300)").unwrap();
    let token = Cancellation::default();
    let cancel = token.clone();
    let thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        cancel.cancel();
    });
    let began = Instant::now();
    let result = browser
        .watch_until_click(&options(1, 30000), &token)
        .unwrap_err();
    assert!(
        result.starts_with("cancelled:") && result.contains("\"dispatch_attempts\":0"),
        "{result}"
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    thread.join().unwrap();
    // The target can still appear after cancellation. No browser-side callback
    // is allowed to dispatch input on its own, including after this call ends.
    assert_eq!(
        browser
            .evaluate("new Promise(resolve=>setTimeout(()=>resolve(fixture.clicks.length),350))")
            .unwrap(),
        "0"
    );
    assert!(
        browser.observe_scope("#arena").is_ok(),
        "the same session remains usable"
    );

    browser.navigate(&fixture()).unwrap();
    browser.evaluate("window.name='no-input';document.addEventListener('click',()=>window.name='unexpected-input');fixture.start('hold',1,300);setTimeout(()=>location.replace('about:blank'),40);true").unwrap();
    let result = browser
        .watch_until_click(&options(1, 2000), &Cancellation::default())
        .unwrap_err();
    assert!(result.contains("\"dispatch_attempts\":0"), "{result}");
    assert_eq!(browser.evaluate("window.name").unwrap(), "\"no-input\"");
    browser.close().unwrap();
}

#[test]
#[ignore = "requires installed sandboxed Chromium; run in a private test session"]
fn browser_click_watch_refuses_occlusion_ambiguity_and_different_origin() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&fixture()).unwrap();
    browser
        .evaluate("fixture.cover();fixture.start('hold',1)")
        .unwrap();
    let result = browser
        .watch_until_click(&options(1, 2000), &Cancellation::default())
        .unwrap_err();
    assert!(
        result.contains("target_occluded") && result.contains("\"dispatch_attempts\":0"),
        "{result}"
    );
    assert_eq!(
        browser
            .evaluate("fixture.clicks.length + fixture.outsideClicks")
            .unwrap(),
        "0"
    );

    browser.navigate(&fixture()).unwrap();
    browser.evaluate("const other=document.createElement('button');other.className='mole';other.textContent='Other';document.getElementById('arena').appendChild(other);fixture.show();true").unwrap();
    let result = browser
        .watch_until_click(&options(1, 2000), &Cancellation::default())
        .unwrap_err();
    assert!(
        result.contains("ambiguous_visible_targets") && result.contains("\"dispatch_attempts\":0"),
        "{result}"
    );

    let mut wrong_origin = options(1, 2000);
    wrong_origin.expected_origin = Some("https://example.invalid".into());
    let result = browser
        .watch_until_click(&wrong_origin, &Cancellation::default())
        .unwrap_err();
    assert!(
        result.contains("expected_origin") && result.contains("\"dispatch_attempts\":0"),
        "{result}"
    );
    browser.close().unwrap();
}

#[test]
#[ignore = "requires installed sandboxed Chromium; prints fixture-measured p95 latency"]
fn browser_click_watch_reports_fixture_received_click_latency() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&fixture()).unwrap();
    browser.evaluate("fixture.start('repeat',24)").unwrap();
    let result = browser
        .watch_until_click(&options(24, 10000), &Cancellation::default())
        .unwrap();
    assert!(result.contains("\"confirmed_clicks\":24"), "{result}");
    assert_eq!(browser.evaluate("fixture.clicks.length===24 && new Set(fixture.clicks.map(click=>click.round)).size===24 && fixture.clicks.every(click=>click.trusted) && fixture.outsideClicks===0 && fixture.queries===1").unwrap(), "true");
    let p95: f64 = browser.evaluate("fixture.p95()").unwrap().parse().unwrap();
    let minimum: f64 = browser
        .evaluate("Math.min(...fixture.clicks.map(click=>click.latency_ms))")
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        p95.is_finite() && minimum >= 0.0 && p95 < 1000.0,
        "fixture p95 was {p95} ms"
    );
    println!(
        "browser click watch: 24/24 trusted clicks, fixture appearance-to-received p95={p95:.3} ms, minimum={minimum:.3} ms, screenshots=0, scope queries=1"
    );
    browser.close().unwrap();
}
