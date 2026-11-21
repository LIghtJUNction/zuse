#![cfg(feature = "std")]
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use zuse::{
    browser::Browser,
    observation::Cancellation,
    runtime::{self, Options},
};

#[test]
fn hold_preflight_and_program_limits() {
    for command in ["keyboard.hold", "browser.hold"] {
        for args in [
            r#"key="a""#,
            r#"key="",duration=2"#,
            r#"key="a",duration=0"#,
            r#"key="a",duration=60001"#,
            r#"key="a",duration=-1"#,
            r#"key="a",duration="2""#,
        ] {
            assert!(runtime::check(&format!("{command}({args})"), Options::default()).is_err());
        }
        for duration in [1, 60_000] {
            let call = format!(r#"{command}(key="a",duration={duration})"#);
            assert!(runtime::check(&call, Options::default()).is_ok());
            assert!(
                runtime::check(
                    &call,
                    Options {
                        read_only: true,
                        ..Options::default()
                    }
                )
                .is_err()
            );
        }
    }
    let script = r#"keyboard.hold(key="a",duration=40); browser.hold(key="Enter",duration=10)"#;
    assert!(
        runtime::check(
            script,
            Options {
                max_wait_ms: 49,
                ..Options::default()
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            script,
            Options {
                max_wait_ms: 50,
                ..Options::default()
            }
        )
        .is_ok()
    );
    assert!(
        runtime::check(
            r#"keyboard.hold(key="a",duration=1,window="")"#,
            Options::default()
        )
        .is_err()
    );
    assert!(
        runtime::check(
            r#"browser.hold(key="unknown+a",duration=1)"#,
            Options::default()
        )
        .is_err()
    );
}

fn page() -> Browser {
    let mut browser = Browser::launch(true).unwrap();
    browser.evaluate("document.title='Local hold fixture';document.body.innerHTML='<input id=entry><input id=other>';true").unwrap();
    browser.evaluate("entry.focus();window.edges=[];for(const t of ['keydown','keyup'])document.addEventListener(t,e=>edges.push([e.type,e.target.id,e.key,e.isTrusted,performance.now()]),true);true").unwrap();
    browser
}

#[test]
#[ignore = "requires a sandboxed headless Chromium and uses only its private blank page"]
fn chromium_hold_receives_one_trusted_pair_for_duration_and_cancel() {
    let mut browser = page();
    browser.hold("a", 100, &Cancellation::default()).unwrap();
    assert_eq!(browser.evaluate("edges.length===2 && edges[0][0]==='keydown' && edges[1][0]==='keyup' && edges[1][4]-edges[0][4]>=95 && edges.every(e=>e[1]==='entry' && e[2]==='a' && e[3]) && entry.value==='a'").unwrap(),"true");
    browser.evaluate("edges.length=0").unwrap();
    browser.hold("Space", 1, &Cancellation::default()).unwrap();
    assert_eq!(
        browser
            .evaluate("entry.value==='a ' && edges.length===2 && edges.every(e=>e[2]===' ')")
            .unwrap(),
        "true"
    );
    browser.evaluate("edges.length=0").unwrap();
    let (sent, begin) = mpsc::channel();
    let token = Cancellation::with_dispatch_guard(move || {
        sent.send(()).unwrap();
        Ok(Box::new(()))
    });
    let halt = token.clone();
    let worker = thread::spawn(move || {
        begin.recv_timeout(Duration::from_secs(5)).unwrap();
        halt.cancel();
    });
    let start = Instant::now();
    assert!(
        browser
            .hold("Enter", 30_000, &token)
            .unwrap_err()
            .starts_with("cancelled:")
    );
    worker.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(browser.evaluate("edges.length===2 && edges[0][0]==='keydown' && edges[1][0]==='keyup' && edges.every(e=>e[1]==='entry' && e[2]==='Enter' && e[3])").unwrap(),"true");
}

#[test]
#[ignore = "requires a sandboxed headless Chromium and uses only its private blank page"]
fn chromium_hold_stops_on_focus_change_and_document_replacement() {
    let mut browser = page();
    browser
        .evaluate("entry.addEventListener('keydown',()=>other.focus(),{once:true});true")
        .unwrap();
    assert!(
        browser
            .hold("a", 30_000, &Cancellation::default())
            .unwrap_err()
            .starts_with("stale_target:")
    );
    assert_eq!(browser.evaluate("edges.length===1 && edges[0][0]==='keydown' && edges[0][1]==='entry' && document.activeElement===other").unwrap(),"true");
    browser
        .evaluate("other.addEventListener('keydown',()=>location.reload(),{once:true});true")
        .unwrap();
    assert!(
        browser
            .hold("b", 30_000, &Cancellation::default())
            .unwrap_err()
            .starts_with("stale_target:")
    );
}

#[test]
#[ignore = "requires a sandboxed headless Chromium and uses only its private blank page"]
fn chromium_hold_rejects_ambiguous_focus_and_denied_dispatch() {
    let mut browser = page();
    let denied = Cancellation::with_dispatch_guard(|| Err("input disabled".into()));
    assert_eq!(
        browser.hold("a", 10, &denied).unwrap_err(),
        "input disabled"
    );
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(
        browser
            .hold("a", 10, &cancelled)
            .unwrap_err()
            .starts_with("cancelled:")
    );
    browser.evaluate("const host=document.body.appendChild(document.createElement('div')); const inside=host.attachShadow({mode:'closed'}).appendChild(document.createElement('input')); inside.focus();true").unwrap();
    assert!(
        browser
            .hold("a", 10, &Cancellation::default())
            .unwrap_err()
            .starts_with("stale_target:")
    );
    assert_eq!(browser.evaluate("edges.length").unwrap(), "0");
}
