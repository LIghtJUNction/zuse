#![cfg(feature = "std")]

use zuse::{
    json,
    mcp::Engine,
    runtime::{self, Options, Runtime},
};

const VALID: &[&str] = &[
    r#"mouse.move_to(text="Save",role="button")"#,
    r#"mouse.move_to(text="保存",duration=850,steps=100,curve="bezier",via="-40,10;80,90")"#,
    r#"mouse.move_to(text="Save",role="button",scope="0/1",exact=false,case_sensitive=false,index=0,speed=800,steps=120,curve="linear",jitter=0.6,speed_variation=0.2,seed=99,offset_x=-3,offset_y=2.5,via="-40,10;80,90")"#,
    r#"mouse.move_to(text="Save",duration=0,steps=1,via="")"#,
    r#"mouse.move_to(text="Save",duration=60000,steps=4096,jitter=1000,speed_variation=1,offset_x=-16777216,offset_y=16777216)"#,
];

const INVALID: &[&str] = &[
    "mouse.move_to()",
    r#"mouse.move_to(text="")"#,
    r#"mouse.move_to(text=" \n\t")"#,
    r#"mouse.move_to(text="Save\u0000")"#,
    r#"mouse.move_to(text="Save",role="")"#,
    r#"mouse.move_to(text="Save",role=" ")"#,
    r#"mouse.move_to(text="Save",duration=500,speed=800)"#,
    r#"mouse.move_to(text="Save",speed=0)"#,
    r#"mouse.move_to(text="Save",speed=-1)"#,
    r#"mouse.move_to(text="Save",speed=1e309)"#,
    r#"mouse.move_to(text="Save",speed=NaN)"#,
    r#"mouse.move_to(text="Save",duration=-1)"#,
    r#"mouse.move_to(text="Save",duration=60001)"#,
    r#"mouse.move_to(text="Save",duration=1.5)"#,
    r#"mouse.move_to(text="Save",steps=0)"#,
    r#"mouse.move_to(text="Save",steps=4097)"#,
    r#"mouse.move_to(text="Save",duration=1,steps=2)"#,
    r#"mouse.move_to(text="Save",duration=0,steps=2)"#,
    r#"mouse.move_to(text="Save",jitter=-0.1)"#,
    r#"mouse.move_to(text="Save",jitter=1000.1)"#,
    r#"mouse.move_to(text="Save",jitter=1e309)"#,
    r#"mouse.move_to(text="Save",speed_variation=-0.1)"#,
    r#"mouse.move_to(text="Save",speed_variation=1.01)"#,
    r#"mouse.move_to(text="Save",speed_variation=Inf)"#,
    r#"mouse.move_to(text="Save",index=-1)"#,
    r#"mouse.move_to(text="Save",index=4096)"#,
    r#"mouse.move_to(text="Save",scope="1/0")"#,
    r#"mouse.move_to(text="Save",scope="0/")"#,
    r#"mouse.move_to(text="Save",offset_x=16777217)"#,
    r#"mouse.move_to(text="Save",offset_y=-16777217)"#,
    r#"mouse.move_to(text="Save",offset_x=1e309)"#,
    r#"mouse.move_to(text="Save",via="1")"#,
    r#"mouse.move_to(text="Save",via="1,2,3")"#,
    r#"mouse.move_to(text="Save",via="NaN,0")"#,
    r#"mouse.move_to(text="Save",via="0,inf")"#,
    r#"mouse.move_to(text="Save",via="16777217,0")"#,
    r#"mouse.move_to(text="Save",via="0,1;")"#,
    r#"mouse.move_to(text="Save",via="0,1;;2,3")"#,
    r#"mouse.move_to(text="Save",curve="unknown")"#,
    r#"mouse.move_to(text="Save",exact="false")"#,
    r#"mouse.move_to(text="Save",case_sensitive=0)"#,
    r#"mouse.move_to(text="Save",seed="random")"#,
    r#"mouse.move_to(text="Save",unknown=true)"#,
];

#[test]
fn checks_all_movement_controls_without_desktop_access() {
    for source in VALID {
        let result = runtime::check(source, Options::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        json::with_value(&result, |root| {
            assert_eq!(json::text(root.get("status").unwrap())?, "valid");
            assert_eq!(root.get("steps").unwrap().integer().unwrap(), 1);
            let plan = root.get("plan").unwrap().elements().next().unwrap();
            assert_eq!(json::text(plan.get("operation").unwrap())?, "mouse.move_to");
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn dry_run_uses_the_same_validation_and_omits_target_text() {
    let source =
        r#"mouse.move_to(text="private-target-example",role="button",speed=640,via="1,2;3,4")"#;
    let expected = runtime::check(source, Options::default()).unwrap();
    let actual = Runtime::new()
        .execute(
            source,
            Options {
                dry_run: true,
                ..Options::default()
            },
        )
        .unwrap();
    assert_eq!(actual, expected);
    assert!(!actual.contains("private-target-example"));
}

#[test]
fn invalid_options_are_rejected_before_any_earlier_operation_executes() {
    for source in INVALID {
        assert!(
            runtime::check(source, Options::default()).is_err(),
            "{source}"
        );
        let program =
            format!("process.start(program=\"zuse-move-to-preflight-must-not-launch\");{source}");
        let expected = runtime::check(&program, Options::default()).unwrap_err();
        // Reaching process.start would produce an execution result instead of
        // the second operation's validation error. No helper process is needed.
        let actual = Runtime::new()
            .execute(&program, Options::default())
            .unwrap_err();
        assert_eq!(actual, expected, "{source}");
    }
}

#[test]
fn waypoint_count_is_bounded_before_execution() {
    let via = vec!["1,2"; 65].join(";");
    let source = format!("mouse.move_to(text=\"Save\",via={})", json::quote(&via));
    assert!(
        runtime::check(&source, Options::default())
            .unwrap_err()
            .contains("64")
    );
    let via = vec!["1,2"; 64].join(";");
    let source = format!("mouse.move_to(text=\"Save\",via={})", json::quote(&via));
    assert!(runtime::check(&source, Options::default()).is_ok());
}

#[test]
fn read_only_blocks_move_to_in_checks_and_dry_runs() {
    let source = r#"mouse.move_to(text="Save",duration=0)"#;
    for dry_run in [false, true] {
        let options = Options {
            read_only: true,
            dry_run,
            ..Options::default()
        };
        assert!(
            runtime::check(source, options)
                .unwrap_err()
                .contains("read-only")
        );
        assert!(
            Runtime::new()
                .execute(source, options)
                .unwrap_err()
                .contains("read-only")
        );
    }
}

#[test]
fn scheduled_durations_and_other_waits_share_one_static_budget() {
    let source =
        r#"mouse.move_to(text="Save");mouse.move_to(text="Next",duration=200);wait(ms=50)"#;
    for (limit, valid) in [(599, false), (600, true)] {
        let result = runtime::check(
            source,
            Options {
                max_wait_ms: limit,
                ..Options::default()
            },
        );
        assert_eq!(result.is_ok(), valid, "budget {limit}: {result:?}");
    }
    assert!(
        runtime::check(
            r#"mouse.move_to(text="Save",duration=40000);mouse.move_to(text="Next",duration=40000)"#,
            Options::default()
        )
        .unwrap_err()
        .contains("wait budget")
    );
    // Speed depends on resolved geometry; static validation still reserves all
    // known waits, and the executor charges the resolved path before input.
    let source = r#"mouse.move_to(text="Save",speed=800);wait(ms=50)"#;
    for (limit, valid) in [(49, false), (50, true)] {
        assert_eq!(
            runtime::check(
                source,
                Options {
                    max_wait_ms: limit,
                    ..Options::default()
                }
            )
            .is_ok(),
            valid
        );
    }
}

#[test]
fn schema_exposes_the_target_and_motion_options() {
    let schema = runtime::schema();
    json::with_value(&schema, |root| {
        let command = root
            .get("commands")
            .unwrap()
            .elements()
            .find(|command| {
                command
                    .get("name")
                    .and_then(|value| value.string())
                    .is_some_and(|name| name.matches("mouse.move_to"))
            })
            .expect("mouse.move_to is discoverable");
        assert_eq!(command.get("read_only").unwrap().boolean(), Some(false));
        let parameters = command.get("parameters").unwrap();
        let expected = [
            ("text", "text", true),
            ("role", "text", false),
            ("scope", "text", false),
            ("exact", "boolean", false),
            ("case_sensitive", "boolean", false),
            ("index", "integer", false),
            ("duration", "integer", false),
            ("speed", "number", false),
            ("steps", "integer", false),
            ("curve", "text", false),
            ("jitter", "number", false),
            ("speed_variation", "number", false),
            ("seed", "integer", false),
            ("offset_x", "number", false),
            ("offset_y", "number", false),
            ("via", "text", false),
        ];
        for (name, kind, required) in expected {
            let parameter = parameters
                .elements()
                .find(|parameter| {
                    parameter
                        .get("name")
                        .and_then(|value| value.string())
                        .is_some_and(|value| value.matches(name))
                })
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(json::text(parameter.get("type").unwrap())?, kind);
            assert_eq!(parameter.get("required").unwrap().boolean(), Some(required));
        }
        Ok(())
    })
    .unwrap();
}

fn mcp_call(engine: &mut Engine, name: &str, arguments: &str) -> (bool, String) {
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":{},"arguments":{},"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}}}}}"#,
        json::quote(name),
        arguments
    );
    let response = String::from_utf8(engine.handle(request.as_bytes()).unwrap().unwrap()).unwrap();
    json::with_value(&response, |root| {
        let result = root.get("result").expect("tool result");
        let is_error = result.get("isError").unwrap().boolean().unwrap();
        let content = result.get("content").unwrap().elements().next().unwrap();
        Ok((is_error, json::text(content.get("text").unwrap())?))
    })
    .unwrap()
}

#[test]
fn mcp_check_and_execute_dry_run_share_move_to_validation() {
    let mut engine = Engine::new(Options::default());
    for (name, extra) in [("zuse.check", ""), ("zuse.execute", ",\"dry_run\":true")] {
        for source in [VALID[2], INVALID[6], INVALID[33]] {
            let arguments = format!("{{\"code\":{}{extra}}}", json::quote(source));
            let (is_error, payload) = mcp_call(&mut engine, name, &arguments);
            match runtime::check(source, Options::default()) {
                Ok(expected) => {
                    assert!(!is_error, "{name}: {payload}");
                    assert_eq!(payload, expected);
                }
                Err(expected) => {
                    assert!(is_error, "{name}: {payload}");
                    assert_eq!(json::field(&payload, "error").unwrap(), expected);
                }
            }
        }
    }
    let (is_error, payload) = mcp_call(&mut engine, "zuse.schema", "{}");
    assert!(!is_error);
    assert_eq!(payload, runtime::schema());
}

#[test]
fn mcp_read_only_cannot_be_weakened_by_dry_run_arguments() {
    let mut engine = Engine::new(Options {
        read_only: true,
        ..Options::default()
    });
    for name in ["zuse.check", "zuse.execute"] {
        let extra = if name == "zuse.execute" {
            ",\"dry_run\":true,\"read_only\":false"
        } else {
            ""
        };
        let arguments = format!("{{\"code\":{}{extra}}}", json::quote(VALID[0]));
        let (is_error, payload) = mcp_call(&mut engine, name, &arguments);
        assert!(is_error, "{payload}");
        assert!(payload.contains("read-only"), "{payload}");
    }
}
