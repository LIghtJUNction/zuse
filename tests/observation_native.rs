#![cfg(all(feature = "std", target_os = "linux"))]
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use zuse::{
    accessibility,
    observation::{Cancellation, Condition, Observer, Query},
};

#[test]
#[ignore = "requires ZUSE_OBSERVATION_FIXTURE on an isolated X display and accessibility bus"]
fn atspi_watches_only_the_selected_scope_and_survives_cancellation() {
    let fixture = std::env::var("ZUSE_OBSERVATION_FIXTURE")
        .expect("compile tests/native_observation_fixture.c with GTK 3");
    let mut child = Command::new(fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "READY");
    let mut input = child.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let scope = loop {
        let snapshot = accessibility::snapshot(512, 16).unwrap();
        // Only fixture startup is retried. The watch below uses AT-SPI events.
        let found = snapshot.split("\"path\":").skip(1).find_map(|node| {
            let path = node.strip_prefix('"')?.split('"').next()?;
            if node
                .split("\"path\":")
                .next()?
                .contains("zuse-observation-scope")
            {
                Some(path.to_owned())
            } else {
                None
            }
        });
        if let Some(scope) = found {
            break scope;
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not appear on AT-SPI"
        );
        thread::sleep(Duration::from_millis(50));
    };
    let node = accessibility::node(&scope).unwrap();
    assert!(node.contains(&format!("\"pid\":{}", child.id())));
    let mut observer = Observer::default();
    let receipt = observer
        .watch(
            Query {
                source: "desktop".into(),
                scope,
                selector: String::new(),
            },
            None,
        )
        .unwrap();
    let id = receipt
        .split_once("\"watch\":\"")
        .unwrap()
        .1
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    writeln!(input, "outside").unwrap();
    input.flush().unwrap();
    assert!(
        observer
            .wait_until(
                &id,
                &Condition::default(),
                100,
                &Cancellation::default(),
                None
            )
            .unwrap_err()
            .starts_with("timeout:")
    );

    let send = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        writeln!(input, "ready").unwrap();
        input.flush().unwrap();
        input
    });
    let condition = Condition {
        kind: "text".into(),
        text: Some("ready".into()),
        role: Some("label".into()),
    };
    let result = observer
        .wait_until(&id, &condition, 3000, &Cancellation::default(), None)
        .unwrap();
    assert!(result.contains("\"status\":\"satisfied\"") && result.contains("ready"));
    assert!(result.contains("\"screenshots\":0"));
    input = send.join().unwrap();

    let cancellation = Cancellation::default();
    let external = cancellation.clone();
    let cancel = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        external.cancel();
    });
    let began = Instant::now();
    assert!(
        observer
            .wait_until(&id, &Condition::default(), 30000, &cancellation, None)
            .unwrap_err()
            .starts_with("cancelled:")
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    cancel.join().unwrap();
    assert!(observer.snapshot(&id, None).unwrap().contains("ready"));
    writeln!(input, "next").unwrap();
    input.flush().unwrap();
    assert!(
        observer
            .wait_until(
                &id,
                &Condition::default(),
                3000,
                &Cancellation::default(),
                None
            )
            .unwrap()
            .contains("next")
    );
    observer.close(&id, None).unwrap();
    writeln!(input, "quit").unwrap();
    input.flush().unwrap();
    assert!(child.wait().unwrap().success());
}
