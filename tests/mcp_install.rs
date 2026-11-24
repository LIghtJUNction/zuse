#![cfg(feature = "std")]

use std::process::{Command, Output};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zuse"))
        .args(arguments)
        .env_remove("PATH")
        .output()
        .unwrap()
}

#[test]
fn installation_plans_do_not_require_harness_executables() {
    for (client, verb, commands) in [
        ("codex", "add", 1),
        ("claude", "add", 1),
        ("ssmcp", "download", 2),
    ] {
        let output = run(&["mcp", "install", client, "--dry-run"]);
        assert!(output.status.success(), "{output:?}");
        let result = String::from_utf8(output.stdout).unwrap();
        zuse::json::with_value(&result, |root| {
            assert_eq!(zuse::json::text(root.get("status").unwrap())?, "planned");
            let argv: Vec<_> = root.get("commands").unwrap().elements().collect();
            assert_eq!(argv.len(), commands);
            let command = argv[0]
                .elements()
                .map(zuse::json::text)
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(command[0], client);
            assert_eq!(command[2], verb);
            if client != "ssmcp" {
                assert!(command.ends_with(&["mcp".to_owned(), "--stdio".to_owned()]));
            }
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn install_rejects_server_flags_and_read_only_writes() {
    for arguments in [
        &["mcp", "install", "codex", "--stdio", "--dry-run"][..],
        &["mcp", "install", "claude", "--read-only"][..],
        &["mcp", "install", "unknown", "--dry-run"][..],
    ] {
        assert!(!run(arguments).status.success());
    }
}
