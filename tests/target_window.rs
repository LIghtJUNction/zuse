#![cfg(feature = "std")]
use zuse::target::{TargetQuery, TargetWindow, locate_in_window};

const WINDOWS: &str = r#"[{"id":"0x1","pid":42,"x":100,"y":200,"width":400,"height":300,"scale":1.25,"focused":true,"mapped":true}]"#;
const NODE: &str = r#"{"path":"0/1/0/2","pid":42,"name":"Save","role":"push button","bounds":{"x":120,"y":240,"width":80,"height":30},"visible":true,"showing":true}"#;

#[test]
fn scoped_lookup_ignores_other_applications_without_rescaling_logical_bounds() {
    let other = NODE
        .replace("\"pid\":42", "\"pid\":99")
        .replace("0/1/0/2", "0/2/0/2");
    let snapshot = format!("{{\"nodes\":[{NODE},{other}]}}");
    let window = TargetWindow::from_windows(WINDOWS, "active").unwrap();
    let target = locate_in_window(&snapshot, &TargetQuery::button("Save"), &window).unwrap();
    assert_eq!(target.target.bounds.x, 120.0);
    assert!(target.to_json().contains("\"x\":160"));
    target.verify(WINDOWS, NODE).unwrap();
}

#[test]
fn moved_rescaled_unfocused_or_replaced_targets_stop_before_input() {
    let snapshot = format!("{{\"nodes\":[{NODE}]}}");
    let window = TargetWindow::from_windows(WINDOWS, "0x1").unwrap();
    let target = locate_in_window(&snapshot, &TargetQuery::button("Save"), &window).unwrap();
    for changed in [
        WINDOWS.replace("\"x\":100", "\"x\":500"),
        WINDOWS.replace("1.25", "1.5"),
        WINDOWS.replace("\"focused\":true", "\"focused\":false"),
        WINDOWS.replace("\"pid\":42", "\"pid\":43"),
        WINDOWS.replace("\"mapped\":true", "\"mapped\":false"),
    ] {
        assert!(target.verify(&changed, NODE).is_err());
    }
    for changed in [
        NODE.replace("\"x\":120", "\"x\":130"),
        NODE.replace("Save", "Delete"),
        NODE.replace("\"showing\":true", "\"showing\":false"),
        NODE.replace("\"pid\":42", "\"pid\":43"),
    ] {
        assert!(target.verify(WINDOWS, &changed).is_err());
    }
}

#[test]
fn same_name_buttons_remain_ambiguous_inside_the_selected_window() {
    let other = NODE
        .replace("0/1/0/2", "0/1/0/3")
        .replace("\"x\":120", "\"x\":250");
    let window = TargetWindow::from_windows(WINDOWS, "active").unwrap();
    assert!(
        locate_in_window(
            &format!("{{\"nodes\":[{NODE},{other}]}}"),
            &TargetQuery::button("Save"),
            &window
        )
        .unwrap_err()
        .contains("ambiguous")
    );
}
