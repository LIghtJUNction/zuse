#![cfg(feature = "std")]

use zuse::script::syntax::{
    self, ErrorKind, Expr, MAX_ARGS, MAX_DEPTH, MAX_NAME_BYTES, MAX_REPEAT, MAX_SCRIPT_BYTES,
    MAX_STATEMENTS, MAX_STRING_BYTES, MAX_TOKENS, StatementKind,
};

#[test]
fn scripts_preserve_functions_scopes_and_parallel_branch_groups() {
    let source = r##"
        fn sweep(pointer, target) {
            let local = mouse(color="#00e5ff", shape="arrow");
            repeat 2 {
                pointer.move(x=120, y=-30, duration=250);
            }
            sequence {
                target.move_to(text="保存", role="button");
                wait(ms=20);
            }
        }
        let person = user_mouse();
        let agent = mouse(shared=false);
        let alias = agent;
        parallel {
            { sweep(agent, person); }
            {
                let other = mouse();
                other.move(x=400, y=100);
            }
        }
    "##;
    let program = syntax::parse(source).unwrap();
    assert_eq!(program.statements.len(), 5);
    let StatementKind::Function { name, params, body } = &program.statements[0].kind else {
        panic!("expected function");
    };
    assert_eq!(name, "sweep");
    assert_eq!(params, &["pointer", "target"]);
    assert_eq!(body.len(), 3);
    assert!(matches!(body[0].kind, StatementKind::Let { .. }));
    assert!(matches!(
        body[1].kind,
        StatementKind::Repeat { count: 2, .. }
    ));
    assert!(matches!(body[2].kind, StatementKind::Sequence(_)));
    let StatementKind::Let { name, value } = &program.statements[3].kind else {
        panic!("expected alias");
    };
    assert_eq!(name, "alias");
    assert_eq!(value, &Expr::Identifier("agent".into()));
    let StatementKind::Parallel(branches) = &program.statements[4].kind else {
        panic!("expected parallel");
    };
    assert_eq!(branches.len(), 2);
    let StatementKind::Block(first) = &branches[0].kind else {
        panic!("expected sequential branch group");
    };
    let StatementKind::Call(call) = &first[0].kind else {
        panic!("expected function call");
    };
    assert_eq!(call.receiver, None);
    assert_eq!(call.name, "sweep");
    assert_eq!(call.args[0].name, None);
    assert_eq!(call.args[0].value, Expr::Identifier("agent".into()));
    let span = program.statements[4].span;
    assert!(source[span.start..span.end].starts_with("parallel"));
    assert!(source[span.start..span.end].ends_with('}'));
}

#[test]
fn method_receivers_and_named_values_are_unambiguous() {
    let program = syntax::parse(
        r#"pointer.move_to(text="Save", role="button", duration=650, speed_variation=2.5e-1, seed=18446744073709551615, exact=true)"#,
    )
    .unwrap();
    let StatementKind::Call(call) = &program.statements[0].kind else {
        panic!("expected call");
    };
    assert_eq!(call.receiver.as_deref(), Some("pointer"));
    assert_eq!(call.name, "move_to");
    assert_eq!(call.args.len(), 6);
    assert_eq!(call.args[0].name.as_deref(), Some("text"));
    assert_eq!(call.args[0].value, Expr::String("Save".into()));
    assert_eq!(call.args[3].value, Expr::Number("2.5e-1".into()));
    assert_eq!(
        call.args[4].value,
        Expr::Number("18446744073709551615".into())
    );
    assert_eq!(call.args[5].value, Expr::Bool(true));
}

#[test]
fn strings_use_validated_unicode_escapes_and_keep_comment_markers_literal() {
    let program =
        syntax::parse(r#"f(text="中文 \uD83D\uDE00\n\\\" # // /* */", other='it\'s literal')"#)
            .unwrap();
    let StatementKind::Call(call) = &program.statements[0].kind else {
        panic!("expected call");
    };
    assert_eq!(
        call.args[0].value,
        Expr::String("中文 😀\n\\\" # // /* */".into())
    );
    assert_eq!(call.args[1].value, Expr::String("it's literal".into()));
}

#[test]
fn newlines_comments_trailing_commas_and_serial_alias_are_supported() {
    let program = syntax::parse(
        "\u{feff}# start\r\nfn demo(\n p,\n) { p.click() }\n\
         let a =\n mouse\n(\n color\n=\n\"#00e5ff\",\n)\n\
         serial { demo(a) } sequence { a.move(x=1, y=2) }\n\
         a.click() /* outer\n /* nested */ remaining */ a.click() // finish\n",
    )
    .unwrap();
    assert_eq!(program.statements.len(), 6);
    assert!(matches!(
        program.statements[2].kind,
        StatementKind::Sequence(_)
    ));
    assert!(matches!(
        program.statements[3].kind,
        StatementKind::Sequence(_)
    ));
}

#[test]
fn identifiers_after_newlines_do_not_get_swallowed_into_previous_aliases() {
    let program = syntax::parse("let a = mouse()\nlet b = a\nb.click()\n").unwrap();
    assert_eq!(program.statements.len(), 3);
    assert!(matches!(
        &program.statements[1].kind,
        StatementKind::Let { value: Expr::Identifier(value), .. } if value == "a"
    ));
}

#[test]
fn nested_call_syntax_is_preserved_for_semantic_validation() {
    let program = syntax::parse("let p = factory(mouse(color=\"cyan\"))").unwrap();
    let StatementKind::Let {
        value: Expr::Call(outer),
        ..
    } = &program.statements[0].kind
    else {
        panic!("expected constructor call");
    };
    assert_eq!(outer.name, "factory");
    assert!(matches!(&outer.args[0].value, Expr::Call(inner) if inner.name == "mouse"));
}

#[test]
fn malformed_calls_and_missing_separators_fail_the_whole_parse() {
    for (source, expected) in [
        ("a.click()b.click()", ErrorKind::ExpectedSeparator),
        (
            "let a = mouse() let b = mouse()",
            ErrorKind::ExpectedSeparator,
        ),
        ("f(x=1,x=2)", ErrorKind::DuplicateArgument),
        ("f(x=1,p)", ErrorKind::PositionalAfterNamed),
        ("f(p,,q)", ErrorKind::ExpectedValue),
        ("f(x=)", ErrorKind::ExpectedValue),
        ("f(x 1)", ErrorKind::ExpectedCommaOrCloseParen),
        ("let = mouse()", ErrorKind::ExpectedName),
        ("let p mouse()", ErrorKind::ExpectedEquals),
        ("a.move", ErrorKind::ExpectedOpenParen),
        ("a.b.c()", ErrorKind::ExpectedOpenParen),
        ("parallel f()", ErrorKind::ExpectedOpenBrace),
        ("fn f(p,p) {}", ErrorKind::DuplicateParameter),
        ("fn let() {}", ErrorKind::ReservedName),
        ("let true = mouse()", ErrorKind::ReservedName),
        ("f(x=1); }", ErrorKind::UnexpectedCloseBrace),
        ("{ f()", ErrorKind::ExpectedCloseBrace),
        ("/* unfinished", ErrorKind::UnterminatedComment),
    ] {
        let error = syntax::parse(source).unwrap_err();
        assert_eq!(error.kind, expected, "{source}: {error}");
    }
}

#[test]
fn decimal_numbers_follow_strict_finite_spelling() {
    for raw in [
        "00", "01", "-01", "1.", "1e", "1e+", "1e999", "1_000", "1a", "1.2.3", "-",
    ] {
        let source = format!("f(value={raw})");
        assert_eq!(
            syntax::parse(&source).unwrap_err().kind,
            ErrorKind::InvalidNumber,
            "{source}"
        );
    }
    for raw in ["0", "-0", "0.25", "-1.5", "1e+2", "1E-2"] {
        let program = syntax::parse(&format!("f(value={raw})")).unwrap();
        let StatementKind::Call(call) = &program.statements[0].kind else {
            panic!("expected call");
        };
        assert_eq!(call.args[0].value, Expr::Number(raw.into()));
    }
}

#[test]
fn invalid_escapes_surrogates_and_literal_controls_are_rejected() {
    for source in [
        r#"f(text="\q")"#,
        r#"f(text="\uD800")"#,
        r#"f(text="\uDC00")"#,
        r#"f(text="\uD800\u0041")"#,
        r#"f(text="\u00xy")"#,
    ] {
        assert_eq!(
            syntax::parse(source).unwrap_err().kind,
            ErrorKind::InvalidEscape,
            "{source}"
        );
    }
    assert_eq!(
        syntax::parse("f(text=\"line\nbreak\")").unwrap_err().kind,
        ErrorKind::ControlCharacter
    );
    assert_eq!(
        syntax::parse("f(text=\"unfinished").unwrap_err().kind,
        ErrorKind::UnterminatedString
    );
    assert_eq!(
        syntax::parse("f();\0").unwrap_err().kind,
        ErrorKind::ControlCharacter
    );
}

#[test]
fn diagnostics_use_utf8_byte_offsets_but_character_columns_and_crlf_lines() {
    let source = "f(text=\"中文\"); @";
    let offset = source.find('@').unwrap();
    let error = syntax::parse(source).unwrap_err();
    assert_eq!(error.span.start, offset);
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, source[..offset].chars().count() + 1);
    let source = "f(text=\"中文\");\r\nlet p = mouse(\r\n color=\"\\q\"\r\n)";
    let error = syntax::parse(source).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidEscape);
    assert_eq!(error.span.start, source.find("\\q").unwrap());
    assert_eq!((error.span.line, error.span.column), (3, 9));
    assert!(error.to_string().contains("at 3:9"));
}

#[test]
fn source_token_and_statement_budgets_are_independent() {
    assert_eq!(
        syntax::parse(&" ".repeat(MAX_SCRIPT_BYTES + 1))
            .unwrap_err()
            .kind,
        ErrorKind::ScriptTooLarge
    );
    assert_eq!(
        syntax::parse(&";".repeat(MAX_TOKENS + 1)).unwrap_err().kind,
        ErrorKind::TooManyTokens
    );
    assert_eq!(
        syntax::parse(&"f();".repeat(MAX_STATEMENTS + 1))
            .unwrap_err()
            .kind,
        ErrorKind::TooManyStatements
    );
    assert_eq!(
        syntax::parse(&"f();".repeat(MAX_STATEMENTS))
            .unwrap()
            .statements
            .len(),
        MAX_STATEMENTS
    );
}

#[test]
fn names_strings_and_argument_lists_have_explicit_boundaries() {
    assert!(syntax::parse(&format!("let {} = mouse()", "a".repeat(MAX_NAME_BYTES))).is_ok());
    assert_eq!(
        syntax::parse(&format!("let {} = mouse()", "a".repeat(MAX_NAME_BYTES + 1)))
            .unwrap_err()
            .kind,
        ErrorKind::NameTooLong
    );
    assert!(syntax::parse(&format!("f(text=\"{}\")", "x".repeat(MAX_STRING_BYTES))).is_ok());
    assert_eq!(
        syntax::parse(&format!("f(text=\"{}\")", "x".repeat(MAX_STRING_BYTES + 1)))
            .unwrap_err()
            .kind,
        ErrorKind::StringTooLong
    );
    let args = (0..MAX_ARGS).map(|n| format!("p{n}=1")).collect::<Vec<_>>();
    assert!(syntax::parse(&format!("f({})", args.join(","))).is_ok());
    assert_eq!(
        syntax::parse(&format!("f({},extra=1)", args.join(",")))
            .unwrap_err()
            .kind,
        ErrorKind::TooManyArguments
    );
    let params = (0..=MAX_ARGS).map(|n| format!("p{n}")).collect::<Vec<_>>();
    assert_eq!(
        syntax::parse(&format!("fn f({}) {{}}", params.join(",")))
            .unwrap_err()
            .kind,
        ErrorKind::TooManyArguments
    );
}

#[test]
fn recursive_syntax_and_comments_are_bounded() {
    let at_limit = format!("{}{}", "{".repeat(MAX_DEPTH), "}".repeat(MAX_DEPTH));
    assert!(syntax::parse(&at_limit).is_ok());
    let too_deep = format!("{}{}", "{".repeat(MAX_DEPTH + 1), "}".repeat(MAX_DEPTH + 1));
    assert_eq!(
        syntax::parse(&too_deep).unwrap_err().kind,
        ErrorKind::TooDeep
    );
    let too_deep = format!(
        "{}p{}",
        "f(".repeat(MAX_DEPTH + 1),
        ")".repeat(MAX_DEPTH + 1)
    );
    assert_eq!(
        syntax::parse(&too_deep).unwrap_err().kind,
        ErrorKind::TooDeep
    );
    let too_deep = format!(
        "{}{}",
        "/*".repeat(MAX_DEPTH + 1),
        "*/".repeat(MAX_DEPTH + 1)
    );
    assert_eq!(
        syntax::parse(&too_deep).unwrap_err().kind,
        ErrorKind::TooDeep
    );
}

#[test]
fn repeat_count_is_an_integer_bounded_before_expansion() {
    for count in [0, 1, MAX_REPEAT] {
        let program = syntax::parse(&format!("repeat {count} {{ f() }}")).unwrap();
        assert!(matches!(
            program.statements[0].kind,
            StatementKind::Repeat { count: parsed, .. } if parsed == count
        ));
    }
    for count in ["-1", "4097", "1.0", "1e2", "count"] {
        assert_eq!(
            syntax::parse(&format!("repeat {count} {{ f() }}"))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRepeatCount
        );
    }
}

#[test]
fn empty_scripts_and_compound_statement_adjacency_are_well_defined() {
    assert!(
        syntax::parse("# comment only\n; /* empty */")
            .unwrap()
            .statements
            .is_empty()
    );
    let program = syntax::parse("fn a() {}fn b() {}{}parallel {}sequence {}").unwrap();
    assert_eq!(program.statements.len(), 5);
}
