#![cfg(all(feature = "std", target_os = "linux"))]

use std::{
    thread,
    time::{Duration, Instant},
};
use zuse::{
    accessibility::{
        self,
        controls::{self, Operation, Query},
    },
    json,
    observation::Cancellation,
};

#[derive(Debug)]
struct Node {
    path: String,
    pid: u32,
    role: String,
    name: String,
    actions: Vec<String>,
    visible: Option<bool>,
    showing: Option<bool>,
}

fn nodes(input: &str, field: &str) -> Vec<Node> {
    json::with_value(input, |root| {
        root.get(field)
            .unwrap()
            .elements()
            .map(|node| {
                Ok(Node {
                    path: json::text(node.get("path").unwrap())?,
                    pid: u32::try_from(node.get("pid").unwrap().integer().unwrap()).unwrap(),
                    role: json::text(node.get("role").unwrap())?,
                    name: json::text(node.get("name").unwrap())?,
                    actions: node
                        .get("actions")
                        .unwrap()
                        .elements()
                        .map(|action| json::text(action.get("name").unwrap()))
                        .collect::<Result<Vec<_>, _>>()?,
                    visible: node.get("visible").and_then(|value| value.boolean()),
                    showing: node.get("showing").and_then(|value| value.boolean()),
                })
            })
            .collect()
    })
    .unwrap()
}

fn text_at(path: &str) -> String {
    json::with_value(&accessibility::node(path).unwrap(), |node| {
        json::text(node.get("text").unwrap())
    })
    .unwrap()
}

fn focused_at(path: &str) -> bool {
    json::with_value(&accessibility::node(path).unwrap(), |node| {
        Ok(node.get("focused").unwrap().boolean() == Some(true))
    })
    .unwrap()
}

fn assert_scoped_label(scope: &str, path: &str, expected: &str) {
    let snapshot = accessibility::snapshot_scope(scope, 4096, 64).unwrap();
    json::with_value(&snapshot, |root| {
        assert_eq!(root.get("truncated").unwrap().boolean(), Some(false));
        assert_eq!(root.get("errors").unwrap().integer().unwrap(), 0);
        let node = root
            .get("nodes")
            .unwrap()
            .elements()
            .find(|node| {
                node.get("path")
                    .and_then(|value| value.string())
                    .is_some_and(|value| value.matches(path))
            })
            .expect("scoped snapshot must contain the descendant label");
        assert_eq!(json::text(node.get("name").unwrap())?, expected);
        assert_eq!(json::text(node.get("text").unwrap())?, expected);
        Ok(())
    })
    .unwrap();
}

fn target_path(receipt: &str) -> String {
    json::with_value(receipt, |root| {
        assert!(
            root.get("status")
                .and_then(|value| value.string())
                .is_some()
        );
        assert!(root.get("elapsed_ms").unwrap().integer().unwrap() >= 0);
        json::text(root.get("target").unwrap().get("path").unwrap())
    })
    .unwrap()
}

#[test]
#[ignore = "run this compiled test binary with tests/run_accessibility_native.py --test-binary in a fresh isolated fixture"]
fn semantic_controls_enumerate_select_focus_wait_and_cancel_without_coordinates() {
    assert_eq!(std::env::var("ZUSE_ATSPI_FIXTURE").as_deref(), Ok("1"));
    let deadline = Instant::now() + Duration::from_secs(8);
    let fixture = loop {
        let snapshot = accessibility::snapshot(256, 16).unwrap();
        // Skip the registry root: its PID is null by design.
        let filtered = json::with_value(&snapshot, |root| {
            Ok(format!(
                "{{\"nodes\":[{}]}}",
                root.get("nodes")
                    .unwrap()
                    .elements()
                    .filter(|node| node
                        .get("pid")
                        .and_then(|value| value.integer().ok())
                        .is_some())
                    .map(|node| node.raw().to_owned())
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        })
        .unwrap();
        let all = nodes(&filtered, "nodes");
        if all.iter().any(|node| node.name == "Zuse editable fixture") {
            break all;
        }
        assert!(
            Instant::now() < deadline,
            "isolated GTK fixture did not appear"
        );
        thread::sleep(Duration::from_millis(40));
    };
    let editor = fixture
        .iter()
        .find(|node| node.name == "Zuse editable fixture")
        .unwrap();
    let button = fixture
        .iter()
        .find(|node| node.name == "Zuse action fixture" && !node.actions.is_empty())
        .unwrap();
    let label = fixture
        .iter()
        .find(|node| node.name == "Zuse action waiting")
        .unwrap();
    let scope = editor.path.split('/').take(3).collect::<Vec<_>>().join("/");
    let pid = editor.pid;
    let base = Query {
        scope: &scope,
        expected_pid: pid,
        role: None,
        name: None,
        text: None,
        exact: true,
    };
    let cancellation = Cancellation::default();

    let discovered = accessibility::windows(pid, &cancellation).unwrap();
    json::with_value(&discovered, |root| {
        assert_eq!(root.get("truncated").unwrap().boolean(), Some(false));
        assert_eq!(root.get("errors").unwrap().integer().unwrap(), 0);
        let windows = root.get("windows").unwrap().elements().collect::<Vec<_>>();
        assert!(!windows.is_empty());
        assert!(
            windows
                .iter()
                .all(|window| window.get("pid").unwrap().integer().unwrap() == i64::from(pid))
        );
        assert!(windows.iter().any(|window| {
            window
                .get("scope")
                .and_then(|value| value.string())
                .is_some_and(|value| value.matches(&scope))
        }));
        Ok(())
    })
    .unwrap();

    let all = controls::find(&base, &cancellation).unwrap();
    let all_nodes = nodes(&all, "matches");
    assert!(all_nodes.iter().any(|node| node.path == editor.path));
    assert!(all_nodes.iter().any(|node| node.path == button.path));
    assert!(all_nodes.iter().all(|node| node.pid == pid
        && (node.path == scope || node.path.starts_with(&format!("{scope}/")))));
    json::with_value(&all, |root| {
        assert_eq!(
            root.get("count").unwrap().integer().unwrap(),
            all_nodes.len() as i64
        );
        assert_eq!(root.get("truncated").unwrap().boolean(), Some(false));
        assert_eq!(root.get("errors").unwrap().integer().unwrap(), 0);
        Ok(())
    })
    .unwrap();
    let wrong_pid = Query {
        expected_pid: if pid == u32::MAX { 1 } else { pid + 1 },
        ..base
    };
    assert!(
        controls::find(&wrong_pid, &cancellation)
            .unwrap_err()
            .starts_with("stale_target:")
    );
    assert!(
        controls::invoke(
            &Query {
                role: Some(&button.role),
                name: Some(&button.name),
                ..wrong_pid
            },
            Operation::Reveal,
            None,
            1000,
            &cancellation
        )
        .unwrap_err()
        .starts_with("stale_target:")
    );

    let duplicate = Query {
        role: Some(&button.role),
        name: Some("Zuse duplicate target"),
        ..base
    };
    let matches = nodes(
        &controls::find(&duplicate, &cancellation).unwrap(),
        "matches",
    );
    assert_eq!(matches.len(), 2);
    for name in ["duplicate", "zuse duplicate target"] {
        assert!(
            nodes(
                &controls::find(
                    &Query {
                        name: Some(name),
                        ..duplicate
                    },
                    &cancellation
                )
                .unwrap(),
                "matches"
            )
            .is_empty()
        );
    }
    assert_eq!(
        nodes(
            &controls::find(
                &Query {
                    name: Some("duplicate"),
                    exact: false,
                    ..duplicate
                },
                &cancellation
            )
            .unwrap(),
            "matches"
        )
        .len(),
        2
    );
    let wrong_role = button.role.to_uppercase();
    assert!(
        nodes(
            &controls::find(
                &Query {
                    role: Some(&wrong_role),
                    exact: false,
                    ..duplicate
                },
                &cancellation
            )
            .unwrap(),
            "matches"
        )
        .is_empty()
    );
    let action = matches[0]
        .actions
        .first()
        .expect("GTK button advertises an action");
    let error = controls::invoke(
        &duplicate,
        Operation::Action(action),
        None,
        1000,
        &cancellation,
    )
    .unwrap_err();
    assert!(error.starts_with("ambiguous_target:"), "{error}");
    let error =
        controls::invoke(&duplicate, Operation::Reveal, None, 1000, &cancellation).unwrap_err();
    assert!(error.starts_with("ambiguous_target:"), "{error}");
    let invoked = controls::invoke(
        &duplicate,
        Operation::Action(action),
        Some(1),
        1000,
        &cancellation,
    )
    .unwrap();
    assert_eq!(target_path(&invoked), matches[1].path);
    assert!(
        controls::invoke(
            &duplicate,
            Operation::Action(action),
            Some(2),
            1000,
            &cancellation
        )
        .is_err()
    );
    assert_eq!(text_at(&label.path), "Zuse action waiting");

    let unique = Query {
        role: Some(&button.role),
        name: Some("Zuse action fixture"),
        ..base
    };
    // An already showing component needs no native scroll and must not invoke
    // its advertised click action while satisfying reveal.
    let revealed = controls::invoke(&unique, Operation::Reveal, None, 1000, &cancellation).unwrap();
    assert_eq!(target_path(&revealed), button.path);
    json::with_value(&revealed, |root| {
        assert_eq!(root.get("scroll_submitted").unwrap().boolean(), Some(false));
        assert_eq!(
            root.get("target")
                .unwrap()
                .get("showing")
                .unwrap()
                .boolean(),
            Some(true)
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(text_at(&label.path), "Zuse action waiting");
    let action = button.actions.first().unwrap();
    let error = controls::invoke(
        &unique,
        Operation::Action("zuse-no-such-action"),
        None,
        1000,
        &cancellation,
    )
    .unwrap_err();
    assert!(error.contains("unsupported"), "{error}");
    assert_eq!(text_at(&label.path), "Zuse action waiting");
    let editor_query = Query {
        role: Some(&editor.role),
        name: Some("Zuse editable fixture"),
        ..base
    };
    let focused =
        controls::invoke(&editor_query, Operation::Focus, None, 1000, &cancellation).unwrap();
    assert_eq!(target_path(&focused), editor.path);
    assert!(focused_at(&editor.path));

    let denied =
        Cancellation::with_dispatch_guard(|| Err("semantic fixture permission revoked".into()));
    let error =
        controls::invoke(&unique, Operation::Action(action), None, 1000, &denied).unwrap_err();
    assert!(error.contains("permission revoked"), "{error}");
    assert_eq!(text_at(&label.path), "Zuse action waiting");

    // Acquiring a host input permit can itself outlive the operation budget.
    // Recheck the deadline after acquisition, before sending native input.
    let guard_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let called = guard_called.clone();
    let delayed_permission = Cancellation::with_dispatch_guard(move || {
        called.store(true, std::sync::atomic::Ordering::Release);
        thread::sleep(Duration::from_millis(350));
        Ok(Box::new(()))
    });
    let error = controls::invoke(
        &unique,
        Operation::Action(action),
        None,
        300,
        &delayed_permission,
    )
    .unwrap_err();
    assert!(
        guard_called.load(std::sync::atomic::Ordering::Acquire),
        "test did not reach permission acquisition: {error}"
    );
    assert!(error.starts_with("timeout:"), "{error}");
    assert_eq!(
        text_at(&label.path),
        "Zuse action waiting",
        "expired permission acquisition must not send input"
    );

    let waiting = Query {
        role: Some(&label.role),
        text: Some("Zuse action waiting"),
        ..base
    };
    assert_eq!(
        nodes(&controls::wait(&waiting, 1000, &denied).unwrap(), "matches").len(),
        1
    );

    let hidden = Query {
        role: Some(&button.role),
        name: Some("Zuse hidden target"),
        ..base
    };
    let hidden_nodes = nodes(&controls::find(&hidden, &cancellation).unwrap(), "matches");
    assert_eq!(
        hidden_nodes.len(),
        1,
        "hidden controls must remain enumerable"
    );
    assert!(hidden_nodes[0].visible == Some(false) || hidden_nodes[0].showing == Some(false));
    let started = Instant::now();
    let error =
        controls::invoke(&hidden, Operation::Action(action), None, 120, &cancellation).unwrap_err();
    assert!(error.starts_with("timeout:"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "short timeout used an unrelated scan deadline"
    );
    let error = controls::invoke(&hidden, Operation::Reveal, None, 120, &cancellation).unwrap_err();
    assert!(error.starts_with("timeout:"), "{error}");
    assert_eq!(text_at(&label.path), "Zuse action waiting");

    let missing = Query {
        name: Some("Zuse semantic missing target"),
        ..base
    };
    let started = Instant::now();
    let error = controls::wait(&missing, 120, &cancellation).unwrap_err();
    assert!(error.starts_with("timeout:"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1));
    let stopping = Cancellation::default();
    let external = stopping.clone();
    let stop = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        external.cancel();
    });
    let started = Instant::now();
    let result = controls::wait(&missing, 10000, &stopping);
    stop.join().unwrap();
    let error = result.unwrap_err();
    assert!(error.starts_with("cancelled:"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(focused_at(&editor.path));
    assert_eq!(text_at(&label.path), "Zuse action waiting");

    // The fixture is fresh for this independent harness. Send its one visible
    // state-changing action last, then confirm the result through semantic text.
    // Warm the legacy scanner's descendant caches, then inspect it immediately
    // after the real action, before node()/semantic wait can refresh the label.
    assert_scoped_label(&scope, &label.path, "Zuse action waiting");
    let invoked = controls::invoke(
        &unique,
        Operation::Action(action),
        None,
        3000,
        &cancellation,
    )
    .unwrap();
    assert_eq!(target_path(&invoked), button.path);
    // GTK activation can finish on its main loop after the action reply. Give
    // it time without an intervening accessibility read of the descendant.
    thread::sleep(Duration::from_millis(500));
    assert_scoped_label(&scope, &label.path, "Zuse action completed");
    let completed = Query {
        role: Some(&label.role),
        text: Some("Zuse action completed"),
        ..base
    };
    let matched = controls::wait(&completed, 3000, &cancellation).unwrap();
    let completed_nodes = nodes(&matched, "matches");
    assert_eq!(completed_nodes.len(), 1);
    assert_eq!(completed_nodes[0].path, label.path);
    assert_eq!(text_at(&label.path), "Zuse action completed");
}
