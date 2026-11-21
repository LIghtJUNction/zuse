#![cfg(feature = "std")]

use zuse::{
    drawing::{self, Anchor, parse_points},
    motion::{MAX_COORDINATE, Point},
    runtime::{self, Options, Runtime},
    target::{Bounds, TargetWindow},
};

fn snapshot(x: i32, scale: f64, focused: bool, pid: u32) -> String {
    format!(
        r#"[{{"id":"window-1","pid":{pid},"x":{x},"y":40,"width":800,"height":600,"scale":{scale},"focused":{focused},"mapped":true}}]"#
    )
}

#[test]
fn window_anchor_resolves_local_points_and_rejects_changed_windows() {
    let window = TargetWindow::from_windows(&snapshot(100, 1.25, true, 42), "window-1").unwrap();
    let anchor = Anchor::new(
        window,
        Bounds {
            x: 20.0,
            y: 30.0,
            width: 500.0,
            height: 400.0,
        },
    )
    .unwrap();
    let points = parse_points("0,0;12.5,24.5;499,399").unwrap();
    assert_eq!(
        anchor.resolve(&points).unwrap(),
        [
            Point { x: 120.0, y: 70.0 },
            Point { x: 132.5, y: 94.5 },
            Point { x: 619.0, y: 469.0 },
        ]
    );
    anchor.verify(&snapshot(100, 1.25, true, 42)).unwrap();
    for current in [
        snapshot(101, 1.25, true, 42),
        snapshot(100, 1.0, true, 42),
        snapshot(100, 1.25, false, 42),
        snapshot(100, 1.25, true, 43),
        "[]".into(),
    ] {
        assert!(
            anchor
                .verify(&current)
                .unwrap_err()
                .contains("stale_anchor")
        );
    }
    for point in [
        Point { x: -1.0, y: 0.0 },
        Point { x: 500.0, y: 0.0 },
        Point { x: 0.0, y: 400.0 },
    ] {
        assert!(anchor.resolve(&[point]).is_err());
    }
}

#[test]
fn anchors_require_focused_windows_and_contained_regions() {
    let window = TargetWindow::from_windows(&snapshot(100, 1.25, false, 42), "window-1").unwrap();
    assert!(
        Anchor::new(
            window,
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0
            }
        )
        .is_err()
    );
    let window = TargetWindow::from_windows(&snapshot(100, 1.25, true, 42), "window-1").unwrap();
    for region in [
        Bounds {
            x: 1.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        Bounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 600.0,
        },
        Bounds {
            x: -1.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        },
    ] {
        assert!(Anchor::new(window.clone(), region).is_err());
    }
}

#[test]
fn drawing_preflight_checks_the_entire_path_and_wait_budget() {
    let source = r#"anchor.set(name="board",window="window-1",x=20,y=30,width=500,height=400);
        mouse.click_at(x=5,y=5,anchor="board");
        mouse.stroke(points="1,2;40,50;60,2",duration=100,anchor="board")"#;
    let options = Options {
        dry_run: true,
        max_wait_ms: 540,
        ..Options::default()
    };
    assert!(runtime::check(source, options).is_ok());
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 539,
                ..options
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            source,
            Options {
                read_only: true,
                ..options
            }
        )
        .is_err()
    );
    for invalid in [
        r#"anchor.set(name="board",window="active",width=10,height=10)"#,
        r#"anchor.set(name="",window="window-1",width=10,height=10)"#,
        r#"mouse.stroke(points="1,2;3,4;NaN,8")"#,
        r#"mouse.stroke(points="1,2;3,4",button="bad")"#,
        r#"mouse.stroke(points="1,2;3,4;5,6",duration=1)"#,
        r#"mouse.stroke(points="1,2;3,4",settle=1001)"#,
        r#"mouse.click_at(x=16777217,y=0)"#,
        r#"mouse.click_at(x=1,y=0,anchor="")"#,
        r#"mouse.click_at(x=1,y=0,duration=-1)"#,
        r#"mouse.click_at(x=1,y=0,curve="unknown")"#,
        r#"mouse.click_at(x=1,y=0,jitter=-0.1)"#,
    ] {
        let source = format!("wait(ms=0);{invalid}");
        assert!(
            Runtime::new().execute(&source, Options::default()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn target_movement_defaults_and_explicit_immediacy_share_static_budgets() {
    for (source, budget) in [
        ("mouse.move(x=10,y=20)", 350),
        ("mouse.move_to(text=\"Save\")", 350),
        ("mouse.click_at(x=10,y=20)", 380),
        ("mouse.click_at(x=10,y=20,duration=0)", 30),
        ("mouse.move(x=10,y=20);mouse.click_at(x=30,y=40)", 730),
    ] {
        let options = Options {
            dry_run: true,
            max_wait_ms: budget,
            ..Options::default()
        };
        assert!(runtime::check(source, options).is_ok(), "{source}");
        assert!(
            runtime::check(
                source,
                Options {
                    max_wait_ms: budget - 1,
                    ..options
                }
            )
            .unwrap_err()
            .contains("wait budget"),
            "{source}"
        );
    }
    let immediate = Options {
        dry_run: true,
        max_wait_ms: 0,
        ..Options::default()
    };
    runtime::check("mouse.move(x=1,y=2,duration=0);mouse.click_at(x=3,y=4,duration=0,settle=0,curve=\"linear\",jitter=0,seed=8)", immediate).unwrap();
    let source = "let p=mouse();p.move(x=10,y=20);p.move_to(text=\"Save\")";
    let options = Options {
        dry_run: true,
        max_wait_ms: 700,
        ..Options::default()
    };
    assert_eq!(
        zuse::script::compile(source, options)
            .unwrap()
            .summary
            .static_wait_ms,
        700
    );
    assert!(
        zuse::script::compile(
            source,
            Options {
                max_wait_ms: 699,
                ..options
            }
        )
        .is_err()
    );
    zuse::script::compile("let p=mouse();p.move(x=1,y=2,duration=0)", immediate).unwrap();
}

#[test]
fn unknown_anchor_fails_before_acquiring_a_pointer() {
    let mut runtime = Runtime::new();
    let output = runtime
        .execute(
            r#"mouse.stroke(points="1,2;3,4",anchor="missing")"#,
            Options::default(),
        )
        .unwrap();
    assert!(runtime::failed(&output));
    assert!(output.contains("unknown anchor"));
    let output = runtime
        .execute("mouse.position()", Options::default())
        .unwrap();
    assert!(output.contains("runtime mouse is unavailable"));
}

#[test]
fn drawing_path_size_is_bounded() {
    let source = std::iter::repeat_n("1,2", 4096)
        .collect::<Vec<_>>()
        .join(";");
    assert_eq!(parse_points(&source).unwrap().len(), 4096);
    assert!(parse_points(&(source + ";3,4")).is_err());
}

#[test]
fn shape_geometry_is_explicit_and_closes_exactly() {
    let origin = Point { x: 10.5, y: 20.5 };
    let end = Point { x: 30.5, y: 60.5 };
    assert_eq!(drawing::line(origin, end).unwrap(), [origin, end]);
    assert_eq!(
        drawing::rect(origin, 20.0, 40.0).unwrap(),
        [
            origin,
            Point { x: 30.5, y: 20.5 },
            end,
            Point { x: 10.5, y: 60.5 },
            origin,
        ]
    );
    let circle = drawing::circle(origin, 7.5, 64).unwrap();
    assert_eq!(circle.len(), 65);
    assert_eq!(circle[0], Point { x: 18.0, y: 20.5 });
    assert_eq!(circle.first(), circle.last());
    for point in &circle {
        assert!(((point.x - origin.x).hypot(point.y - origin.y) - 7.5).abs() < 1e-12);
    }
    let polygon = drawing::polygon("10,20;30,40;50,20").unwrap();
    assert_eq!(polygon.len(), 4);
    assert_eq!(polygon.first(), polygon.last());
    assert_eq!(
        drawing::polygon("10,20;30,40;50,20;10,20").unwrap(),
        polygon
    );
}

#[test]
fn shape_geometry_rejects_invalid_sizes_and_counts_closure_in_the_limit() {
    let origin = Point { x: 0.0, y: 0.0 };
    for invalid in [0.0, -1.0, f64::INFINITY, f64::NAN] {
        assert!(drawing::rect(origin, invalid, 10.0).is_err());
        assert!(drawing::rect(origin, 10.0, invalid).is_err());
        assert!(drawing::circle(origin, invalid, 64).is_err());
    }
    assert!(
        drawing::line(
            origin,
            Point {
                x: f64::NAN,
                y: 0.0
            }
        )
        .is_err()
    );
    assert!(
        drawing::rect(
            Point {
                x: MAX_COORDINATE,
                y: 0.0
            },
            1.0,
            1.0
        )
        .is_err()
    );
    assert!(
        drawing::circle(
            Point {
                x: MAX_COORDINATE,
                y: 0.0
            },
            1.0,
            3
        )
        .is_err()
    );
    assert!(drawing::circle(origin, 10.0, 2).is_err());
    assert!(drawing::circle(origin, 10.0, 4096).is_err());
    assert_eq!(drawing::circle(origin, 10.0, 4095).unwrap().len(), 4096);
    assert!(drawing::polygon("0,0;10,10").is_err());
    assert!(drawing::polygon("0,0;10,10;0,0").is_err());
    let vertices = (0..4095)
        .map(|i| format!("{i},1"))
        .collect::<Vec<_>>()
        .join(";");
    assert_eq!(drawing::polygon(&vertices).unwrap().len(), 4096);
    assert_eq!(
        drawing::polygon(&format!("{vertices};0,1")).unwrap().len(),
        4096
    );
    assert!(drawing::polygon(&format!("{vertices};4095,1")).is_err());
}

#[test]
fn shape_schema_and_shared_preflight_cover_defaults_and_budgets() {
    let source = r#"
        draw.line(x1=1.5,y1=2,x2=30,y2=40,anchor="board");
        draw.rect(x=1,y=2,width=30.5,height=40,button="right");
        draw.circle(x=50,y=50,radius=20);
        draw.polygon(points="1,2;30,40;50,2")
    "#;
    let options = Options {
        dry_run: true,
        max_wait_ms: 1440,
        ..Options::default()
    };
    assert!(runtime::check(source, options).is_ok());
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 1439,
                ..options
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            source,
            Options {
                read_only: true,
                ..options
            }
        )
        .is_err()
    );
    let schema = runtime::schema();
    for name in ["draw.line", "draw.rect", "draw.circle", "draw.polygon"] {
        assert!(schema.contains(&format!("\"name\":\"{name}\",\"read_only\":false")));
    }
    for invalid in [
        "draw.line(x1=0,y1=0,x2=1)",
        "draw.rect(x=0,y=0,width=0,height=10)",
        "draw.rect(x=16777216,y=0,width=1,height=10)",
        "draw.circle(x=0,y=0,radius=-1)",
        "draw.circle(x=0,y=0,radius=10,steps=2)",
        "draw.circle(x=0,y=0,radius=10,steps=4096)",
        "draw.circle(x=0,y=0,radius=10,steps=64,duration=63)",
        "draw.rect(x=0,y=0,width=10,height=10,duration=3)",
        "draw.line(x1=0,y1=0,x2=1,y2=1,settle=1001)",
        r#"draw.line(x1=0,y1=0,x2=1,y2=1,button="bad")"#,
        r#"draw.polygon(points="0,0;1,2;NaN,0")"#,
        r#"draw.polygon(points="0,0;1,2;3,0",anchor="")"#,
    ] {
        // Whole-program preflight rejects even a malformed later operation.
        assert!(
            Runtime::new()
                .execute(&format!("wait(ms=0);{invalid}"), options)
                .is_err(),
            "{invalid}"
        );
    }
    let circle = "draw.circle(x=0,y=0,radius=10,steps=4095,duration=0,settle=0);";
    assert!(runtime::check(&circle.repeat(16), options).is_ok());
    assert!(
        runtime::check(&circle.repeat(17), options)
            .unwrap_err()
            .contains("65536")
    );
}

#[test]
fn shape_anchor_is_resolved_before_any_pointer_access() {
    let mut runtime = Runtime::new();
    for source in [
        r#"draw.line(x1=1,y1=2,x2=3,y2=4,anchor="missing")"#,
        r#"draw.triangle(x1=1,y1=2,x2=3,y2=4,x3=5,y3=2,anchor="missing")"#,
        r#"draw.rect(x=1,y=2,width=3,height=4,anchor="missing")"#,
        r#"draw.circle(x=10,y=10,radius=3,anchor="missing")"#,
        r#"draw.polygon(points="1,2;3,4;5,2",anchor="missing")"#,
    ] {
        let output = runtime.execute(source, Options::default()).unwrap();
        assert!(runtime::failed(&output));
        assert!(output.contains("unknown anchor"));
    }
    let output = runtime
        .execute("mouse.position()", Options::default())
        .unwrap();
    assert!(output.contains("runtime mouse is unavailable"));
}

#[test]
fn numeric_triangle_closes_one_stroke_and_shares_all_drawing_preflight() {
    let a = Point { x: 10.5, y: 20.5 };
    let b = Point { x: 30.5, y: 20.5 };
    let c = Point { x: 20.5, y: 5.5 };
    assert_eq!(drawing::triangle(a, b, c).unwrap(), [a, b, c, a]);
    for invalid in [
        Point {
            x: f64::NAN,
            y: 0.0,
        },
        Point {
            x: 0.0,
            y: f64::INFINITY,
        },
        Point {
            x: MAX_COORDINATE + 1.0,
            y: 0.0,
        },
    ] {
        assert!(drawing::triangle(invalid, b, c).is_err());
        assert!(drawing::triangle(a, invalid, c).is_err());
        assert!(drawing::triangle(a, b, invalid).is_err());
    }
    let source = r#"draw.triangle(x1=10.5,y1=20.5,x2=30.5,y2=20.5,x3=20.5,y3=5.5,anchor="board",duration=60,settle=10)"#;
    let options = Options {
        dry_run: true,
        max_wait_ms: 80,
        ..Options::default()
    };
    runtime::check(source, options).unwrap();
    assert!(
        runtime::check(
            source,
            Options {
                max_wait_ms: 79,
                ..options
            }
        )
        .is_err()
    );
    assert!(
        runtime::check(
            source,
            Options {
                read_only: true,
                ..options
            }
        )
        .is_err()
    );
    for invalid in [
        "draw.triangle(x1=0,y1=0,x2=10,y2=0,x3=5)",
        "draw.triangle(x1=0,y1=0,x2=10,y2=0,x3=16777217,y3=5)",
        "draw.triangle(x1=0,y1=0,x2=10,y2=0,x3=5,y3=5,duration=2)",
        "draw.triangle(x1=0,y1=0,x2=10,y2=0,x3=5,y3=5,settle=1001)",
    ] {
        assert!(runtime::check(invalid, options).is_err(), "{invalid}");
    }
    let full = "draw.circle(x=0,y=0,radius=10,steps=4095,duration=0,settle=0);".repeat(16);
    let extra = "draw.triangle(x1=0,y1=0,x2=10,y2=0,x3=5,y3=5,duration=0,settle=0)";
    assert!(
        runtime::check(&(full + extra), options)
            .unwrap_err()
            .contains("65536")
    );
    assert!(runtime::schema().contains("\"name\":\"draw.triangle\",\"read_only\":false"));
}
