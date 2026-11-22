#![cfg(all(feature = "std", target_os = "linux"))]

use std::time::{Duration, Instant};
use zuse::{
    accessibility, json,
    observation::Cancellation,
    platform::Desktop,
    runtime::{self, Options, Runtime},
};

fn named_node(name: &str) -> (String, u32) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = accessibility::snapshot(256, 12).unwrap();
        let found = json::with_value(&snapshot, |root| {
            assert_eq!(root.get("truncated").unwrap().boolean(), Some(false));
            assert_eq!(root.get("errors").unwrap().integer().unwrap(), 0);
            let matches = root
                .get("nodes")
                .unwrap()
                .elements()
                .filter(|node| {
                    node.get("name")
                        .and_then(|value| value.string())
                        .is_some_and(|value| value.matches(name))
                })
                .collect::<Vec<_>>();
            assert!(matches.len() <= 1, "fixture target must be unique");
            matches
                .first()
                .map(|node| {
                    Ok((
                        json::text(node.get("path").unwrap())?,
                        u32::try_from(node.get("pid").unwrap().integer().unwrap()).unwrap(),
                    ))
                })
                .transpose()
        })
        .unwrap();
        if let Some(found) = found {
            return found;
        }
        assert!(Instant::now() < deadline, "fixture target missing: {name}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn text_at(path: &str) -> String {
    json::with_value(&accessibility::node(path).unwrap(), |node| {
        json::text(node.get("text").unwrap())
    })
    .unwrap()
}

fn focused_windows(desktop: &Desktop) -> Vec<String> {
    json::with_value(&desktop.windows().unwrap(), |root| {
        let mut ids = root
            .elements()
            .filter(|window| window.get("focused").and_then(|value| value.boolean()) == Some(true))
            .map(|window| json::text(window.get("id").unwrap()))
            .collect::<Result<Vec<_>, _>>()?;
        ids.sort();
        Ok(ids)
    })
    .unwrap()
}

fn role_at(path: &str) -> String {
    json::with_value(&accessibility::node(path).unwrap(), |node| {
        json::text(node.get("role").unwrap())
    })
    .unwrap()
}

fn focused_at(path: &str) -> bool {
    json::with_value(&accessibility::node(path).unwrap(), |node| {
        Ok(node.get("focused").unwrap().boolean() == Some(true))
    })
    .unwrap()
}

fn assert_scoped_controls(editor: &str, button: &str, pid: u32) {
    let window = editor.split('/').take(3).collect::<Vec<_>>().join("/");
    for scope in [window.as_str(), editor] {
        let source = format!(
            "accessibility.tree(scope={},limit=4096,depth=64)",
            json::quote(scope),
        );
        let result = Runtime::new()
            .execute(
                &source,
                Options {
                    read_only: true,
                    ..Options::default()
                },
            )
            .unwrap();
        assert!(!runtime::failed(&result), "{result}");
        json::with_value(&result, |root| {
            let snapshot = root
                .get("results")
                .unwrap()
                .elements()
                .next()
                .unwrap()
                .get("value")
                .unwrap();
            assert_eq!(snapshot.get("truncated").unwrap().boolean(), Some(false));
            assert_eq!(snapshot.get("errors").unwrap().integer().unwrap(), 0);
            let mut paths = Vec::new();
            for node in snapshot.get("nodes").unwrap().elements() {
                let path = json::text(node.get("path").unwrap())?;
                assert!(path == scope || path.starts_with(&format!("{scope}/")));
                assert_eq!(node.get("pid").unwrap().integer().unwrap(), i64::from(pid));
                paths.push(path);
            }
            assert!(paths.iter().any(|path| path == editor));
            assert_eq!(paths.iter().any(|path| path == button), scope == window);
            Ok(())
        })
        .unwrap();
    }
}

fn reject_control_identity_mismatches(editor: &str, button: &str, label: &str, pid: u32) {
    let button_role = role_at(button);
    let editor_role = role_at(editor);
    let wrong_pid = if pid == u32::MAX { 1 } else { pid + 1 };
    let before = text_at(editor);
    accessibility::focus_checked(
        editor,
        Some(pid),
        Some(&editor_role),
        Some("Zuse editable fixture"),
    )
    .unwrap();
    assert!(focused_at(editor));
    for (expected_pid, expected_role, expected_name, field) in [
        (
            wrong_pid,
            button_role.as_str(),
            "Zuse action fixture",
            "expected_pid",
        ),
        (
            pid,
            "incorrect role",
            "Zuse action fixture",
            "expected_role",
        ),
        (
            pid,
            button_role.as_str(),
            "Zuse action Fixture",
            "expected_name",
        ),
        (pid, button_role.as_str(), "", "expected_name"),
    ] {
        let error = accessibility::action_checked(
            button,
            0,
            Some(expected_pid),
            Some(expected_role),
            Some(expected_name),
        )
        .unwrap_err();
        assert!(
            error.starts_with("stale_target:") && error.contains(field),
            "{error}"
        );
        assert_eq!(text_at(label), "Zuse action waiting");
    }
    for (expected_pid, expected_role, expected_name, field) in [
        (
            wrong_pid,
            button_role.as_str(),
            "Zuse action fixture",
            "expected_pid",
        ),
        (
            pid,
            "incorrect role",
            "Zuse action fixture",
            "expected_role",
        ),
        (
            pid,
            button_role.as_str(),
            "Zuse action Fixture",
            "expected_name",
        ),
    ] {
        let error = accessibility::focus_checked(
            button,
            Some(expected_pid),
            Some(expected_role),
            Some(expected_name),
        )
        .unwrap_err();
        assert!(
            error.starts_with("stale_target:") && error.contains(field),
            "{error}"
        );
        assert!(focused_at(editor));
        assert!(!focused_at(button));
        assert_eq!(text_at(editor), before);
        assert_eq!(text_at(label), "Zuse action waiting");
    }

    // Exercise the DSL dispatch too: dropping an expected field between
    // preflight and the native call would trigger the real button below.
    for operation in ["accessibility.action", "accessibility.focus"] {
        let source = format!(
            "{operation}(path={},expected_pid={pid},expected_role={},expected_name=\"different control\");accessibility.action(path={})",
            json::quote(button),
            json::quote(&button_role),
            json::quote(button),
        );
        let result = Runtime::new().execute(&source, Options::default()).unwrap();
        assert!(
            runtime::failed(&result) && result.contains("stale_target"),
            "{result}"
        );
        assert!(focused_at(editor));
        assert!(!focused_at(button));
        assert_eq!(text_at(label), "Zuse action waiting");
    }

    for source in [
        format!(
            "accessibility.action(path={},index=0,expected_pid={pid},expected_role={},expected_name=\"Zuse action fixture\")",
            json::quote(button),
            json::quote(&button_role),
        ),
        format!(
            "accessibility.focus(path={},expected_pid={pid},expected_role={},expected_name=\"Zuse action fixture\")",
            json::quote(button),
            json::quote(&button_role),
        ),
    ] {
        let mut denied = Runtime::new();
        denied.set_cancellation(Cancellation::with_dispatch_guard(|| {
            Err("fixture input permission revoked".into())
        }));
        let result = denied.execute(&source, Options::default()).unwrap();
        assert!(
            runtime::failed(&result) && result.contains("permission revoked"),
            "{result}"
        );
        let mut cancelled = Runtime::new();
        cancelled.cancellation().cancel();
        let result = cancelled.execute(&source, Options::default()).unwrap();
        assert!(
            runtime::failed(&result) && result.contains("cancelled"),
            "{result}"
        );
        assert!(focused_at(editor));
        assert!(!focused_at(button));
        assert_eq!(text_at(editor), before);
        assert_eq!(text_at(label), "Zuse action waiting");
    }
}

fn accept_guarded_controls(editor: &str, pid: u32) {
    // These existing duplicate buttons have no clicked callback. Do not change
    // the shared waiting label: the other native test also uses it to detect
    // accidental input, and independent tests must not depend on run order.
    let duplicate = json::with_value(&accessibility::snapshot(256, 12).unwrap(), |root| {
        let node = root
            .get("nodes")
            .unwrap()
            .elements()
            .find(|node| {
                node.get("name")
                    .and_then(|name| name.string())
                    .is_some_and(|name| name.matches("Zuse duplicate target"))
            })
            .unwrap();
        json::text(node.get("path").unwrap())
    })
    .unwrap();
    let result = accessibility::action_checked(
        &duplicate,
        0,
        Some(pid),
        Some(&role_at(&duplicate)),
        Some("Zuse duplicate target"),
    )
    .unwrap();
    json::with_value(&result, |root| {
        assert_eq!(root.get("completed").unwrap().boolean(), Some(true));
        Ok(())
    })
    .unwrap();
    accessibility::focus_checked(
        &duplicate,
        Some(pid),
        Some(&role_at(&duplicate)),
        Some("Zuse duplicate target"),
    )
    .unwrap();
    assert!(focused_at(&duplicate));
    assert!(!focused_at(editor));
    let result = accessibility::focus_checked(
        editor,
        Some(pid),
        Some(&role_at(editor)),
        Some("Zuse editable fixture"),
    )
    .unwrap();
    json::with_value(&result, |root| {
        assert_eq!(root.get("focused").unwrap().boolean(), Some(true));
        Ok(())
    })
    .unwrap();
    assert!(focused_at(editor));
    assert!(!focused_at(&duplicate));
}

#[test]
#[ignore = "run tests/run_accessibility_native.py --test-target accessibility_guard_native"]
fn conditional_draft_edits_use_the_native_object_and_preserve_other_actions() {
    assert_eq!(
        std::env::var("ZUSE_ATSPI_GUARD_FIXTURE").as_deref(),
        Ok("1")
    );
    let (editor, pid) = named_node("Zuse editable fixture");
    let (button, _) = named_node("Zuse action fixture");
    let (label, _) = named_node("Zuse action waiting");
    let (password, _) = named_node("Zuse secret fixture");
    let desktop = Desktop::connect().unwrap();

    assert_scoped_controls(&editor, &button, pid);
    reject_control_identity_mismatches(&editor, &button, &label, pid);
    let focus = focused_windows(&desktop);

    for existing in ["existing draft", " "] {
        accessibility::set_text(&editor, existing).unwrap();
        let error = accessibility::set_text_checked(&editor, "replacement", Some(""), Some(pid))
            .unwrap_err();
        assert!(error.contains("precondition failed"), "{error}");
        assert_eq!(text_at(&editor), existing);
    }
    accessibility::set_text(&editor, "").unwrap();
    let wrong_pid = if pid == u32::MAX { 1 } else { pid + 1 };
    assert!(
        accessibility::set_text_checked(&editor, "replacement", Some(""), Some(wrong_pid))
            .unwrap_err()
            .contains("expected_pid")
    );
    assert!(text_at(&editor).is_empty());
    assert!(
        accessibility::set_text_checked(&button, "replacement", Some(""), Some(pid)).is_err(),
        "a node without readable editable text must not pass an empty-draft check"
    );
    assert!(
        accessibility::set_text_checked(&password, "replacement", Some(""), Some(pid)).is_err()
    );

    let mut denied = Runtime::new();
    denied.set_cancellation(Cancellation::with_dispatch_guard(|| {
        Err("fixture input permission revoked".into())
    }));
    let source = format!(
        "accessibility.set_text(path={},text=\"replacement\",expected_text=\"\",expected_pid={pid})",
        json::quote(&editor),
    );
    let result = denied.execute(&source, Options::default()).unwrap();
    assert!(
        runtime::failed(&result) && result.contains("permission revoked"),
        "{result}"
    );
    assert!(text_at(&editor).is_empty());

    let mut cancelled = Runtime::new();
    cancelled.cancellation().cancel();
    assert!(runtime::failed(
        &cancelled.execute(&source, Options::default()).unwrap()
    ));
    assert!(text_at(&editor).is_empty());

    // Check beyond the 4096-character observation preview. Preconditions must
    // compare the full editable value, not a clipped snapshot string.
    let long = format!("{}中文", "x".repeat(5000));
    let result = accessibility::set_text_checked(&editor, &long, Some(""), Some(pid)).unwrap();
    json::with_value(&result, |root| {
        assert_eq!(json::text(root.get("text").unwrap())?, long);
        assert_eq!(root.get("verified").unwrap().boolean(), Some(true));
        Ok(())
    })
    .unwrap();
    accessibility::set_text_checked(&editor, "", Some(&long), Some(pid)).unwrap();

    // A real GTK changed callback normalizes this input during set_text.
    // Readback failure must stop the following action and describe submission.
    let mut runtime = Runtime::new();
    let mismatch = runtime
        .execute(
            &format!(
                "accessibility.set_text(path={},text=\"zuse-normalize-fixture\",expected_text=\"\",expected_pid={pid});accessibility.action(path={},index=0)",
                json::quote(&editor), json::quote(&button),
            ),
            Options::default(),
        )
        .unwrap();
    assert!(runtime::failed(&mismatch), "{mismatch}");
    assert!(
        mismatch.contains("submitted") && mismatch.contains("readback differs"),
        "{mismatch}"
    );
    assert_eq!(text_at(&editor), "ZUSE NORMALIZED");
    assert_eq!(text_at(&label), "Zuse action waiting");
    accessibility::set_text_checked(&editor, "", Some("ZUSE NORMALIZED"), Some(pid)).unwrap();

    let module = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("skills/zuse/assets/chatgpt-desktop.zuse");
    let result = runtime
        .execute(
            &format!(
                "import(path={},as=\"chatgpt\");chatgpt.prepare_draft(editor={},pid={pid},message=\"native fixture draft 中文\")",
                json::quote(module.to_str().unwrap()), json::quote(&editor),
            ),
            Options::default(),
        )
        .unwrap();
    assert!(!runtime::failed(&result), "{result}");
    assert_eq!(text_at(&editor), "native fixture draft 中文");
    assert_eq!(
        text_at(&label),
        "Zuse action waiting",
        "preparation must not send"
    );
    assert_eq!(
        focused_windows(&desktop),
        focus,
        "editing must not change native focus"
    );
    accept_guarded_controls(&editor, pid);
}

#[test]
#[ignore = "run tests/run_accessibility_native.py --test-target accessibility_guard_native"]
fn cancelling_a_submitted_edit_does_not_wait_for_the_gtk_reply() {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
    };

    assert_eq!(
        std::env::var("ZUSE_ATSPI_GUARD_FIXTURE").as_deref(),
        Ok("1")
    );
    let marker = PathBuf::from(std::env::var_os("ZUSE_ATSPI_DELAY_MARKER").unwrap());
    let done = PathBuf::from(format!("{}.done", marker.to_str().unwrap()));
    assert!(
        !marker.exists() && !done.exists(),
        "fixture markers must be fresh"
    );
    let (editor, pid) = named_node("Zuse editable fixture");
    let (button, _) = named_node("Zuse action fixture");
    let (label, _) = named_node("Zuse action waiting");
    accessibility::set_text(&editor, "").unwrap();

    struct Permit(Arc<AtomicBool>);
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let held = Arc::new(AtomicBool::new(false));
    let acquire = held.clone();
    let token = Cancellation::with_dispatch_guard(move || {
        acquire.store(true, Ordering::Release);
        Ok(Box::new(Permit(acquire.clone())))
    });
    let worker_token = token.clone();
    let source = format!(
        "accessibility.set_text(path={},text=\"zuse-delayed-fixture\",expected_text=\"\",expected_pid={pid});accessibility.action(path={},index=0)",
        json::quote(&editor),
        json::quote(&button),
    );
    let (result_tx, result_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut runtime = Runtime::new();
        runtime.set_cancellation(worker_token);
        result_tx
            .send(runtime.execute(&source, Options::default()))
            .unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        if let Ok(result) = result_rx.try_recv() {
            worker.join().unwrap();
            panic!("edit returned before entering the delayed GTK callback: {result:?}");
        }
        assert!(Instant::now() < deadline, "GTK callback was not entered");
        std::thread::sleep(Duration::from_millis(2));
    }
    let permit_held_during_reply = held.load(Ordering::Acquire);
    let cancel_started = Instant::now();
    token.cancel();
    let cancel_elapsed = cancel_started.elapsed();
    let result = result_rx.recv_timeout(Duration::from_millis(300));
    let returned_before_reply = !done.exists();
    // Drain the owned callback before reading its text or finishing this test.
    // Cancellation stops local waiting; it does not undo the submitted edit.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !done.exists() {
        assert!(Instant::now() < deadline, "GTK callback did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.join().unwrap();
    assert!(
        !permit_held_during_reply,
        "input permit outlived submission"
    );
    assert!(
        cancel_elapsed < Duration::from_millis(250),
        "cancel waited {cancel_elapsed:?}"
    );
    let result = result
        .expect("cancelled edit did not return promptly")
        .unwrap();
    assert!(
        returned_before_reply,
        "edit waited for the delayed GTK reply"
    );
    assert!(runtime::failed(&result), "{result}");
    assert!(
        result.contains("cancelled") && result.contains("submitted"),
        "{result}"
    );
    assert_eq!(text_at(&editor), "zuse-delayed-fixture");
    assert_eq!(
        text_at(&label),
        "Zuse action waiting",
        "cancel must stop subsequent input"
    );
    accessibility::set_text_checked(&editor, "", Some("zuse-delayed-fixture"), Some(pid)).unwrap();
}
