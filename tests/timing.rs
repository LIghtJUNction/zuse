#![cfg(feature = "std")]
use zuse::{
    json,
    runtime::{Options, Runtime},
};

fn field(result: &str, field: &str) -> String {
    json::with_value(result, |root| Ok(root.get(field).unwrap().raw().to_owned())).unwrap()
}

#[test]
fn timing_accumulates_across_commands_resets_and_stops() {
    let mut runtime = Runtime::new();
    runtime
        .execute("timer.start()", Options::default())
        .unwrap();
    let first = runtime.execute("wait(ms=2)", Options::default()).unwrap();
    assert!(first.contains("\"timing\":") && first.contains("\"commands\":1"));
    assert!(first.contains("\"command_us\":") && first.contains("\"total_elapsed_us\":"));
    let second = runtime.execute("wait(ms=2)", Options::default()).unwrap();
    assert!(second.contains("\"commands\":2"));
    let reset = runtime
        .execute("timer.reset()", Options::default())
        .unwrap();
    assert!(reset.contains("\"commands\":0") && reset.contains("\"command_total_us\":0"));
    runtime
        .execute("wait(ms=1);timer.stop()", Options::default())
        .unwrap();
    let frozen = runtime
        .execute("timer.status()", Options::default())
        .unwrap();
    runtime.execute("wait(ms=2)", Options::default()).unwrap();
    let later = runtime
        .execute("timer.status()", Options::default())
        .unwrap();
    let value = |text: &str| {
        json::with_value(text, |root| {
            Ok(root
                .get("results")
                .unwrap()
                .elements()
                .next()
                .unwrap()
                .get("value")
                .unwrap()
                .raw()
                .to_owned())
        })
        .unwrap()
    };
    assert_eq!(value(&frozen), value(&later));
    assert!(!later.contains("\"timing\":"));
}

#[test]
fn failed_commands_are_timed_and_stop_the_remaining_program() {
    let mut runtime = Runtime::new();
    let result = runtime
        .execute(
            "timer.start();browser.text();timer.reset()",
            Options::default(),
        )
        .unwrap();
    assert_eq!(field(&result, "status"), "\"failed\"");
    assert_eq!(field(&result, "failed_step"), "1");
    assert!(result.contains("\"timing\":") && result.contains("\"commands\":1"));
    assert!(!result.contains("\"operation\":\"timer.reset\""));
}
