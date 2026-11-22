#![cfg(feature = "std")]
use zuse::{
    browser::Browser,
    motion::Point,
    observation::Cancellation,
    runtime::{self, Options},
};

#[test]
fn browser_pointer_paths_are_preflighted_and_budgeted() {
    for source in [
        "browser.click_at(x=-1,y=0)",
        "browser.click_at(x=0,y=16777217)",
        "browser.click_at(x=0,y=0,button=\"invalid\")",
        "browser.stroke(points=\"1,2\")",
        "browser.stroke(points=\"1,2;NaN,3\")",
        "browser.stroke(points=\"1,2;3,-4\")",
        "browser.stroke(points=\"1,2;3,4;5,6\",duration=1)",
        "browser.stroke(points=\"1,2;3,4\",duration=60001)",
        "browser.stroke(points=\"1,2;3,4\",duration=40000);wait(ms=40000)",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    assert!(runtime::check("browser.click_at(x=12.5,y=2.25,button=\"middle\");browser.stroke(points=\"1,2;3,4\",duration=0)", Options::default()).is_ok());
    assert!(
        runtime::check(
            "browser.click_at(x=1,y=2)",
            Options {
                read_only: true,
                ..Options::default()
            }
        )
        .is_err()
    );
    let points = std::iter::repeat_n("1,2", 4097)
        .collect::<Vec<_>>()
        .join(";");
    assert!(
        runtime::check(
            &format!("browser.stroke(points=\"{points}\",duration=0)"),
            Options::default()
        )
        .is_err()
    );
    let points = std::iter::repeat_n("1,2", 4096)
        .collect::<Vec<_>>()
        .join(";");
    let source = std::iter::repeat_n(
        format!("browser.stroke(points=\"{points}\",duration=0)"),
        17,
    )
    .collect::<Vec<_>>()
    .join(";");
    assert!(
        runtime::check(&source, Options::default())
            .unwrap_err()
            .contains("65536")
    );
}

fn fixture() -> String {
    let mut result = String::from("data:text/html,");
    for byte in br#"<!doctype html><meta charset="utf-8"><title>Zuse pointer fixture</title><style>html,body{margin:0}canvas{display:block}</style><canvas width="500" height="350"></canvas><script>window.events=[];for(const type of ['mousedown','mousemove','mouseup','click'])document.addEventListener(type,e=>events.push({type:e.type,x:e.clientX,y:e.clientY,button:e.button,buttons:e.buttons,trusted:e.isTrusted,time:performance.now()}));</script>"# {
        use std::fmt::Write;
        write!(result, "%{byte:02X}").unwrap();
    }
    result
}

#[test]
#[ignore = "requires installed sandboxed Chromium; uses an owned profile only"]
fn native_browser_receives_trusted_coordinate_clicks_and_drags() {
    let mut browser = Browser::launch(true).unwrap();
    browser.viewport(640, 480).unwrap();
    browser.navigate(&fixture()).unwrap();
    browser
        .click_at(25.0, 35.0, "left", &Cancellation::default())
        .unwrap();
    assert_eq!(browser.evaluate("events.some(e=>e.type==='click'&&e.x===25&&e.y===35&&e.trusted)&&events.every(e=>e.trusted)").unwrap(), "true");
    browser.evaluate("events=[]").unwrap();
    let result = browser
        .stroke(
            &[
                Point { x: 20.0, y: 30.0 },
                Point { x: 80.0, y: 30.0 },
                Point { x: 80.0, y: 90.0 },
            ],
            180,
            "left",
            &Cancellation::default(),
        )
        .unwrap();
    assert!(result.contains("\"button_released\":true"));
    assert_eq!(browser.evaluate("events.filter(e=>e.type==='mousedown').length===1&&events.filter(e=>e.type==='mouseup').length===1&&events.some(e=>e.type==='mousemove'&&e.x===80&&e.y===30&&e.buttons===1)&&events.some(e=>e.type==='mouseup'&&e.x===80&&e.y===90&&e.buttons===0)&&events.every(e=>e.trusted)").unwrap(), "true");
    browser.evaluate("events=[]").unwrap();
    assert!(
        browser
            .stroke(
                &[Point { x: 20.0, y: 30.0 }, Point { x: 640.0, y: 90.0 }],
                100,
                "left",
                &Cancellation::default()
            )
            .is_err()
    );
    assert_eq!(browser.evaluate("events.length").unwrap(), "0");
    browser.close().unwrap();
}
