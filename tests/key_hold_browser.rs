#![cfg(feature = "std")]
use std::{
    thread,
    time::{Duration, Instant},
};
use zuse::{browser::Browser, observation::Cancellation};

fn fixture() -> String {
    let mut result = String::from("data:text/html,");
    for byte in br#"<!doctype html><title>Zuse hold fixture</title><input id="one" autofocus><input id="two"><script>window.events=[];for(const type of ['keydown','keyup'])document.addEventListener(type,e=>events.push({type:e.type,key:e.key,trusted:e.isTrusted,time:performance.now()}));</script>"# {
        use std::fmt::Write;
        write!(result, "%{byte:02X}").unwrap();
    }
    result
}

#[test]
#[ignore = "requires installed sandboxed Chromium; uses an owned profile only"]
fn browser_receives_a_real_hold_and_releases_on_cancellation_or_focus_change() {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&fixture()).unwrap();
    browser
        .evaluate("document.querySelector('#one').focus()")
        .unwrap();
    let result = browser
        .hold_key("Up", 180, &Cancellation::default())
        .unwrap();
    assert!(result.contains("\"released\":true"), "{result}");
    assert_eq!(browser.evaluate("events.length===2 && events[0].type==='keydown' && events[1].type==='keyup' && events.every(e=>e.trusted&&e.key==='ArrowUp') && events[1].time-events[0].time>=140").unwrap(), "true");

    browser.evaluate("events=[]").unwrap();
    let token = Cancellation::default();
    let cancel = token.clone();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        cancel.cancel();
    });
    let began = Instant::now();
    let error = browser.hold_key("Left", 5_000, &token).unwrap_err();
    worker.join().unwrap();
    assert!(error.starts_with("cancelled:"), "{error}");
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(browser.evaluate("events.length===2 && events[0].type==='keydown' && events[1].type==='keyup' && events.every(e=>e.key==='ArrowLeft')").unwrap(), "true");

    browser.evaluate("events=[];document.addEventListener('keydown',()=>setTimeout(()=>document.querySelector('#two').focus(),60),{once:true})").unwrap();
    let error = browser
        .hold_key("Down", 1_000, &Cancellation::default())
        .unwrap_err();
    assert!(error.starts_with("stale_target:"), "{error}");
    assert_eq!(browser.evaluate("events.length===2 && events[0].type==='keydown' && events[1].type==='keyup' && events.every(e=>e.key==='ArrowDown')").unwrap(), "true");
    browser.close().unwrap();
}
