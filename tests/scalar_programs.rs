#![cfg(feature = "std")]

use std::{
    io::Write,
    process::{Command, Stdio},
};
use zuse::{
    json,
    language::Parser,
    runtime::{self, Options, Runtime},
    script::{ModuleSession, ProgramMode},
};

const SIERPINSKI: &str = include_str!("../examples/fractal-sierpinski.zuse");

fn options() -> Options {
    Options {
        max_steps: 4096,
        ..Options::default()
    }
}

fn expand(source: &str) -> String {
    let prepared = ModuleSession::default().prepare(source, options()).unwrap();
    assert_eq!(prepared.mode, ProgramMode::Flat);
    runtime::check_prepared(&prepared, options()).unwrap();
    prepared.source
}

fn waits(source: &str) -> Vec<f64> {
    Parser::new(source)
        .map(|call| {
            let call = call.unwrap();
            assert_eq!(call.name, "wait");
            call.get("ms").unwrap().as_f64().unwrap()
        })
        .collect()
}

fn stopped_timer(machine: &mut Runtime) {
    let result = machine.execute("timer.status()", options()).unwrap();
    assert!(result.contains("\"running\":false"), "{result}");
}

#[test]
fn arithmetic_precedence_grouping_and_pure_math_become_literal_arguments() {
    let source = expand(
        r#"
        let product = 2 + 3 * 4
        let grouped = (2 + 3) * 4
        let subtraction = 10-3*2
        let unary = -(2+3) + 9
        wait(ms=product); wait(ms=grouped); wait(ms=subtraction); wait(ms=unary)
        wait(ms=17%5); wait(ms=18/3/2)
        wait(ms=math.sqrt(81)); wait(ms=math.abs(-4))
        wait(ms=math.floor(3.9)); wait(ms=math.ceil(3.1)); wait(ms=math.round(2.5))
        wait(ms=math.min(7,9)); wait(ms=math.max(7,9))
    "#,
    );
    assert_eq!(
        waits(&source),
        [
            14.0, 20.0, 4.0, 4.0, 2.0, 3.0, 9.0, 4.0, 3.0, 4.0, 3.0, 7.0, 9.0
        ]
    );
    let source = expand(
        "draw.line(x1=math.sin(math.pi()/2),y1=math.cos(0),x2=math.cos(math.pi()/2),y2=0,duration=0,settle=0)",
    );
    let call = Parser::new(&source).next().unwrap().unwrap();
    assert!((call.get("x1").unwrap().as_f64().unwrap() - 1.0).abs() < 1e-12);
    assert_eq!(call.get("y1").unwrap().as_f64(), Some(1.0));
    assert!(call.get("x2").unwrap().as_f64().unwrap().abs() < 1e-12);
}

#[test]
fn scalar_bindings_are_lexical_immutable_and_explicit_function_inputs() {
    let source = expand(
        r#"
        let n = 2
        { let n = 7; wait(ms=n) }
        if true { let n = 9; wait(ms=n) }
        for i in 0..3 { let n = i+1; wait(ms=n) }
        fn twice(value) { let doubled = value*2; wait(ms=doubled) }
        twice(value=n)
        wait(ms=n)
    "#,
    );
    assert_eq!(waits(&source), [7.0, 9.0, 1.0, 2.0, 3.0, 4.0, 2.0]);
    for invalid in [
        "let n=1; let n=2; wait(ms=n)",
        "let n=1; n=2; wait(ms=n)",
        "{ let local=1; } wait(ms=local)",
        "if true { let local=1; } wait(ms=local)",
        "for i in 0..1 {} wait(ms=i)",
        "let outside=1; fn capture(){wait(ms=outside)} capture()",
        "fn duplicate(n){let n=2;wait(ms=n)} duplicate(n=1)",
    ] {
        assert!(
            ModuleSession::default()
                .prepare(invalid, options())
                .is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn comparisons_choose_only_the_selected_branch_and_require_booleans() {
    let source = expand(
        r#"
        if 2<3 {wait(ms=1)} else {wait(ms=101)}
        if 2<=2 {wait(ms=2)} else {wait(ms=102)}
        if 3>2 {wait(ms=3)} else {wait(ms=103)}
        if 3>=3 {wait(ms=4)} else {wait(ms=104)}
        if (2+3==5) {wait(ms=5)} else {wait(ms=105)}
        if 2!=3 {wait(ms=6)} else {wait(ms=106)}
        if false {wait(ms=107)} else {wait(ms=7)}
        let ready = 3*3==9
        if ready {wait(ms=8)}
    "#,
    );
    assert_eq!(waits(&source), [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    for invalid in [
        "if 1 {wait(ms=1)}",
        "if \"true\" {wait(ms=1)}",
        "wait(ms=true+1)",
    ] {
        assert!(
            ModuleSession::default()
                .prepare(invalid, options())
                .is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn for_ranges_are_half_open_integer_ranges_with_local_bindings() {
    let source = expand(
        r#"
        let end = 3
        for i in -2..end { wait(ms=i+2) }
        for unused in 3..3 { wait(ms=99) }
        for row in 0..2 { for column in 0..2 { wait(ms=row*2+column) } }
    "#,
    );
    assert_eq!(
        waits(&source),
        [0.0, 1.0, 2.0, 3.0, 4.0, 0.0, 1.0, 2.0, 3.0]
    );
    for invalid in [
        "for i in 0.5..2 {wait(ms=0)}",
        "for i in 0..2.5 {wait(ms=0)}",
    ] {
        assert!(
            ModuleSession::default()
                .prepare(invalid, options())
                .is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn recursive_calls_terminate_by_values_and_survive_declaration_only_import() {
    let library = "fn countdown(n){if n>0{countdown(n=n-1)}else{wait(ms=7)}}";
    let session = ModuleSession::default();
    let imported = session
        .prepare(
            &format!("import(source={},as=\"count\")", json::quote(library)),
            options(),
        )
        .unwrap();
    assert!(imported.source.is_empty());
    assert_eq!(imported.next_session.functions(), ["count.countdown"]);
    let prepared = imported
        .next_session
        .prepare("count.countdown(n=6)", options())
        .unwrap();
    assert_eq!(waits(&prepared.source), [7.0]);
    runtime::check_prepared(&prepared, options()).unwrap();
    assert!(session.functions().is_empty());
}

fn fractal(depth: u32) -> String {
    expand(&format!(
        "{SIERPINSKI}\nsierpinski(canvas=\"board\",x=50,y=590,width=650,height=560,depth={depth})"
    ))
}

fn triangles(source: &str) -> Vec<[f64; 6]> {
    Parser::new(source)
        .map(|call| {
            let call = call.unwrap();
            assert_eq!(call.name, "draw.triangle");
            assert_eq!(call.get("duration").unwrap().as_f64(), Some(60.0));
            assert_eq!(call.get("settle").unwrap().as_f64(), Some(10.0));
            ["x1", "y1", "x2", "y2", "x3", "y3"].map(|key| call.get(key).unwrap().as_f64().unwrap())
        })
        .collect()
}

#[test]
fn recursive_fractal_generates_exact_triangle_geometry_and_call_counts() {
    assert_eq!(
        triangles(&fractal(0)),
        [[50.0, 590.0, 700.0, 590.0, 375.0, 30.0]]
    );
    assert_eq!(
        triangles(&fractal(1)),
        [
            [50.0, 590.0, 375.0, 590.0, 212.5, 310.0],
            [375.0, 590.0, 700.0, 590.0, 537.5, 310.0],
            [212.5, 310.0, 537.5, 310.0, 375.0, 30.0],
        ]
    );
    for depth in [2, 5] {
        let triangles = triangles(&fractal(depth));
        assert_eq!(triangles.len(), 3_usize.pow(depth));
        let width = 650.0 / 2_f64.powi(depth as i32);
        let height = 560.0 / 2_f64.powi(depth as i32);
        let mut positions = std::collections::BTreeSet::new();
        for triangle in triangles {
            let [x1, y1, x2, y2, x3, y3] = triangle;
            assert_eq!(x2 - x1, width);
            assert_eq!(y1, y2);
            assert_eq!(x3, (x1 + x2) / 2.0);
            assert_eq!(y1 - y3, height);
            for [x, y] in [[x1, y1], [x2, y2], [x3, y3]] {
                assert!((50.0..=700.0).contains(&x));
                assert!((30.0..=590.0).contains(&y));
            }
            // These dimensions halve exactly in binary; compare independent leaf locations.
            assert!(positions.insert(((x1 * 32.0) as i64, (y1 * 32.0) as i64)));
        }
    }
    let source = fractal(5);
    let prepared = ModuleSession::default()
        .prepare(&source, options())
        .unwrap();
    runtime::check_prepared(
        &prepared,
        Options {
            max_wait_ms: 19_440,
            ..options()
        },
    )
    .unwrap();
    assert!(
        runtime::check_prepared(
            &prepared,
            Options {
                max_wait_ms: 19_439,
                ..options()
            }
        )
        .is_err()
    );
}

#[test]
fn invalid_numeric_programs_fail_before_earlier_effects_or_new_definitions() {
    for invalid in [
        "wait(ms=1/0)",
        "wait(ms=1%0)",
        "let huge=1e308*1e308;wait(ms=huge)",
        "let bad=math.sqrt(-1);wait(ms=bad)",
        "wait(ms=\"1\"+2)",
        "let result=window.list();wait(ms=0)",
        "let result=keyboard.type(text=\"must not type\");wait(ms=0)",
    ] {
        let mut machine = Runtime::new();
        let source = format!("fn pending(){{timer.status()}};timer.start();{invalid}");
        assert!(
            ModuleSession::default()
                .prepare(&source, options())
                .is_err(),
            "{invalid}"
        );
        assert!(machine.execute(&source, options()).is_err(), "{invalid}");
        assert!(machine.functions().is_empty(), "{invalid}");
        stopped_timer(&mut machine);
    }
}

#[test]
fn loop_recursion_work_and_step_bounds_reject_before_execution() {
    for source in [
        "timer.start();fn forever(){forever()}forever()",
        "timer.start();fn deep(n){if n>0{deep(n=n-1)}}deep(n=100)",
        "timer.start();for i in 0..4097 {}",
        "timer.start();for i in 0..4096 {for j in 0..4096 {}}",
    ] {
        let mut machine = Runtime::new();
        assert!(
            ModuleSession::default().prepare(source, options()).is_err(),
            "{source}"
        );
        assert!(machine.execute(source, options()).is_err(), "{source}");
        stopped_timer(&mut machine);
    }
    let limited = Options {
        max_steps: 3,
        ..options()
    };
    assert!(
        ModuleSession::default()
            .prepare("for i in 0..4 {wait(ms=0)}", limited)
            .is_err()
    );
    let source = format!(
        "{SIERPINSKI}\nsierpinski(canvas=\"board\",x=50,y=590,width=650,height=560,depth=5)"
    );
    assert!(
        ModuleSession::default()
            .prepare(
                &source,
                Options {
                    max_steps: 242,
                    ..options()
                }
            )
            .is_err()
    );
}

#[test]
fn imports_still_reject_top_level_scalar_control_and_actions() {
    for library in [
        "let value=1;fn inspect(){timer.status()}",
        "if true {timer.start()}",
        "for i in 0..1 {timer.start()}",
    ] {
        let source = format!("import(source={},as=\"bad\")", json::quote(library));
        assert!(
            ModuleSession::default()
                .prepare(&source, options())
                .is_err(),
            "{library}"
        );
    }
    let source = format!("import(source={},as=\"fractal\")", json::quote(SIERPINSKI));
    let prepared = ModuleSession::default()
        .prepare(&source, options())
        .unwrap();
    assert!(prepared.source.is_empty());
    assert_eq!(prepared.next_session.functions(), ["fractal.sierpinski"]);
}

#[test]
fn dry_runs_and_failed_replacements_keep_session_definitions_transactional() {
    let mut machine = Runtime::new();
    machine
        .execute("fn retained(){timer.status()}", options())
        .unwrap();
    let source = "fn pending(n){if n>0{timer.start()}};pending(n=1);wait(ms=1/0)";
    assert!(machine.execute(source, options()).is_err());
    assert_eq!(machine.functions(), ["retained"]);
    stopped_timer(&mut machine);
    machine
        .execute(
            "fn pending(n){if n>0{timer.start()}}pending(n=1)",
            Options {
                dry_run: true,
                ..options()
            },
        )
        .unwrap();
    assert_eq!(machine.functions(), ["retained"]);
    stopped_timer(&mut machine);
}

#[test]
fn scalar_arguments_preserve_scoped_mouse_borrows_and_parallel_conflicts() {
    let source = r#"
        let step=10
        fn hop(m, distance){m.move(x=distance+5,y=20,duration=0)}
        let pointer=user_mouse()
        let alias=pointer
        hop(m=alias,distance=step)
        pointer.position()
    "#;
    let prepared = ModuleSession::default().prepare(source, options()).unwrap();
    assert_eq!(prepared.mode, ProgramMode::Scoped);
    let plan = prepared.compile_scoped(options()).unwrap();
    assert_eq!(plan.summary.mice, 1);
    assert_eq!(plan.summary.shared_mice, 1);
    assert_eq!(plan.summary.steps, 3);
    let conflicting = r#"
        fn hop(m, distance){m.move(x=distance+5,y=20,duration=0)}
        let pointer=user_mouse()
        parallel {hop(m=pointer,distance=10);hop(m=pointer,distance=20)}
    "#;
    assert!(runtime::check(conflicting, options()).is_err());
}

#[test]
fn scalar_text_values_cannot_inject_extra_runtime_commands() {
    let source = expand(r#"let payload="value\");timer.start();#";keyboard.type(text=payload)"#);
    let calls = Parser::new(&source).collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "keyboard.type");
    let value = calls[0].get("text").unwrap();
    let mut bytes = vec![0; value.decoded_len().unwrap()];
    assert_eq!(
        value.decode_text(&mut bytes).unwrap(),
        "value\");timer.start();#"
    );
}

#[test]
fn cli_script_stdin_preflights_scalar_drawing_and_capture_without_input() {
    let capture =
        std::env::temp_dir().join(format!("zuse-scalar-dry-run-{}.png", std::process::id()));
    assert!(!capture.exists(), "fixture path must not already exist");
    let source = format!(
        r#"
        {SIERPINSKI}
        mouse.use(kind="user",appearance="system")
        anchor.set(name="board",window="OBSERVED_WINDOW_ID",width=755,height=646)
        for stripe in 0..3 {{
            let y=600+stripe*10
            draw.line(anchor="board",x1=50,y1=y,x2=700,y2=y,duration=60,settle=10)
        }}
        sierpinski(canvas="board",x=50,y=590,width=650,height=560,depth=5)
        window.capture(id="OBSERVED_WINDOW_ID",path={})
    "#,
        json::quote(capture.to_str().unwrap())
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args([
            "script",
            "-",
            "--dry-run",
            "--max-steps",
            "4096",
            "--max-wait-ms",
            "90000",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = String::from_utf8(output.stdout).unwrap();
    let steps = json::with_value(&receipt, |root| {
        Ok(root.get("steps").unwrap().raw().to_owned())
    })
    .unwrap();
    assert_eq!(steps, "249");
    assert!(receipt.contains("\"operation\":\"window.capture\""));
    assert!(!capture.exists(), "dry-run must not capture a real window");
}
