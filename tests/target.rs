#![cfg(feature = "std")]

use zuse::target::{Bounds, TargetQuery, locate_in, validate_query};

const RECT: &str = r#"{"x":-120.5,"y":30,"width":80,"height":24}"#;

fn node(path: &str, role: &str, name: &str, text: &str, bounds: &str) -> String {
    format!(
        r#"{{"path":{path:?},"role":{role:?},"name":{name:?},"text":{text:?},"bounds":{bounds}}}"#
    )
}

fn snapshot(nodes: &[String], truncated: bool, errors: usize) -> String {
    format!(
        r#"{{"nodes":[{}],"truncated":{truncated},"errors":{errors}}}"#,
        nodes.join(",")
    )
}

#[test]
fn constructors_use_exact_case_sensitive_matching() {
    let query = TargetQuery::text("Save");
    assert!(query.exact);
    assert!(query.case_sensitive);
    assert_eq!(query.role, None);
    assert_eq!(query.scope, None);
    assert_eq!(query.index, None);
    assert_eq!(TargetQuery::button("Save").role, Some("button"));
}

#[test]
fn button_roles_match_across_native_backends() {
    for role in ["button", "push button", "ControlType.Button", "AXButton"] {
        let input = snapshot(&[node("0/1/0", role, "Save", "", RECT)], false, 0);
        let target = locate_in(&input, &TargetQuery::button("Save")).unwrap();
        assert_eq!(target.role, role);
        assert_eq!(target.path, "0/1/0");
        assert_eq!(
            target.bounds,
            Bounds {
                x: -120.5,
                y: 30.0,
                width: 80.0,
                height: 24.0,
            }
        );
    }
}

#[test]
fn button_query_excludes_labels_with_the_same_text() {
    let input = snapshot(
        &[
            node("0/1", "label", "Save", "", RECT),
            node("0/2", "push button", "Save", "", RECT),
        ],
        false,
        0,
    );
    assert_eq!(
        locate_in(&input, &TargetQuery::button("Save"))
            .unwrap()
            .path,
        "0/2"
    );
    let mut query = TargetQuery::text("Save");
    query.role = Some("LABEL");
    assert_eq!(locate_in(&input, &query).unwrap().path, "0/1");
}

#[test]
fn text_matches_names_or_content_with_unicode_case_options() {
    let input = snapshot(
        &[node("0/1", "label", "Résumé", "欢迎使用 Zuse", RECT)],
        false,
        0,
    );
    assert!(locate_in(&input, &TargetQuery::text("RÉSUMÉ")).is_err());
    let mut query = TargetQuery::text("RÉSUMÉ");
    query.case_sensitive = false;
    assert_eq!(locate_in(&input, &query).unwrap().name, "Résumé");
    assert!(locate_in(&input, &TargetQuery::text("Zuse")).is_err());
    query = TargetQuery::text("使用 zUSE");
    query.case_sensitive = false;
    query.exact = false;
    assert_eq!(locate_in(&input, &query).unwrap().path, "0/1");
}

#[test]
fn json_escaped_text_is_decoded_before_matching() {
    let input = r#"{"nodes":[{"path":"0/1","role":"button","name":"\u4fdd\u5b58 \"draft\"","text":"","bounds":{"x":1,"y":2,"width":3,"height":4}}]}"#;
    let target = locate_in(input, &TargetQuery::button("保存 \"draft\"")).unwrap();
    assert_eq!(target.name, "保存 \"draft\"");
}

#[test]
fn duplicate_names_require_an_explicit_index_or_scope() {
    let input = snapshot(
        &[
            node("0/1/0", "button", "Save", "Save", RECT),
            node("0/2/0", "button", "Save", "", RECT),
        ],
        false,
        0,
    );
    let mut query = TargetQuery::button("Save");
    assert!(locate_in(&input, &query).unwrap_err().contains("ambiguous"));
    query.index = Some(1);
    assert_eq!(locate_in(&input, &query).unwrap().path, "0/2/0");
    query.index = Some(2);
    assert!(
        locate_in(&input, &query)
            .unwrap_err()
            .contains("out of range")
    );
    query.index = None;
    query.scope = Some("0/1");
    assert_eq!(locate_in(&input, &query).unwrap().path, "0/1/0");
}

#[test]
fn scope_matches_complete_path_segments_and_includes_its_node() {
    let input = snapshot(
        &[
            node("0/10", "button", "Save", "", RECT),
            node("0/1", "button", "Save", "", RECT),
        ],
        false,
        0,
    );
    let mut query = TargetQuery::button("Save");
    query.scope = Some("0/1");
    assert_eq!(locate_in(&input, &query).unwrap().path, "0/1");
    query.scope = Some("0/3");
    assert!(
        locate_in(&input, &query)
            .unwrap_err()
            .contains("no visible target")
    );
}

#[test]
fn explicitly_hidden_nodes_are_not_targeted_or_counted() {
    for flag in ["visible", "showing"] {
        let hidden = format!(
            r#"{{"path":"0/0","role":"button","name":"Save","bounds":{RECT},"{flag}":false}}"#
        );
        let input = snapshot(std::slice::from_ref(&hidden), false, 0);
        assert!(
            locate_in(&input, &TargetQuery::button("Save"))
                .unwrap_err()
                .contains("no visible target")
        );
        let input = snapshot(&[hidden, node("0/1", "button", "Save", "", RECT)], false, 0);
        assert_eq!(
            locate_in(&input, &TargetQuery::button("Save"))
                .unwrap()
                .path,
            "0/1"
        );
    }
}

#[test]
fn password_text_and_redaction_markers_are_excluded() {
    for role in ["password text", "AXSecureTextField", "ControlType.Edit"] {
        let input = snapshot(
            &[node("0/1", role, "Password", "[redacted]", RECT)],
            false,
            0,
        );
        let mut query = TargetQuery::text("redacted");
        query.exact = false;
        assert!(locate_in(&input, &query).is_err());
        assert_eq!(
            locate_in(&input, &TargetQuery::text("Password"))
                .unwrap()
                .path,
            "0/1"
        );
    }
    let input = snapshot(
        &[node("0/1", "password text", "Password", "secret", RECT)],
        false,
        0,
    );
    assert!(locate_in(&input, &TargetQuery::text("secret")).is_err());
}

#[test]
fn malformed_or_empty_rectangles_return_a_bounds_error() {
    for bounds in [
        "null",
        "{}",
        r#"{"x":0,"y":0,"width":0,"height":10}"#,
        r#"{"x":0,"y":0,"width":10,"height":-1}"#,
        r#"{"x":"1","y":0,"width":10,"height":10}"#,
        r#"{"x":1e309,"y":0,"width":10,"height":10}"#,
        r#"{"x":1e308,"y":0,"width":1e308,"height":10}"#,
    ] {
        let input = snapshot(&[node("0/1", "button", "Save", "", bounds)], false, 0);
        assert!(
            locate_in(&input, &TargetQuery::button("Save"))
                .unwrap_err()
                .contains("bounds"),
            "{bounds}"
        );
    }
}

#[test]
fn index_does_not_silently_skip_a_match_with_invalid_bounds() {
    let input = snapshot(
        &[
            node(
                "0/0",
                "button",
                "Save",
                "",
                r#"{"x":0,"y":0,"width":0,"height":10}"#,
            ),
            node("0/1", "button", "Save", "", RECT),
        ],
        false,
        0,
    );
    let mut query = TargetQuery::button("Save");
    query.index = Some(0);
    assert!(locate_in(&input, &query).unwrap_err().contains("bounds"));
    query.index = Some(1);
    assert_eq!(locate_in(&input, &query).unwrap().path, "0/1");
}

#[test]
fn matching_containers_without_bounds_do_not_create_ambiguity() {
    let input = snapshot(
        &[
            node("0/0", "application", "Save", "", "null"),
            r#"{"path":"0/0/0","role":"panel","name":"Save"}"#.into(),
            node("0/0/0/0", "button", "Save", "", RECT),
        ],
        false,
        0,
    );
    assert_eq!(
        locate_in(&input, &TargetQuery::text("Save")).unwrap().path,
        "0/0/0/0"
    );
}

#[test]
fn partial_snapshots_require_explicit_observed_match_selection() {
    for (truncated, errors) in [(true, 0), (false, 1), (true, 3)] {
        let input = snapshot(
            &[node("0/1/0", "button", "Save", "", RECT)],
            truncated,
            errors,
        );
        let mut query = TargetQuery::button("Save");
        assert!(
            locate_in(&input, &query)
                .unwrap_err()
                .contains("incomplete")
        );
        query.scope = Some("0/1");
        assert!(
            locate_in(&input, &query)
                .unwrap_err()
                .contains("incomplete")
        );
        query.index = Some(0);
        assert_eq!(locate_in(&input, &query).unwrap().path, "0/1/0");
    }
}

#[test]
fn missing_matches_and_malformed_snapshots_fail_clearly() {
    assert!(
        locate_in(&snapshot(&[], true, 0), &TargetQuery::text("Missing"))
            .unwrap_err()
            .contains("incomplete")
    );
    for input in [
        "{}",
        r#"{"nodes":null}"#,
        r#"{"nodes":{}}"#,
        r#"{"nodes":[],"truncated":"false"}"#,
        r#"{"nodes":[],"errors":-1}"#,
        "not json",
    ] {
        assert!(locate_in(input, &TargetQuery::text("Save")).is_err());
    }
    let input = snapshot(&[node("1/0", "button", "Save", "", RECT)], false, 0);
    assert!(
        locate_in(&input, &TargetQuery::button("Save"))
            .unwrap_err()
            .contains("path")
    );
}

#[test]
fn query_validation_rejects_blank_text_and_invalid_scopes() {
    for text in ["", " \n\t", "a\0b"] {
        assert!(validate_query(&TargetQuery::text(text)).is_err());
    }
    for scope in ["", "1", "0/", "0//1", "0/-1", "0/a", "0/1000001"] {
        let mut query = TargetQuery::text("Save");
        query.scope = Some(scope);
        assert!(validate_query(&query).is_err(), "{scope}");
    }
    let mut query = TargetQuery::text("Save");
    query.role = Some(" ");
    assert!(validate_query(&query).is_err());
    query.role = None;
    query.scope = Some("0/1000000");
    assert!(validate_query(&query).is_ok());
}
