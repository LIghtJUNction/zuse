#![cfg(feature = "std")]

use std::{thread, time::Duration};
use zuse::{
    browser::Browser,
    json,
    observation::Cancellation,
    runtime::{self, Options, Runtime},
};

#[test]
fn canvas_commands_preflight_before_opening_a_browser() {
    let program = "canvas.watch(selector=\"#park-canvas\",name=\"parking\");canvas.read(name=\"parking\");canvas.close(name=\"parking\")";
    // TTL bounds an observer in the background; it does not wait or consume a
    // caller's input-wait budget. Dry-run must not require an active browser.
    assert!(
        runtime::check(
            program,
            Options {
                max_wait_ms: 0,
                ..Options::default()
            }
        )
        .is_ok()
    );
    let planned = Runtime::new()
        .execute(
            program,
            Options {
                dry_run: true,
                ..Options::default()
            },
        )
        .unwrap();
    assert!(!runtime::failed(&planned));
    assert!(
        runtime::check(
            "canvas.read(name=\"parking\")",
            Options {
                read_only: true,
                ..Options::default()
            }
        )
        .is_ok()
    );
    for source in [
        "canvas.watch(selector=\"canvas\",name=\"parking\")",
        "canvas.close(name=\"parking\")",
    ] {
        assert!(
            runtime::check(
                source,
                Options {
                    read_only: true,
                    ..Options::default()
                }
            )
            .is_err()
        );
    }
    for source in [
        "canvas.watch(name=\"parking\")",
        "canvas.watch(selector=\"canvas\")",
        "canvas.watch(selector=\" \" ,name=\"parking\")",
        "canvas.watch(selector=\"canvas\",name=\"parking\",ttl_ms=0)",
        "canvas.watch(selector=\"canvas\",name=\"parking\",ttl_ms=15001)",
        "canvas.watch(selector=\"canvas\",name=\"parking\",history=0)",
        "canvas.watch(selector=\"canvas\",name=\"parking\",history=\"false\")",
        "canvas.read()",
        "canvas.close(name=\"../canvas\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
    assert!(
        runtime::check(
            "canvas.watch(selector=\"canvas\",name=\"parking\",ttl_ms=15000)",
            Options::default()
        )
        .is_ok()
    );
    for history in ["true", "false"] {
        let source =
            format!("canvas.watch(selector=\"canvas\",name=\"parking\",history={history})");
        assert!(runtime::check(&source, Options::default()).is_ok());
    }
    for (name, valid) in [
        ("x".repeat(64), true),
        ("x".repeat(65), false),
        ("a\0b".into(), false),
    ] {
        let source = format!("canvas.read(name={})", json::quote(&name));
        assert_eq!(runtime::check(&source, Options::default()).is_ok(), valid);
    }
    assert!(runtime::schema().contains("\"name\":\"canvas.read\",\"read_only\":true"));
}

fn fixture() -> String {
    let mut result = String::from("data:text/html,");
    for byte in br#"<!doctype html><meta charset="utf-8"><title>Zuse canvas observer fixture</title><canvas id="park-canvas" width="446" height="446"></canvas><canvas id="other" width="446" height="446"></canvas><script>
const fixture = {canvas:document.querySelector('#park-canvas'),other:document.querySelector('#other'),calls:0};
fixture.context=fixture.canvas.getContext('2d');fixture.otherContext=fixture.other.getContext('2d');
fixture.native=CanvasRenderingContext2D.prototype.drawImage;
fixture.original=function(...args){fixture.calls++;return Reflect.apply(fixture.native,this,args);};
CanvasRenderingContext2D.prototype.drawImage=fixture.original;
fixture.image=new Image();
fixture.image.src=URL.createObjectURL(new Blob(['<svg xmlns="http://www.w3.org/2000/svg" width="355" height="200"><rect width="355" height="200" fill="blue"/></svg>'],{type:'image/svg+xml'}));
fixture.ready=fixture.image.decode().then(()=>true);
fixture.draw=()=>{fixture.context.setTransform(1.115,0,0,1.115,55.75,83.625);fixture.context.drawImage(fixture.image,-40,-25,80,50);};
</script>"# {
        use std::fmt::Write;
        write!(result, "%{byte:02X}").unwrap();
    }
    result
}

fn browser() -> Browser {
    let mut browser = Browser::launch(true).unwrap();
    browser.navigate(&fixture()).unwrap();
    assert_eq!(browser.evaluate("fixture.ready").unwrap(), "true");
    browser
}

fn field(receipt: &str, key: &str) -> String {
    json::with_value(receipt, |root| Ok(root.get(key).unwrap().raw().to_owned())).unwrap()
}

#[test]
#[ignore = "requires installed sandboxed Chromium; uses public drawing APIs in an owned profile"]
fn native_canvas_records_public_draws_and_binds_read_only_conditions() {
    let mut browser = browser();
    let cancellation = Cancellation::default();
    assert!(
        browser
            .canvas_watch("canvas", "parking", 5000, &cancellation)
            .unwrap_err()
            .contains("ambiguous_target")
    );
    let receipt = browser
        .canvas_watch("#park-canvas", "parking", 5000, &cancellation)
        .unwrap();
    assert_eq!(field(&receipt, "available"), "false");
    assert_eq!(
        field(&receipt, "unavailable_reason"),
        "\"no_matching_2d_drawImage_observed\""
    );
    browser
        .evaluate("fixture.draw();fixture.otherContext.drawImage(fixture.image,0,0)")
        .unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "event_count"), "1");
    assert_eq!(field(&receipt, "retained_event_count"), "1");
    assert_eq!(field(&receipt, "history_recorded"), "true");
    assert_eq!(field(&receipt, "source_count"), "1");
    assert_eq!(field(&receipt, "coordinate_space"), "\"canvas_bitmap\"");
    assert_eq!(field(&receipt, "visibility"), "\"unverified\"");
    assert_eq!(browser.evaluate("fixture.calls").unwrap(), "2");
    // Chromium can retain Canvas transforms at float32 precision. Preserve
    // those observed values exactly, then separately bound their deviation
    // from the fixture's intended scale and bitmap-space translation.
    let transform = browser
        .evaluate(
            "(()=>{const m=fixture.context.getTransform();return [m.a,m.b,m.c,m.d,m.e,m.f];})()",
        )
        .unwrap();
    let expected_matrix = json::with_value(&transform, |root| {
        Ok(root
            .elements()
            .map(|value| value.raw().parse::<f64>().unwrap())
            .collect::<Vec<_>>())
    })
    .unwrap();
    let (matrix, center) = json::with_value(&receipt, |root| {
        let draw = root.get("latest").unwrap().elements().next().unwrap();
        let matrix = draw
            .get("matrix")
            .unwrap()
            .elements()
            .map(|value| value.raw().parse::<f64>().unwrap())
            .collect::<Vec<_>>();
        let center = draw
            .get("center")
            .unwrap()
            .elements()
            .map(|value| value.raw().parse::<f64>().unwrap())
            .collect::<Vec<_>>();
        Ok((matrix, center))
    })
    .unwrap();
    assert_eq!(
        matrix, expected_matrix,
        "raw matrix must match getTransform(): {transform}; receipt: {receipt}"
    );
    assert_eq!(
        center,
        [expected_matrix[4], expected_matrix[5]],
        "the drawn rectangle is centered at local (0,0); receipt: {receipt}"
    );
    const MATRIX_ABS_EPSILON: f64 = 1e-7;
    const BITMAP_ABS_EPSILON: f64 = 1e-5;
    for (index, (actual, nominal)) in matrix
        .iter()
        .zip([1.115, 0.0, 0.0, 1.115, 55.75, 83.625])
        .enumerate()
    {
        let epsilon = if index < 4 {
            MATRIX_ABS_EPSILON
        } else {
            BITMAP_ABS_EPSILON
        };
        assert!(
            (actual - nominal).abs() <= epsilon,
            "matrix[{index}] exceeds absolute tolerance {epsilon}; getTransform(): {transform}; receipt: {receipt}"
        );
    }
    let condition = browser
        .canvas_condition(
            "parking",
            r#"(()=>{
        const m=fixture.context.getTransform(), expected=[m.a,m.b,m.c,m.d,m.e,m.f];
        const draw=state.latest[0];
        return state.available && state.event_count === 1 && draw.matrix.length === 6
            && draw.matrix.every((value,index)=>value === expected[index])
            && Math.abs(draw.matrix[0]-1.115) <= 1e-7
            && Math.abs(draw.center[0]-55.75) <= 1e-5
            && Math.abs(draw.center[1]-83.625) <= 1e-5
            && draw.source.width === 355 && draw.source.height === 200
            && draw.source.src.startsWith('blob:');
    })()"#,
        )
        .unwrap();
    assert_eq!(
        browser.evaluate(&condition).unwrap(),
        "true",
        "getTransform(): {transform}; receipt: {receipt}"
    );

    let mutate_copy = browser.canvas_condition("parking", "(()=>{state.latest[0].matrix[0]=999;state.latest[0].source_rect[0]=999;state.latest[0].destination_rect[0]=999;state.sources[0].src='changed';return true;})()").unwrap();
    assert_eq!(browser.evaluate(&mutate_copy).unwrap(), "true");
    assert_eq!(browser.evaluate(&condition).unwrap(), "true");
    assert_eq!(browser.evaluate("(()=>{try{globalThis[Symbol.for('zuse.canvas.observer.v1')].state('parking','another-owner');return false;}catch(error){return error.message.includes('owner');}})()").unwrap(), "true");
    assert!(
        browser
            .canvas_watch("#park-canvas", "second", 5000, &cancellation)
            .is_err()
    );
    assert_eq!(
        browser.evaluate(&condition).unwrap(),
        "true",
        "failed second watch must not remove the first watch"
    );

    // Cleanup remains available after cancellation, then ordinary drawing works.
    let cancelled = Cancellation::default();
    cancelled.cancel();
    let receipt = browser.canvas_close("parking", &cancelled).unwrap();
    assert_eq!(field(&receipt, "restored"), "true");
    assert_eq!(browser.evaluate("CanvasRenderingContext2D.prototype.drawImage === fixture.original && !globalThis[Symbol.for('zuse.canvas.observer.v1')]").unwrap(), "true");
    assert!(
        browser
            .evaluate(&condition)
            .unwrap_err()
            .contains("stale_target")
    );
    assert!(
        browser
            .canvas_watch("#park-canvas", "parking", 5000, &cancelled)
            .is_err()
    );
    assert_eq!(
        browser
            .evaluate("CanvasRenderingContext2D.prototype.drawImage === fixture.original")
            .unwrap(),
        "true"
    );
    browser.close().unwrap();
}

#[test]
#[ignore = "requires installed sandboxed Chromium; uses an owned profile only"]
fn native_canvas_expires_invalidates_and_preserves_restore_conflicts() {
    let mut browser = browser();
    let cancellation = Cancellation::default();
    browser
        .canvas_watch("#park-canvas", "parking", 40, &cancellation)
        .unwrap();
    thread::sleep(Duration::from_millis(80));
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "status"), "\"expired\"");
    assert_eq!(field(&receipt, "restored"), "true");
    let condition = browser.canvas_condition("parking", "true").unwrap();
    assert!(
        browser
            .evaluate(&condition)
            .unwrap_err()
            .contains("expired")
    );
    browser.canvas_close("parking", &cancellation).unwrap();

    browser
        .canvas_watch("#park-canvas", "parking", 5000, &cancellation)
        .unwrap();
    browser.evaluate("fixture.canvas.width++").unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "stop_reason"), "\"canvas_resized\"");
    assert_eq!(field(&receipt, "restored"), "true");
    browser.canvas_close("parking", &cancellation).unwrap();

    browser
        .canvas_watch("#park-canvas", "parking", 5000, &cancellation)
        .unwrap();
    browser.evaluate("fixture.replacement=function(...args){return Reflect.apply(fixture.native,this,args);};CanvasRenderingContext2D.prototype.drawImage=fixture.replacement").unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "restore_conflict"), "true");
    let receipt = browser.canvas_close("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "restore_conflict"), "true");
    assert_eq!(
        browser
            .evaluate("CanvasRenderingContext2D.prototype.drawImage === fixture.replacement")
            .unwrap(),
        "true"
    );

    browser
        .evaluate("CanvasRenderingContext2D.prototype.drawImage=fixture.original")
        .unwrap();
    browser
        .canvas_watch("#park-canvas", "parking", 5000, &cancellation)
        .unwrap();
    browser.navigate("about:blank").unwrap();
    assert!(
        browser
            .canvas_read("parking", &cancellation)
            .unwrap_err()
            .contains("stale_target")
    );
    browser.canvas_close("parking", &cancellation).unwrap();
    browser.close().unwrap();
}

#[test]
#[ignore = "requires installed sandboxed Chromium; no network images or input are used"]
fn native_canvas_caps_records_without_suppressing_native_draws() {
    let mut browser = browser();
    let cancellation = Cancellation::default();
    browser
        .canvas_watch("#park-canvas", "parking", 15000, &cancellation)
        .unwrap();
    browser
        .evaluate("for(let i=0;i<5000;i++)fixture.context.drawImage(fixture.image,i%400,0)")
        .unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "status"), "\"limit\"");
    assert_eq!(field(&receipt, "restored"), "true");
    assert_eq!(field(&receipt, "truncated"), "true");
    assert!(field(&receipt, "event_count").parse::<usize>().unwrap() <= 4096);
    assert!(receipt.len() <= 256 * 1024);
    assert_eq!(browser.evaluate("fixture.calls").unwrap(), "5000");
    browser.canvas_close("parking", &cancellation).unwrap();

    browser
        .canvas_watch("#park-canvas", "parking", 15000, &cancellation)
        .unwrap();
    browser.evaluate("for(let i=0;i<65;i++){const source=document.createElement('canvas');source.width=source.height=1;fixture.context.drawImage(source,0,0)}").unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "stop_reason"), "\"sources\"");
    assert_eq!(field(&receipt, "source_count"), "64");
    assert_eq!(browser.evaluate("fixture.calls").unwrap(), "5065");
    browser.canvas_close("parking", &cancellation).unwrap();
    browser.close().unwrap();
}

#[test]
#[ignore = "requires installed sandboxed Chromium; uses bounded public draws in an owned profile"]
fn native_canvas_latest_mode_stays_fresh_without_growing_history() {
    let mut browser = browser();
    let cancellation = Cancellation::default();
    browser
        .canvas_watch_with_history("#park-canvas", "parking", 15000, false, &cancellation)
        .unwrap();
    browser
        .evaluate("for(let i=0;i<5000;i++)fixture.context.drawImage(fixture.image,i%400,0)")
        .unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "status"), "\"watching\"");
    assert_eq!(field(&receipt, "active"), "true");
    assert_eq!(field(&receipt, "available"), "true");
    assert_eq!(field(&receipt, "event_count"), "5000");
    assert_eq!(field(&receipt, "retained_event_count"), "0");
    assert_eq!(field(&receipt, "history_recorded"), "false");
    assert_eq!(field(&receipt, "events"), "[]");
    assert_eq!(field(&receipt, "source_count"), "1");
    assert_eq!(field(&receipt, "truncated"), "false");
    assert_eq!(field(&receipt, "visibility"), "\"unverified\"");
    assert!(
        receipt.len() < 8192,
        "one retained source must not grow with 5000 historical draws"
    );
    let condition = browser.canvas_condition("parking", "state.event_count === 5000 && state.latest.length === 1 && state.latest[0].seq === 5000 && state.latest[0].args[0] === 199").unwrap();
    assert_eq!(browser.evaluate(&condition).unwrap(), "true");
    browser
        .evaluate("fixture.context.drawImage(fixture.image,321,123)")
        .unwrap();
    let condition = browser.canvas_condition("parking", "state.event_count === 5001 && state.latest[0].seq === 5001 && state.latest[0].args[0] === 321 && state.latest[0].args[1] === 123").unwrap();
    assert_eq!(
        browser.evaluate(&condition).unwrap(),
        "true",
        "latest state must still advance after the history-mode cap"
    );
    assert_eq!(browser.evaluate("fixture.calls").unwrap(), "5001");
    let closed = browser.canvas_close("parking", &cancellation).unwrap();
    assert_eq!(field(&closed, "event_count"), "5001");

    // Removing history never removes the independent source-count budget.
    browser
        .canvas_watch_with_history("#park-canvas", "parking", 15000, false, &cancellation)
        .unwrap();
    browser.evaluate("for(let i=0;i<65;i++){const source=document.createElement('canvas');source.width=source.height=1;fixture.context.drawImage(source,0,0)}").unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "stop_reason"), "\"sources\"");
    assert_eq!(field(&receipt, "source_count"), "64");
    assert_eq!(field(&receipt, "events"), "[]");
    assert!(receipt.len() <= 256 * 1024);
    browser.canvas_close("parking", &cancellation).unwrap();

    // Long public blob URLs make metadata exhaust the byte budget before 64
    // sources. All image data is local to the fixture; no network is used.
    browser
        .canvas_watch_with_history("#park-canvas", "parking", 15000, false, &cancellation)
        .unwrap();
    browser.evaluate("(async()=>{const images=await Promise.all(Array.from({length:64},async(_,i)=>{const image=new Image();image.src=fixture.image.src+'#'+i+'-'+ 'x'.repeat(2048);await image.decode();return image;}));for(const image of images)fixture.context.drawImage(image,0,0);})()").unwrap();
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "stop_reason"), "\"bytes\"");
    assert!(field(&receipt, "source_count").parse::<usize>().unwrap() < 64);
    assert_eq!(field(&receipt, "history_recorded"), "false");
    assert_eq!(field(&receipt, "retained_event_count"), "0");
    assert!(receipt.len() <= 256 * 1024);
    browser.canvas_close("parking", &cancellation).unwrap();

    browser
        .canvas_watch_with_history("#park-canvas", "parking", 40, false, &cancellation)
        .unwrap();
    thread::sleep(Duration::from_millis(80));
    let receipt = browser.canvas_read("parking", &cancellation).unwrap();
    assert_eq!(field(&receipt, "status"), "\"expired\"");
    assert_eq!(field(&receipt, "restored"), "true");
    assert_eq!(field(&receipt, "history_recorded"), "false");
    let condition = browser.canvas_condition("parking", "true").unwrap();
    assert!(
        browser
            .evaluate(&condition)
            .unwrap_err()
            .contains("expired")
    );
    browser.canvas_close("parking", &cancellation).unwrap();
    browser.close().unwrap();
}
