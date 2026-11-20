#![cfg(all(feature = "std", target_os = "linux"))]
use zmcpio::json::{Document, Limits, Token};
use zuse::accessibility;
use zuse::{
    json,
    motion::Point,
    platform::Desktop,
    pointer::{MoveOptions, MovePlan},
    runtime::{self, Options, Runtime},
    target::{self, TargetQuery},
};

fn path_for(snapshot: &str, name: &str) -> Option<String> {
    let mut tokens = vec![Token::EMPTY; 16384];
    let document = Document::parse(snapshot.as_bytes(), &mut tokens, Limits::default()).unwrap();
    for node in document.root().get("nodes")?.elements() {
        if node
            .get("name")
            .and_then(|v| v.string())
            .is_some_and(|s| s.matches(name))
        {
            return node
                .get("path")
                .and_then(|v| v.string())
                .map(|s| s.chars().collect());
        }
    }
    None
}

#[test]
#[ignore = "run through python3 tests/run_accessibility_native.py for an isolated fixture"]
fn native_atspi_observe_edit_action_and_redaction() {
    assert_eq!(
        std::env::var("ZUSE_ATSPI_FIXTURE").as_deref(),
        Ok("1"),
        "run isolated fixture launcher"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let snapshot = loop {
        let snapshot = accessibility::snapshot(256, 12).unwrap();
        if path_for(&snapshot, "Zuse editable fixture").is_some() {
            break snapshot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture absent: {snapshot}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(!snapshot.contains("never-expose-this"), "{snapshot}");
    assert!(snapshot.contains("[redacted]"), "{snapshot}");
    verify_target_movement();
    let entry = path_for(&snapshot, "Zuse editable fixture").unwrap();
    let button = path_for(&snapshot, "Zuse action fixture").unwrap();
    let password = path_for(&snapshot, "Zuse secret fixture").unwrap();
    accessibility::set_text(&entry, "zuse 中文").unwrap();
    accessibility::focus(&entry).unwrap();
    assert!(accessibility::text(256, 12).unwrap().contains("zuse 中文"));
    assert!(accessibility::set_text(&password, "secret").is_err());
    accessibility::action(&button, 0).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let snapshot = accessibility::snapshot(256, 12).unwrap();
        if snapshot.contains("Zuse action completed") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "action not observed: {snapshot}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn verify_target_movement() {
    let mut desktop = Desktop::connect().unwrap();
    desktop.move_to(24, 30).unwrap();
    let mut runtime = Runtime::new();
    let began = std::time::Instant::now();
    let output = runtime.execute(
        "mouse.move_to(text=\"Zuse action fixture\",role=\"button\",duration=160,curve=\"bezier\",via=\"120,30\",jitter=1.5,speed_variation=0.3,offset_x=4,offset_y=-3,seed=42)",
        Options::default(),
    ).unwrap();
    assert!(!runtime::failed(&output), "{output}");
    assert!(began.elapsed() >= std::time::Duration::from_millis(150));
    let located = target::locate(&TargetQuery::button("Zuse action fixture")).unwrap();
    let expected = (
        (located.bounds.x + located.bounds.width / 2.0 + 4.0).round() as i32,
        (located.bounds.y + located.bounds.height / 2.0 - 3.0).round() as i32,
    );
    assert_eq!(desktop.position().unwrap(), expected);
    json::with_value(&output, |root| {
        let value = root
            .get("results")
            .unwrap()
            .elements()
            .next()
            .unwrap()
            .get("value")
            .unwrap();
        assert_eq!(
            value
                .get("from")
                .unwrap()
                .get("x")
                .unwrap()
                .integer()
                .unwrap(),
            24
        );
        assert_eq!(
            value
                .get("from")
                .unwrap()
                .get("y")
                .unwrap()
                .integer()
                .unwrap(),
            30
        );
        assert_eq!(value.get("duration_ms").unwrap().integer().unwrap(), 160);
        Ok(())
    })
    .unwrap();
    assert!(
        accessibility::text(256, 12)
            .unwrap()
            .contains("Zuse action waiting")
    );

    for source in [
        "mouse.move_to(text=\"Zuse nonexistent target\",duration=0)",
        "mouse.move_to(text=\"Zuse hidden target\",duration=0)",
        "mouse.move_to(text=\"Zuse duplicate target\",role=\"button\",duration=0)",
    ] {
        let before = desktop.position().unwrap();
        let output = runtime.execute(source, Options::default()).unwrap();
        assert!(runtime::failed(&output), "{source}: {output}");
        assert_eq!(desktop.position().unwrap(), before);
    }

    // A speed-resolved duration is charged before its first pointer event.
    desktop.move_to(24, 30).unwrap();
    let output = runtime
        .execute(
            "mouse.move_to(text=\"Zuse action fixture\",role=\"button\",speed=300)",
            Options {
                max_wait_ms: 1,
                ..Options::default()
            },
        )
        .unwrap();
    assert!(
        runtime::failed(&output) && output.contains("wait budget"),
        "{output}"
    );
    assert_eq!(desktop.position().unwrap(), (24, 30));

    let center = Point {
        x: located.bounds.x + located.bounds.width / 2.0,
        y: located.bounds.y + located.bounds.height / 2.0,
    };
    let first_plan = MovePlan::new(
        Point { x: 24.0, y: 30.0 },
        center,
        &MoveOptions {
            speed: Some(300.0),
            ..MoveOptions::default()
        },
    )
    .unwrap();
    let output = runtime.execute(
        "mouse.move_to(text=\"Zuse action fixture\",role=\"button\",speed=300);mouse.move_to(text=\"Zuse duplicate target\",role=\"button\",index=1,speed=300);wait(ms=10)",
        Options { max_wait_ms: u64::from(first_plan.duration_ms()) + 10, ..Options::default() },
    ).unwrap();
    assert!(
        runtime::failed(&output) && output.contains("wait budget"),
        "{output}"
    );
    assert!(output.contains("\"failed_step\":1"), "{output}");
    assert_eq!(
        desktop.position().unwrap(),
        (center.x.round() as i32, center.y.round() as i32)
    );
    desktop.move_to(24, 30).unwrap();

    let query = TargetQuery::button("Zuse action fixture");
    let receipt = runtime
        .move_to_with(
            &query,
            &MoveOptions {
                duration_ms: Some(120),
                jitter: 1.0,
                ..MoveOptions::default()
            },
            |t, from, to| {
                if t == 0.0 {
                    return from;
                }
                if t == 1.0 {
                    return to;
                }
                Point {
                    x: from.x + (to.x - from.x) * t,
                    y: from.y + (to.y - from.y) * t + 80.0 * t * (1.0 - t),
                }
            },
        )
        .unwrap();
    assert_eq!(receipt.from, Point { x: 24.0, y: 30.0 });
    assert_eq!(
        desktop.position().unwrap(),
        (receipt.to.x.round() as i32, receipt.to.y.round() as i32)
    );
    let before = desktop.position().unwrap();
    assert!(
        runtime
            .move_to_with(&query, &MoveOptions::default(), |t, from, _| {
                if t == 0.0 {
                    from
                } else {
                    Point {
                        x: f64::NAN,
                        y: 0.0,
                    }
                }
            })
            .is_err()
    );
    assert_eq!(desktop.position().unwrap(), before);

    // Exercise the external CLI, including explicit duplicate selection.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(["eval", "mouse.move_to(text=\"Zuse duplicate target\",role=\"button\",index=1,speed=800,curve=\"linear\")"])
        .output().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let selected = target::locate(&TargetQuery {
        index: Some(1),
        ..TargetQuery::button("Zuse duplicate target")
    })
    .unwrap();
    assert_eq!(
        desktop.position().unwrap(),
        (
            (selected.bounds.x + selected.bounds.width / 2.0).round() as i32,
            (selected.bounds.y + selected.bounds.height / 2.0).round() as i32,
        )
    );
    let mut engine = zuse::mcp::Engine::new(Options::default());
    let request = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"zuse.execute","arguments":{"code":"mouse.move_to(text=\"Zuse action waiting\",duration=0)"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let response = String::from_utf8(engine.handle(request).unwrap().unwrap()).unwrap();
    json::with_value(&response, |root| {
        assert_eq!(
            root.get("result")
                .unwrap()
                .get("isError")
                .unwrap()
                .boolean(),
            Some(false),
            "{response}"
        );
        Ok(())
    })
    .unwrap();
    let label = target::locate(&TargetQuery::text("Zuse action waiting")).unwrap();
    assert_eq!(
        desktop.position().unwrap(),
        (
            (label.bounds.x + label.bounds.width / 2.0).round() as i32,
            (label.bounds.y + label.bounds.height / 2.0).round() as i32,
        )
    );
    println!(
        "target motion: native runtime, current start, offsets, timing, no-click, missing/hidden/ambiguous targets, cumulative speed budget, callback preflight, CLI buttons and MCP text passed"
    );
}
