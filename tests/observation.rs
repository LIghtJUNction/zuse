#![cfg(feature = "std")]
use std::{
    thread,
    time::{Duration, Instant},
};
use zuse::{
    browser::Browser,
    observation::{Cancellation, Condition, Observer, Query},
};

fn browser_query(selector: &str) -> Query {
    Query {
        source: "browser".into(),
        selector: selector.into(),
        scope: "0".into(),
    }
}
fn watch_id(receipt: &str) -> String {
    // The generated identifier uses only ASCII letters, digits and hyphens.
    receipt
        .split_once("\"watch\":\"")
        .unwrap()
        .1
        .split('"')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn observation_arguments_fail_before_native_access() {
    assert!(browser_query("").validate(false).is_err());
    assert!(Query::default().validate(true).is_err());
    assert!(
        Query {
            source: "camera".into(),
            ..Query::default()
        }
        .validate(false)
        .is_err()
    );
    assert!(
        Condition {
            kind: "text".into(),
            ..Condition::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
#[ignore = "requires installed sandboxed Chromium; run in a private test session"]
fn browser_waits_on_scoped_changes_without_requery_or_screenshots() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate("data:text/html,<title>zuse observation fixture</title><div id=scope><button id=turn>waiting</button></div><div id=outside style='position:absolute;left:500px;top:500px'>outside</div><input type=password value=never-observe-this>").unwrap();
    browser.evaluate("window.zuseQueries=0;const original=document.querySelectorAll.bind(document);document.querySelectorAll=(...args)=>{window.zuseQueries++;return original(...args)};true").unwrap();
    let mut observer = Observer::default();
    let initial = observer
        .watch(browser_query("#scope"), Some(&mut browser))
        .unwrap();
    let id = watch_id(&initial);
    assert!(!initial.contains("never-observe-this"));
    let before = Instant::now();
    browser.evaluate("setTimeout(()=>document.getElementById('outside').textContent='unrelated',20);setTimeout(()=>document.getElementById('turn').textContent='ready',140);true").unwrap();
    let condition = Condition {
        kind: "text".into(),
        text: Some("ready".into()),
        role: Some("button".into()),
    };
    let changed = observer
        .wait_until(
            &id,
            &condition,
            2000,
            &Cancellation::default(),
            Some(&mut browser),
        )
        .unwrap();
    assert!(before.elapsed() >= Duration::from_millis(90));
    assert!(changed.contains("\"status\":\"satisfied\"") && changed.contains("ready"));
    assert!(changed.contains("\"screenshots\":0"));
    assert_eq!(
        browser.evaluate("window.zuseQueries").unwrap(),
        "1",
        "wait must reuse the original scope"
    );

    let idle = observer.wait_until(
        &id,
        &Condition::default(),
        100,
        &Cancellation::default(),
        Some(&mut browser),
    );
    assert!(idle.unwrap_err().starts_with("timeout:"));
    assert_eq!(
        browser.evaluate("window.zuseQueries").unwrap(),
        "1",
        "idle waiting must not poll DOM queries"
    );

    let cancellation = Cancellation::default();
    let external = cancellation.clone();
    let cancel = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        external.cancel();
    });
    let began = Instant::now();
    assert!(
        observer
            .wait_until(
                &id,
                &Condition::default(),
                30000,
                &cancellation,
                Some(&mut browser)
            )
            .unwrap_err()
            .starts_with("cancelled:")
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    cancel.join().unwrap();
    assert!(
        observer
            .snapshot(&id, Some(&mut browser))
            .unwrap()
            .contains("ready"),
        "cancellation must preserve the selected page and watch"
    );

    browser
        .evaluate("setTimeout(()=>document.getElementById('turn').textContent='next',30);true")
        .unwrap();
    assert!(
        observer
            .wait_until(
                &id,
                &Condition::default(),
                2000,
                &Cancellation::default(),
                Some(&mut browser)
            )
            .unwrap()
            .contains("next")
    );
    observer.close(&id, Some(&mut browser)).unwrap();
    browser.close().unwrap();
}

#[test]
#[ignore = "requires installed sandboxed Chromium; run in a private test session"]
fn browser_rejects_ambiguous_watch_and_reports_removed_scope() {
    let mut browser = Browser::launch(true).unwrap();
    browser
        .navigate(
            "data:text/html,<div class=box></div><div class=box></div><div id=scope>visible</div>",
        )
        .unwrap();
    let mut observer = Observer::default();
    assert!(
        observer
            .watch(browser_query(".box"), Some(&mut browser))
            .unwrap_err()
            .contains("exactly one")
    );
    let id = watch_id(
        &observer
            .watch(browser_query("#scope"), Some(&mut browser))
            .unwrap(),
    );
    browser
        .evaluate("setTimeout(()=>document.getElementById('scope').remove(),30);true")
        .unwrap();
    let gone = Condition {
        kind: "gone".into(),
        ..Condition::default()
    };
    let result = observer
        .wait_until(
            &id,
            &gone,
            2000,
            &Cancellation::default(),
            Some(&mut browser),
        )
        .unwrap();
    assert!(result.contains("\"connected\":false"));
    observer.close(&id, Some(&mut browser)).unwrap();
    browser.close().unwrap();
}
