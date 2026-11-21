#![cfg(feature = "std")]

use zuse::{
    json,
    language::Parser,
    runtime::{self, Options},
    script::{ModuleSession, Prepared, ProgramMode},
};

fn imported() -> Prepared {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("skills/zuse/assets/chatgpt-desktop.zuse");
    ModuleSession::default()
        .prepare(
            &format!(
                "import(path={},as=\"chatgpt\")",
                json::quote(path.to_str().unwrap())
            ),
            Options::default(),
        )
        .unwrap()
}

fn expand(source: &str) -> Prepared {
    imported()
        .next_session
        .prepare(source, Options::default())
        .unwrap()
}

fn readonly() -> Options {
    Options {
        read_only: true,
        ..Options::default()
    }
}

#[test]
fn import_is_effect_free_and_exports_documented_functions() {
    let prepared = imported();
    assert_eq!(prepared.mode, ProgramMode::Flat);
    assert!(prepared.source.is_empty());
    runtime::check(&prepared.source, readonly()).unwrap();
    let metadata = prepared.next_session.function_metadata();
    assert_eq!(metadata.len(), 6);
    assert!(metadata.iter().all(|f| !f.description.is_empty()));
    assert!(
        metadata
            .iter()
            .find(|f| f.name == "chatgpt.prepare_draft")
            .unwrap()
            .description
            .contains("exact empty editor")
    );
}

#[test]
fn observations_preflight_under_read_only_policy() {
    let prepared = expand(
        "chatgpt.windows();\n\
         chatgpt.inspect_window(scope=\"0/1/0\");\n\
         chatgpt.read_conversation(scope=\"0/1/0/2\");\n\
         chatgpt.read_draft(editor=\"0/1/0/3\")",
    );
    runtime::check(&prepared.source, readonly()).unwrap();
}

#[test]
fn draft_and_send_require_mutation_permission() {
    for source in [
        "chatgpt.prepare_draft(editor=\"0/1/0/3\",pid=42,message=\"fixture draft\")",
        "chatgpt.send_prepared(send_button=\"0/1/0/4\",action_index=0)",
    ] {
        let prepared = expand(source);
        runtime::check(&prepared.source, Options::default()).unwrap();
        assert!(
            runtime::check(&prepared.source, readonly())
                .unwrap_err()
                .contains("read-only")
        );
    }
}

#[test]
fn draft_text_stays_literal_and_preparation_never_sends() {
    let message = "fixture\");accessibility.action(path=\"0/1/0/4\",index=0);#\n中文";
    let prepared = expand(&format!(
        "chatgpt.prepare_draft(editor=\"0/1/0/3\",pid=42,message={})",
        json::quote(message)
    ));
    let calls = Parser::new(&prepared.source)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(calls.len(), 2);
    let edit = &calls[0];
    assert_eq!(edit.name, "accessibility.set_text");
    let expected = edit.get("expected_text").unwrap();
    let mut empty = vec![0; expected.decoded_len().unwrap()];
    assert_eq!(expected.decode_text(&mut empty).unwrap(), "");
    assert_eq!(edit.get("expected_pid").unwrap().as_i64(), Some(42));
    let text = edit.get("text").unwrap();
    let mut bytes = vec![0; text.decoded_len().unwrap()];
    assert_eq!(text.decode_text(&mut bytes).unwrap(), message);
    assert_eq!(calls[1].name, "observe");
    assert!(!calls.iter().any(|call| call.name == "accessibility.action"));
}

#[test]
fn helpers_never_focus_or_send_shared_keyboard_pointer_events() {
    let prepared = expand(
        "chatgpt.windows();\n\
         chatgpt.inspect_window(scope=\"0/1/0\");\n\
         chatgpt.read_conversation(scope=\"0/1/0/2\");\n\
         chatgpt.read_draft(editor=\"0/1/0/3\");\n\
         chatgpt.prepare_draft(editor=\"0/1/0/3\",pid=42,message=\"fixture draft\");\n\
         chatgpt.send_prepared(send_button=\"0/1/0/4\",action_index=0)",
    );
    for call in Parser::new(&prepared.source) {
        let call = call.unwrap();
        assert!(
            matches!(
                call.name,
                "window.list" | "observe" | "accessibility.set_text" | "accessibility.action"
            ),
            "unexpected input primitive: {}",
            call.name
        );
    }
}

#[test]
fn send_requires_a_caller_supplied_action_and_expands_once() {
    let session = imported().next_session;
    assert!(
        session
            .prepare(
                "chatgpt.send_prepared(send_button=\"0/1/0/4\")",
                Options::default(),
            )
            .is_err()
    );
    let prepared = session
        .prepare(
            "chatgpt.send_prepared(send_button=\"0/1/0/4\",action_index=2)",
            Options::default(),
        )
        .unwrap();
    let calls = Parser::new(&prepared.source)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "accessibility.action");
    assert!(calls[0].get("index").is_some());
}

#[test]
fn guarded_text_arguments_are_validated_without_a_desktop() {
    runtime::check(
        "accessibility.set_text(path=\"0/1/0/3\",text=\"draft\",expected_text=\"\",expected_pid=42)",
        Options::default(),
    )
    .unwrap();
    for source in [
        "accessibility.set_text(path=\"0/1/0/3\",text=\"draft\",expected_pid=0)",
        "accessibility.set_text(path=\"0/1/0/3\",text=\"draft\",expected_pid=-1)",
        "accessibility.set_text(path=\"0/1/0/3\",text=\"draft\",expected_pid=4294967296)",
        "accessibility.set_text(path=\"0/1/0/3\",text=\"draft\",expected_text=true)",
        "accessibility.set_text(path=\"0/1/0/3\",text=\"draft\",expected_text=\"\\u0000\")",
        "accessibility.set_text(path=\"0/-1\",text=\"draft\",expected_text=\"\")",
    ] {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
    }
}
