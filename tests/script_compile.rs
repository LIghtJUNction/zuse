#![cfg(feature = "std")]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use zuse::{runtime::Options, script};

fn options() -> Options {
    Options {
        max_steps: 16_384,
        max_wait_ms: u64::MAX,
        ..Options::default()
    }
}

fn rejected(source: &str, expected: &str) {
    let error = script::compile(source, options()).expect_err(source);
    assert!(
        error.contains(expected),
        "{source}\nexpected {expected:?}, got {error}"
    );
    assert!(error.contains(" at "), "missing source location: {error}");
}

#[test]
fn complete_program_expands_functions_and_counts_actual_work() {
    let source = r##"
        fn travel(p) {
            let helper = mouse(color="#ee7744", shape="ring", size=40, glow=0.9);
            helper.move(1, 2, duration=0);
            p.move_to("Save", role="button", duration=30);
        }
        let a = mouse();
        let b = mouse(shape="crosshair", x=20, y=30);
        parallel {
            { travel(a); a.click(count=2, interval=10); }
            { b.move(5, 6, duration=20); wait(5); }
        }
        sequence { a.scroll(-1, 2); b.position(); }
    "##;
    let plan = script::compile(source, options()).unwrap();
    assert_eq!(plan.summary.steps, 10);
    assert_eq!(plan.summary.mice, 3);
    assert_eq!(plan.summary.shared_mice, 0);
    assert_eq!(plan.summary.parallel_groups, 1);
    assert_eq!(plan.summary.static_wait_ms, 65);
}

#[test]
fn functions_are_hoisted_and_support_positional_or_named_mouse_parameters() {
    let plan = script::compile(
        r#"
        let a = mouse();
        let b = mouse();
        pair(a, right=b);
        fn pair(left, right) {
            parallel { left.position(); right.position(); }
        }
        "#,
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.steps, 4);
    assert_eq!(plan.summary.mice, 2);
    assert_eq!(plan.summary.parallel_groups, 1);
}

#[test]
fn aliases_and_function_parameters_borrow_without_creating_mice() {
    let plan = script::compile(
        r#"
        fn inspect(p) { let alias = p; alias.position(); }
        let original = mouse();
        let alias = original;
        inspect(alias);
        original.position();
        "#,
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.mice, 1);
    assert_eq!(plan.summary.steps, 3);
}

#[test]
fn nested_scopes_allow_shadowing_and_restore_the_outer_binding() {
    let plan = script::compile(
        "let a=mouse(); { let a=mouse(); a.position(); } a.position();",
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.mice, 2);
    assert_eq!(plan.summary.steps, 4);
    for source in [
        "{ let a=mouse(); } a.position();",
        "sequence { let a=mouse(); } a.position();",
        "repeat 2 { let a=mouse(); } a.position();",
        "parallel { { let a=mouse(); } { a.position(); } }",
        "fn local() { let a=mouse(); } local(); a.position();",
    ] {
        rejected(source, "unknown or out-of-scope mouse variable");
    }
}

#[test]
fn functions_do_not_capture_caller_variables_or_globals() {
    rejected(
        "let a=mouse(); fn inspect() { a.position(); } inspect();",
        "unknown or out-of-scope mouse variable",
    );
    rejected(
        "fn inner() { p.position(); } fn outer(p) { inner(); } let a=mouse(); outer(a);",
        "unknown or out-of-scope mouse variable",
    );
}

#[test]
fn duplicate_bindings_and_invalid_function_declarations_are_rejected() {
    for source in [
        "let a=mouse(); let a=mouse();",
        "let a=mouse(); let a=a;",
        "fn f(a) { let a=mouse(); } let a=mouse(); f(a);",
    ] {
        rejected(source, "duplicate variable");
    }
    rejected("fn f() {} fn f() {}", "duplicate function");
    rejected("fn wait() {}", "reserved");
    rejected("fn mouse() {}", "reserved");
    rejected("fn user_mouse() {}", "reserved");
    rejected("{ fn nested() {} }", "only at script top level");
    rejected("fn outer() { fn inner() {} }", "only at script top level");
    rejected("repeat 0 { fn nested() {} }", "only at script top level");
}

#[test]
fn recursion_is_rejected_even_when_the_function_is_not_called() {
    rejected("fn cycle() { cycle(); }", "recursive function");
    rejected("fn a(p) { b(p); } fn b(q) { a(q); }", "recursive function");
    rejected("fn a() { repeat 0 { a(); } }", "recursive function");
}

#[test]
fn function_call_depth_is_bounded_independently_of_source_nesting() {
    let mut source = String::new();
    for index in 0..33 {
        source.push_str(&format!("fn f{index}() {{ f{}(); }}\n", index + 1));
    }
    source.push_str("fn f33() {} f0();");
    rejected(&source, "function call depth exceeds");
}

#[test]
fn function_expansion_and_lexical_blocks_share_a_tree_depth_limit() {
    let program = |count| {
        let mut source = String::new();
        for index in 0..count {
            let call = if index + 1 == count {
                "wait(0);".to_owned()
            } else {
                format!("f{}();", index + 1)
            };
            source.push_str(&format!(
                "fn f{index}() {{{}{call}{}}}\n",
                "{".repeat(8),
                "}".repeat(8),
            ));
        }
        source.push_str("f0();");
        source
    };
    assert_eq!(
        script::compile(&program(12), options())
            .unwrap()
            .summary
            .steps,
        1,
    );
    rejected(&program(16), "expanded tree depth exceeds 128");
}

#[test]
fn function_values_and_arguments_must_be_explicit_mouse_borrows() {
    for (source, error) in [
        ("missing();", "unknown function"),
        ("mouse();", "must be bound with let"),
        ("let a=1;", "let requires"),
        ("let a=mouse(); let b=a.position();", "let requires"),
        ("fn f() {} let a=f();", "let requires"),
        ("fn f(p) {} f();", "requires p"),
        ("fn f(p) {} f(mouse());", "must be mouse variables"),
        ("fn f(p) {} f(3);", "must be mouse variables"),
        ("fn f(p) {} f(missing);", "unknown or out-of-scope"),
        ("fn f(p) {} let a=mouse(); f(q=a);", "has no argument"),
        ("fn f(p) {} let a=mouse(); f(a,p=a);", "duplicate value"),
    ] {
        rejected(source, error);
    }
}

#[test]
fn parallel_conflicts_follow_aliases_and_function_arguments() {
    for source in [
        "let a=mouse(); parallel { a.position(); a.click(); }",
        "let a=mouse(); let b=a; parallel { a.position(); b.click(); }",
        "fn f(p,q) { parallel { p.position(); q.position(); } } let a=mouse(); f(a,a);",
        "fn f(p) { p.position(); } let a=mouse(); parallel { f(a); a.position(); }",
    ] {
        rejected(source, "parallel branches both use");
    }
    script::compile(
        "let a=mouse(); sequence { a.position(); a.position(); }",
        options(),
    )
    .unwrap();
}

#[test]
fn nested_parallel_groups_cannot_hide_conflicts_from_outer_branches() {
    rejected(
        r#"
        let a=mouse(); let b=mouse();
        parallel {
            { a.position(); }
            { parallel { a.position(); b.position(); } }
        }
        "#,
        "parallel branches both use",
    );
    let plan = script::compile(
        r#"
        let a=mouse(); let b=mouse(); let c=mouse();
        parallel {
            { a.position(); }
            { parallel { b.position(); c.position(); } }
        }
        "#,
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.parallel_groups, 2);
}

#[test]
fn distinct_user_handles_share_one_resource_and_cannot_overlap_in_lifetime() {
    rejected(
        "parallel { { let a=user_mouse(); a.position(); } { let b=mouse(shared=true); b.position(); } }",
        "shared user mouse",
    );
    for source in [
        "let a=user_mouse(); let b=user_mouse(); a.position(); b.position();",
        "let a=user_mouse(appearance=\"system\"); let b=mouse(shared=true,appearance=\"system\");",
        "fn f() { let b=user_mouse(); } let a=user_mouse(); f();",
    ] {
        rejected(source, "cannot be alive at the same time");
    }
    let plan = script::compile(
        r#"
        { let a=user_mouse(); let alias=a; alias.position(); }
        { let b=mouse(shared=true,appearance="system"); b.position(); }
        "#,
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.mice, 2);
    assert_eq!(plan.summary.shared_mice, 2);
}

#[test]
fn system_appearance_cannot_change_through_an_alias_or_function_parameter() {
    for source in [
        "let a=user_mouse(appearance=\"system\"); a.appearance();",
        "let a=mouse(shared=true,appearance=\"system\"); let alias=a; alias.appearance();",
        "fn restyle(p) { p.appearance(); } let a=user_mouse(appearance=\"system\"); restyle(a);",
    ] {
        rejected(source, "appearance cannot change");
    }
    script::compile(
        "fn restyle(p) { p.appearance(); } let a=user_mouse(); restyle(a);",
        options(),
    )
    .unwrap();
}

#[test]
fn readonly_rejects_creation_even_with_dry_run() {
    for dry_run in [false, true] {
        let read_only = Options {
            read_only: true,
            dry_run,
            ..options()
        };
        for source in ["let a=mouse();", "let a=user_mouse(appearance=\"system\");"] {
            let error = script::compile(source, read_only).unwrap_err();
            assert!(error.contains("read-only"), "{error}");
        }
        assert!(script::compile("", read_only).is_ok());
        assert!(script::compile("wait(ms=0);", read_only).is_ok());
    }
}

#[test]
fn constructors_validate_visible_appearance_mode_and_start_coordinates() {
    for (arguments, error) in [
        ("shared=1", "boolean literal"),
        ("appearance=\"system\"", "explicit shared user mouse"),
        ("appearance=\"invisible\"", "must be \"glow\" or \"system\""),
        ("name=\"\"", "1..128"),
        ("name=\"\\u0000\"", "NUL-free"),
        ("x=10", "both x and y"),
        ("x=16777217,y=0", "pointer coordinates"),
        ("color=\"blue\"", "#RRGGBB"),
        ("shape=\"star\"", "shape must be"),
        ("size=15", "16..96"),
        ("size=16.0", "unsigned decimal integer"),
        ("glow=1.1", "glow must be"),
        ("hotspot_x=1", "require an image"),
        (
            "shared=true,appearance=\"system\",color=\"#ffffff\"",
            "system appearance",
        ),
    ] {
        rejected(&format!("let a=mouse({arguments});"), error);
    }
    rejected("let a=user_mouse(shared=false);", "cannot be false");
    script::compile(
        r##"let a=mouse(color="#11223380",shape="dot",size=16,glow=0,x=-10.5,y=20);
            a.appearance(color="#abcdef",shape="crosshair",size=96,glow=1);"##,
        options(),
    )
    .unwrap();
}

#[test]
fn movement_arguments_are_validated_before_a_plan_is_returned() {
    for (call, error) in [
        ("move(x=1)", "requires y"),
        ("move(1,2,x=3)", "duplicate value"),
        ("move(1,2,duration=1,speed=3)", "either duration or speed"),
        ("move(1,2,duration=0,steps=2)", "steps must fit duration"),
        ("move(1,2,duration=10,steps=11)", "steps must fit duration"),
        ("move(1,2,speed=0)", "speed must be"),
        ("move(1,2,steps=4097)", "1..4096"),
        ("move(1,2,jitter=-1)", "jitter must be"),
        ("move(1,2,speed_variation=1.1)", "speed_variation must be"),
        ("move(1,2,curve=\"unknown\")", "supported motion curve"),
        ("move(16777216,0,offset_x=1)", "pointer coordinates"),
        ("move(1,2,seed=-1)", "unsigned decimal integer"),
        ("move(1,2,seed=1.0)", "unsigned decimal integer"),
        (
            "move(1,2,seed=18446744073709551616)",
            "unsigned decimal integer",
        ),
        ("move(1,2,via=\"1,2;NaN,4\")", "via point"),
        ("move(1,2,via=\"1,2;3\")", "x,y pairs"),
        ("move(1,2,via=\"1,2,3\")", "invalid y coordinate"),
        ("move(1,2,surprise=3)", "has no argument"),
        ("move(x=mouse(),y=2)", "numeric literal"),
        ("unknown()", "unknown mouse method"),
    ] {
        rejected(&format!("let a=mouse(); a.{call};"), error);
    }
    let via = std::iter::repeat_n("1,2", 65).collect::<Vec<_>>().join(";");
    rejected(
        &format!("let a=mouse(); a.move(1,2,via=\"{via}\");"),
        "at most 64 points",
    );
    let plan = script::compile(
        r#"let a=mouse(); a.move(2.5,-3.5,speed=100,steps=20,curve="bezier",
           jitter=1,speed_variation=0.2,seed=18446744073709551615,
           via="1,2;3,4",offset_x=1,offset_y=-1);"#,
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.static_wait_ms, 0);
}

#[test]
fn target_arguments_are_validated_without_observing_a_desktop() {
    for (arguments, error) in [
        ("", "requires text"),
        ("text=\" \"", "target text"),
        ("text=\"Save\",role=\"\"", "target role"),
        ("text=\"Save\",scope=\"../1\"", "target scope"),
        ("text=\"Save\",scope=\"0/-1\"", "target scope"),
        ("text=\"Save\",index=4096", "0..4095"),
        ("text=\"Save\",exact=1", "boolean literal"),
        ("text=\"Save\",case_sensitive=\"false\"", "boolean literal"),
    ] {
        rejected(&format!("let a=mouse(); a.move_to({arguments});"), error);
    }
    let plan = script::compile(
        r#"let a=mouse(); a.move_to("保存",role="button",scope="0/1",index=0,
            exact=false,case_sensitive=false,duration=0);"#,
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.steps, 2);
}

#[test]
fn buttons_scroll_wait_and_position_have_strict_schemas() {
    for (call, error) in [
        ("click(button=\"wheel\")", "supported mouse button"),
        ("click(count=0)", "1..10"),
        ("click(interval=1001)", "0..1000"),
        ("down(count=2)", "has no argument"),
        ("up(button=1)", "string literal"),
        ("scroll(x=-1001)", "-1000..1000"),
        ("scroll(y=1.5)", "decimal integer"),
        ("position(1)", "too many positional"),
    ] {
        rejected(&format!("let a=mouse(); a.{call};"), error);
    }
    rejected("wait();", "requires ms");
    rejected("wait(ms=60001);", "0..60000");
    rejected("wait(ms=-1);", "unsigned decimal integer");
    rejected("wait(ms=1.0);", "unsigned decimal integer");
}

#[test]
fn held_button_preflight_follows_aliases_borrows_and_numeric_button_names() {
    for source in [
        "let a=mouse(); a.down(); a.click();",
        "let a=mouse(); let b=a; a.down(\"1\"); b.click(\"left\");",
        "let a=mouse(); a.down(\"right\"); a.click(\"3\");",
        "fn hold(p) { p.down(); } let a=mouse(); hold(a); a.click();",
        "fn click(p) { p.click(); } let a=mouse(); a.down(); click(a);",
        "let a=mouse(); let b=mouse(); parallel { a.down(); b.click(); } a.click();",
    ] {
        rejected(source, "already holds the button");
    }
    for source in [
        "let a=mouse(); a.down(\"1\"); a.up(\"left\"); a.click(\"1\");",
        "let a=mouse(); a.down(\"left\"); a.up(\"1\"); a.click();",
        "fn release(p) { p.up(); } let a=mouse(); a.down(); release(a); a.click();",
        "let a=mouse(); a.down(); {let a=mouse(); a.click();} a.up();",
        "let a=mouse(); let b=mouse(); parallel { a.down(); b.click(); } a.up(); a.click();",
        "let a=mouse(); a.down(\"left\"); a.click(\"right\");",
    ] {
        script::compile(source, options()).unwrap();
    }
}

#[test]
fn scheduled_wait_budget_sums_parallel_work_and_click_intervals() {
    let source = r#"
        let a=mouse(); let b=mouse();
        parallel {
            { a.move(1,2,duration=30); a.click(count=3,interval=10); }
            { b.move(3,4,duration=40); wait(5); }
        }
    "#;
    let limit = Options {
        max_wait_ms: 95,
        ..options()
    };
    assert_eq!(
        script::compile(source, limit)
            .unwrap()
            .summary
            .static_wait_ms,
        95
    );
    let error = script::compile(
        source,
        Options {
            max_wait_ms: 94,
            ..limit
        },
    )
    .unwrap_err();
    assert!(error.contains("wait budget exceeds 94ms"), "{error}");
    assert_eq!(
        script::compile("let a=mouse(); a.move(1,2);", options())
            .unwrap()
            .summary
            .static_wait_ms,
        350
    );
}

#[test]
fn step_limit_counts_expanded_effects_and_excludes_aliases() {
    let source = "fn f(p){p.position();} let a=mouse(); let alias=a; repeat 3 {f(alias);}";
    let limit = Options {
        max_steps: 4,
        ..options()
    };
    assert_eq!(script::compile(source, limit).unwrap().summary.steps, 4);
    let error = script::compile(
        source,
        Options {
            max_steps: 3,
            ..limit
        },
    )
    .unwrap_err();
    assert!(error.contains("step limit exceeded (3)"), "{error}");
    assert!(
        script::compile(
            "",
            Options {
                max_steps: 0,
                ..options()
            }
        )
        .is_ok()
    );
}

#[test]
fn empty_expansions_still_have_a_hard_node_budget() {
    rejected("repeat 4096 { repeat 4096 {} }", "expanded node limit");
    assert_eq!(
        script::compile("repeat 0 { let a=mouse(); }", options())
            .unwrap()
            .summary
            .mice,
        0
    );
}

#[test]
fn total_creation_and_peak_live_mice_have_separate_budgets() {
    let plan = script::compile("repeat 256 { let a=mouse(); }", options()).unwrap();
    assert_eq!(plan.summary.mice, 256);
    rejected("repeat 257 { let a=mouse(); }", "created mouse limit");
    let make = |count| {
        (0..count)
            .map(|i| format!("let a{i}=mouse();"))
            .collect::<String>()
    };
    script::compile(&make(32), options()).unwrap();
    rejected(&make(33), "alive at once");
    script::compile(&format!("{{{}}} {{{}}}", make(32), make(32)), options()).unwrap();
    rejected(
        &format!("let outer=mouse(); fn many() {{{}}} many();", make(32)),
        "alive at once",
    );
}

#[test]
fn parallel_peak_includes_all_branch_local_mice() {
    let source = (0..33).map(|_| "{let a=mouse();}").collect::<String>();
    rejected(&format!("parallel {{{source}}}"), "alive at once");
    let source = (0..32).map(|_| "{let a=mouse();}").collect::<String>();
    script::compile(&format!("parallel {{{source}}}"), options()).unwrap();
}

struct ImageFixture(PathBuf);

impl ImageFixture {
    fn new(size: u32) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "zuse-script-image-{}-{}.pam",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut bytes = format!(
            "P7\nWIDTH {size}\nHEIGHT {size}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n"
        )
        .into_bytes();
        bytes.resize(bytes.len() + (size * size * 4) as usize, 255);
        fs::write(&path, bytes).unwrap();
        Self(path)
    }

    fn quoted(&self) -> String {
        format!("{:?}", self.0.to_str().unwrap())
    }
}

impl Drop for ImageFixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn custom_images_are_loaded_and_validated_during_compilation() {
    let image = ImageFixture::new(2);
    let path = image.quoted();
    let plan = script::compile(
        &format!("let a=mouse(image={path},hotspot_x=1,hotspot_y=1); a.appearance(image={path});"),
        options(),
    )
    .unwrap();
    assert_eq!(plan.summary.steps, 2);
    rejected(
        &format!("let a=mouse(image={path},color=\"#ffffff\");"),
        "cannot be combined",
    );
    rejected(
        &format!("let a=user_mouse(image={path},appearance=\"system\");"),
        "cannot be combined",
    );
    rejected(
        &format!("let a=mouse(image={path},hotspot_x=2);"),
        "hotspot",
    );
    fs::remove_file(&image.0).unwrap();
    rejected(&format!("let a=mouse(image={path});"), "open cursor PAM");
}

#[test]
fn repeated_images_have_a_separate_decoded_memory_budget() {
    let image = ImageFixture::new(256);
    let path = image.quoted();
    let source = |count| format!("let a=mouse(); repeat {count} {{ a.appearance(image={path}); }}");
    assert_eq!(
        script::compile(&source(64), options())
            .unwrap()
            .summary
            .steps,
        65
    );
    rejected(&source(65), "16 MiB pixel budget");
}

#[test]
fn expanded_target_text_has_a_separate_retained_memory_budget() {
    let text = "x".repeat(65_532);
    let source = |count| {
        format!("let a=mouse(); repeat {count} {{ a.move_to(text=\"{text}\",duration=0); }}")
    };
    assert_eq!(
        script::compile(&source(256), options())
            .unwrap()
            .summary
            .steps,
        257,
    );
    rejected(&source(257), "16 MiB retained string budget");
    let text = "x".repeat(32_768);
    rejected(
        &format!(
            "let a=mouse(); repeat 256 {{ a.move_to(text=\"{text}\",role=\"{text}\",duration=0); }}"
        ),
        "16 MiB retained string budget",
    );
}

#[test]
fn errors_identify_the_offending_source_line() {
    let error = script::compile("let a=mouse();\n\na.move(x=1,y=\"bad\");", options()).unwrap_err();
    assert!(error.contains(" at 3:12 "), "{error}");
}

#[test]
fn touchpad_planning_is_pure_and_native_capability_is_a_separate_preflight() {
    let source = r#"
        touchpad.capabilities();
        touchpad.swipe(dx=120,dy=-20,fingers=3,duration=120);
        sequence {
            touchpad.pinch(scale=0.8,duration=60);
            touchpad.rotate(degrees=90,duration=40);
        }
        touchpad.cancel();
    "#;
    let plan = script::compile(source, options()).unwrap();
    assert_eq!(plan.summary.steps, 5);
    assert_eq!(plan.summary.mice, 0);
    assert_eq!(plan.summary.static_wait_ms, 220);
    assert!(
        plan.require_native_capabilities()
            .unwrap_err()
            .contains("touchpad.swipe")
    );
    let plan = script::compile(
        "let p=mouse(); p.move(1,2,duration=0); touchpad.pinch(scale=1.1);",
        options(),
    )
    .unwrap();
    assert!(
        plan.require_native_capabilities()
            .unwrap_err()
            .contains("touchpad.pinch")
    );
    let observe = script::compile(
        "parallel { touchpad.capabilities(); touchpad.capabilities(); }",
        Options {
            read_only: true,
            ..options()
        },
    )
    .unwrap();
    observe.require_native_capabilities().unwrap();
}

#[test]
fn touchpad_arguments_and_global_receiver_are_checked() {
    for (source, error) in [
        ("touchpad.pinch();", "requires scale"),
        ("touchpad.rotate();", "requires degrees"),
        ("touchpad.swipe(fingers=1);", "2..5"),
        ("touchpad.swipe(fingers=3.0);", "unsigned decimal integer"),
        ("touchpad.swipe(dx=1000001);", "swipe dx/dy"),
        ("touchpad.pinch(scale=0);", "pinch scale"),
        ("touchpad.rotate(degrees=3601);", "rotation degrees"),
        (
            "touchpad.swipe(duration=0,steps=2);",
            "steps must fit duration",
        ),
        ("touchpad.pinch(scale=1.1,speed=2);", "has no argument"),
        ("touchpad.cancel(1);", "too many positional"),
        ("touchpad.capabilities(fingers=2);", "has no argument"),
        ("touchpad.scroll(dy=1);", "unknown touchpad operation"),
        ("let touchpad=mouse();", "reserved global receiver"),
        ("fn f(touchpad) {}", "reserved global receiver"),
        ("fn touchpad() {}", "reserved"),
    ] {
        rejected(source, error);
    }
    for source in [
        "touchpad.swipe(dx=1);",
        "touchpad.pinch(scale=1.1);",
        "touchpad.rotate(degrees=1);",
        "touchpad.cancel();",
    ] {
        for dry_run in [false, true] {
            let error = script::compile(
                source,
                Options {
                    read_only: true,
                    dry_run,
                    ..options()
                },
            )
            .unwrap_err();
            assert!(error.contains("read-only"), "{error}");
        }
    }
}

#[test]
fn concurrent_touchpad_gestures_share_one_stream_including_function_calls() {
    rejected(
        "parallel { touchpad.swipe(dx=1); touchpad.pinch(scale=1.1); }",
        "touchpad gesture stream",
    );
    rejected(
        "fn swipe() {touchpad.swipe(dx=1);} parallel {swipe(); {touchpad.cancel();}}",
        "touchpad gesture stream",
    );
    script::compile(
        "sequence {touchpad.swipe(dx=1);touchpad.pinch(scale=1.1);}",
        options(),
    )
    .unwrap();
}

#[test]
fn expanded_touchpad_samples_and_waits_have_separate_budgets() {
    let source =
        |count| format!("repeat {count} {{touchpad.swipe(dx=10,duration=60000,steps=4096);}}");
    assert_eq!(
        script::compile(&source(15), options())
            .unwrap()
            .summary
            .steps,
        15
    );
    rejected(&source(16), "touchpad samples exceed 65536");
    let error = script::compile(
        "touchpad.pinch(scale=2,duration=10);",
        Options {
            max_wait_ms: 9,
            ..options()
        },
    )
    .unwrap_err();
    assert!(error.contains("wait budget exceeds 9ms"), "{error}");
}
