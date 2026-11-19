use zuse::language::{DecodeError, ErrorKind, MAX_ARGS, MAX_SCRIPT_BYTES, Parser, Value};
use zuse::motion::{Curve, MAX_DURATION_MS, MAX_STEPS, Motion, MotionError, Point, Rng};

fn parse_one(source: &str) -> zuse::language::Call<'_> {
    let mut parser = Parser::new(source);
    let call = parser.next().expect("call").expect("valid call");
    assert!(parser.next().is_none());
    call
}

#[test]
fn parse_named_calls_and_comments() {
    let input = "# comment\nmouse.move(x=120, y=-80, duration=350, curve=\"smooth\"); wait(ms=40) // pause\nkeyboard.type(text='你好')";
    let calls = Parser::new(input).collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].name, "mouse.move");
    assert_eq!(calls[0].get("x").and_then(Value::as_i64), Some(120));
    assert_eq!(calls[0].get("y").and_then(Value::as_i64), Some(-80));
    assert_eq!(calls[0].span.line, 2);
    assert_eq!(
        &input[calls[0].span.start..calls[0].span.end],
        "mouse.move(x=120, y=-80, duration=350, curve=\"smooth\")"
    );
    assert_eq!(calls[2].get("text"), Some(Value::String("你好")));
}

#[test]
fn text_decoding_preserves_unicode_and_escaped_controls() {
    let call = parse_one(r#"keyboard.type(text="Hello\n\u4f60\u597d \ud83d\ude00 \\ \" \/ \t")"#);
    let value = call.get("text").unwrap();
    let mut buffer = [0; 128];
    assert_eq!(
        value.decode_text(&mut buffer).unwrap(),
        "Hello\n你好 😀 \\ \" / \t"
    );
}

#[test]
fn text_decode_capacity_is_transactional() {
    let value = parse_one(r#"x(text="\u4f60")"#).get("text").unwrap();
    let mut buffer = [7_u8; 2];
    assert_eq!(
        value.decode_text(&mut buffer),
        Err(DecodeError::BufferTooSmall { needed: 3 })
    );
    assert_eq!(buffer, [7, 7]);
    assert_eq!(
        Value::Bool(true).decode_text(&mut buffer),
        Err(DecodeError::NotText)
    );
    assert_eq!(
        Value::Identifier("ok").decode_text(&mut buffer).unwrap(),
        "ok"
    );
}

#[test]
fn quotes_comments_and_semicolons_inside_values_stay_literal() {
    let call = parse_one("x(text='a; # // \\' \\\\ 中文')");
    let mut buffer = [0; 64];
    assert_eq!(
        call.get("text").unwrap().decode_text(&mut buffer).unwrap(),
        "a; # // ' \\ 中文"
    );
}

#[test]
fn malformed_escapes_and_surrogates_are_rejected_during_parse() {
    for source in [
        r#"x(a="\q")"#,
        r#"x(a="\u123")"#,
        r#"x(a="\ud800")"#,
        r#"x(a="\udc00")"#,
        r#"x(a="\ud800\u0041")"#,
        r#"x(a="\uZZZZ")"#,
    ] {
        assert_eq!(
            Parser::new(source).next().unwrap().unwrap_err().kind,
            ErrorKind::InvalidEscape,
            "{source}"
        );
    }
    assert_eq!(
        Parser::new("x(a=\"line\nbreak\")")
            .next()
            .unwrap()
            .unwrap_err()
            .kind,
        ErrorKind::ControlCharacter
    );
    assert_eq!(
        Parser::new("x(a=\"unfinished")
            .next()
            .unwrap()
            .unwrap_err()
            .kind,
        ErrorKind::UnterminatedString
    );
}

#[test]
fn duplicate_arguments_and_positional_values_are_rejected() {
    let error = Parser::new("x(a=1,a=2)").next().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::DuplicateArgument);
    assert_eq!(error.offset, 6);
    assert_eq!(
        Parser::new("x(1)").next().unwrap().unwrap_err().kind,
        ErrorKind::ExpectedArgumentName
    );
    assert_eq!(
        Parser::new("x(a:1)").next().unwrap().unwrap_err().kind,
        ErrorKind::ExpectedEquals
    );
}

#[test]
fn syntax_rejects_trailing_garbage_and_fuses_after_error() {
    for source in [
        "x() y()",
        "x()junk",
        "x..y()",
        "x(a=1,,)",
        "x(a=)",
        "x(a=1",
        "x(a=one two)",
    ] {
        let mut parser = Parser::new(source);
        assert!(parser.next().unwrap().is_err(), "{source}");
        assert!(parser.next().is_none());
        assert!(parser.next().is_none());
    }
}

#[test]
fn numbers_are_strict_and_integer_spelling_is_preserved() {
    let call = parse_one(
        "x(a=18446744073709551615, b=-9223372036854775808, c=1.25e-3, yes=true, no=false)",
    );
    assert_eq!(call.get("a").and_then(Value::as_u64), Some(u64::MAX));
    assert_eq!(call.get("b").and_then(Value::as_i64), Some(i64::MIN));
    assert_eq!(call.get("c").and_then(Value::as_f64), Some(0.00125));
    assert_eq!(call.get("yes").and_then(Value::as_bool), Some(true));
    assert_eq!(call.get("no").and_then(Value::as_bool), Some(false));
    for value in [
        "01", "-01", "1.", "1e", "1e+", "1e999", "1_000", "-", "0x12", "1.2.3",
    ] {
        let source = format!("x(a={value})");
        assert_eq!(
            Parser::new(&source).next().unwrap().unwrap_err().kind,
            ErrorKind::InvalidNumber,
            "{source}"
        );
    }
}

#[test]
fn multiline_calls_accept_comments_and_a_trailing_comma() {
    let call = parse_one("x(\n # first\n a = 1, // inline\n b = true,\n)\n");
    assert_eq!(call.args().len(), 2);
    assert_eq!(call.get("b"), Some(Value::Bool(true)));
    assert!(Parser::new("# nothing\n; ; // nothing\n").next().is_none());
}

#[test]
fn diagnostics_count_unicode_columns_and_crlf_lines() {
    let mut parser = Parser::new("x(text='你好');\r\n  x(text='é') garbage");
    assert!(parser.next().unwrap().is_ok());
    let error = parser.next().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExpectedSeparator);
    assert_eq!(error.line, 2);
    assert_eq!(error.column, 15);
    assert_eq!(
        &"x(text='你好');\r\n  x(text='é') garbage"[error.offset..],
        "garbage"
    );
}

#[test]
fn argument_and_input_limits_are_enforced() {
    let arguments = (0..=MAX_ARGS)
        .map(|n| format!("a{n}=0"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("x({arguments})");
    assert_eq!(
        Parser::new(&source).next().unwrap().unwrap_err().kind,
        ErrorKind::TooManyArguments
    );
    let large = " ".repeat(MAX_SCRIPT_BYTES + 1);
    assert_eq!(
        Parser::new(&large).next().unwrap().unwrap_err().kind,
        ErrorKind::ScriptTooLarge
    );
    let name = "a".repeat(129);
    assert_eq!(
        Parser::new(&format!("{name}()"))
            .next()
            .unwrap()
            .unwrap_err()
            .kind,
        ErrorKind::NameTooLong
    );
    let value = "a".repeat(65_537);
    assert_eq!(
        Parser::new(&format!("x(text='{value}')"))
            .next()
            .unwrap()
            .unwrap_err()
            .kind,
        ErrorKind::StringTooLong
    );
}

#[test]
fn motion_has_exact_endpoints_and_monotonic_timestamps() {
    let from = Point { x: -24.5, y: 92.0 };
    let to = Point {
        x: 801.0,
        y: -52.25,
    };
    for curve in [
        Curve::Linear,
        Curve::Smooth,
        Curve::EaseInOut,
        Curve::Bezier,
    ] {
        let samples = Motion::new(from, to, 357, 64, curve, 3.0, 42)
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(samples.len(), 65);
        assert_eq!(samples[0].point, from);
        assert_eq!(samples[64].point, to);
        assert_eq!(samples[0].at_ms, 0);
        assert_eq!(samples[64].at_ms, 357);
        assert!(samples.windows(2).all(|pair| pair[0].at_ms < pair[1].at_ms));
        assert!(
            samples
                .iter()
                .all(|sample| sample.point.x.is_finite() && sample.point.y.is_finite())
        );
    }
}

#[test]
fn motion_jitter_is_seeded_and_stays_within_envelope() {
    let a = Point { x: 0.0, y: 0.0 };
    let b = Point { x: 100.0, y: 100.0 };
    let base = Motion::new(a, b, 100, 100, Curve::Linear, 0.0, 1)
        .unwrap()
        .collect::<Vec<_>>();
    let path = |seed| {
        Motion::new(a, b, 100, 100, Curve::Linear, 2.0, seed)
            .unwrap()
            .collect::<Vec<_>>()
    };
    assert_eq!(path(42), path(42));
    assert_ne!(path(42), path(43));
    for (plain, jitter) in base.iter().zip(path(42)) {
        assert!((plain.point.x - jitter.point.x).abs() <= 2.0);
        assert!((plain.point.y - jitter.point.y).abs() <= 2.0);
    }
}

#[test]
fn nonlinear_curves_change_velocity_and_bezier_changes_path() {
    let a = Point::default();
    let b = Point { x: 100.0, y: 0.0 };
    let sample = |curve, index| {
        Motion::new(a, b, 100, 4, curve, 0.0, 0)
            .unwrap()
            .nth(index)
            .unwrap()
            .point
    };
    assert_eq!(sample(Curve::Linear, 1).x, 25.0);
    assert!(sample(Curve::Smooth, 1).x < 25.0);
    assert!(sample(Curve::EaseInOut, 1).x < sample(Curve::Smooth, 1).x);
    assert!(sample(Curve::Bezier, 2).y > 0.0);
    assert_eq!(Curve::parse("bezier"), Some(Curve::Bezier));
    assert_eq!(Curve::parse("unknown"), None);
}

#[test]
fn motion_rejects_nonfinite_and_unbounded_work() {
    let a = Point::default();
    assert_eq!(
        Motion::new(
            a,
            Point {
                x: f64::NAN,
                y: 0.0
            },
            10,
            10,
            Curve::Linear,
            0.0,
            0
        )
        .unwrap_err(),
        MotionError::NonFinite
    );
    assert_eq!(
        Motion::new(a, a, 10, 0, Curve::Linear, 0.0, 0).unwrap_err(),
        MotionError::StepsRange
    );
    assert_eq!(
        Motion::new(a, a, 10, 11, Curve::Linear, 0.0, 0).unwrap_err(),
        MotionError::StepsRange
    );
    assert_eq!(
        Motion::new(a, a, MAX_DURATION_MS + 1, 1, Curve::Linear, 0.0, 0).unwrap_err(),
        MotionError::DurationRange
    );
    assert_eq!(
        Motion::new(a, a, MAX_DURATION_MS, MAX_STEPS + 1, Curve::Linear, 0.0, 0).unwrap_err(),
        MotionError::StepsRange
    );
    assert_eq!(
        Motion::new(a, a, 10, 1, Curve::Linear, -1.0, 0).unwrap_err(),
        MotionError::JitterRange
    );
    assert_eq!(
        Motion::new(a, a, 10, 1, Curve::Linear, f64::INFINITY, 0).unwrap_err(),
        MotionError::NonFinite
    );
    assert_eq!(
        Motion::new(a, Point { x: 1e10, y: 0.0 }, 10, 1, Curve::Linear, 0.0, 0).unwrap_err(),
        MotionError::CoordinateRange
    );
}

#[test]
fn immediate_motion_and_size_hint_are_consistent() {
    let mut motion = Motion::new(
        Point::default(),
        Point { x: 5.0, y: 5.0 },
        0,
        1,
        Curve::Linear,
        0.0,
        0,
    )
    .unwrap();
    assert_eq!(motion.len(), 2);
    assert_eq!(motion.next().unwrap().at_ms, 0);
    assert_eq!(motion.len(), 1);
    assert_eq!(motion.next().unwrap().at_ms, 0);
    assert_eq!(motion.len(), 0);
    assert!(motion.next().is_none());
    assert!(motion.next().is_none());
    assert_eq!(motion.len(), 0);
}

#[test]
fn random_delay_bounds_seed_and_full_width_ranges() {
    let mut a = Rng::new(42);
    let mut b = Rng::new(42);
    for _ in 0..1000 {
        let value = a.random_delay(10, 100).unwrap();
        assert!((10..=100).contains(&value));
        assert_eq!(value, b.random_delay(10, 100).unwrap());
    }
    assert_eq!(a.random_delay(5, 5).unwrap(), 5);
    assert_eq!(a.random_delay(10, 1), Err(MotionError::DelayRange));
    assert_eq!(
        a.random_delay(0, MAX_DURATION_MS + 1),
        Err(MotionError::DelayRange)
    );
    assert_eq!(a.range_u64(u64::MAX, u64::MAX).unwrap(), u64::MAX);
    let mut x = Rng::new(0);
    let mut y = Rng::new(0);
    assert_eq!(x.range_u64(0, u64::MAX).unwrap(), y.next_u64());
}
