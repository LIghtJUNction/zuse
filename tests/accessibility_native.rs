#![cfg(all(feature = "std", target_os = "linux"))]
use zmcpio::json::{Document, Limits, Token};
use zuse::accessibility;

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
