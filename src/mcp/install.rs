//! Register the current Zuse executable with supported local clients.
use crate::{VERSION, integrations, json, runtime::Options};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use zmcpio::json::{Kind, Value};

type Result<T> = std::result::Result<T, String>;
const NAME: &str = "zuse";
const SERVER_ARGS: [&str; 2] = ["mcp", "--stdio"];
const CONFIG_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const CONFIG_LIMIT: u64 = 2 * 1024 * 1024;

/// Install Zuse into a local MCP client or plan the commands without changing it.
pub fn install_client(client: &str, options: Options) -> Result<String> {
    let binary = std::env::current_exe().map_err(|e| format!("find Zuse executable: {e}"))?;
    let path = binary
        .to_str()
        .ok_or("MCP clients require a UTF-8 executable path")?;
    let registration: Vec<&str> = match client {
        "codex" => vec!["mcp", "add", NAME, "--", path, "mcp", "--stdio"],
        "claude" => vec![
            "mcp",
            "add",
            "--scope",
            "user",
            "--transport",
            "stdio",
            NAME,
            "--",
            path,
            "mcp",
            "--stdio",
        ],
        "ssmcp" => vec!["computer", "download", VERSION, "--json"],
        _ => return Err("supported MCP clients: codex, claude, ssmcp".into()),
    };
    if options.dry_run {
        let argv = |args: &[&str]| {
            std::iter::once(client)
                .chain(args.iter().copied())
                .map(json::quote)
                .collect::<Vec<_>>()
                .join(",")
        };
        let mut commands = vec![format!("[{}]", argv(&registration))];
        if client == "ssmcp" {
            commands.push(format!(
                "[{}]",
                argv(&["computer", "install", "<download_id>", "--json"])
            ));
        }
        return Ok(format!(
            r#"{{"status":"planned","client":{},"commands":[{}]}}"#,
            json::quote(client),
            commands.join(",")
        ));
    }
    if options.read_only {
        return Err("MCP installation requires write access to the client configuration".into());
    }
    let executable = integrations::executable(client)
        .ok_or_else(|| format!("{client} executable is unavailable on PATH"))?;
    let run = |args: &[&str], timeout| {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        integrations::utility(&executable, &args, timeout)
    };
    if client == "ssmcp" {
        if let Ok(existing) = ssmcp_managed_version(&executable)
            && let Ok(Some(path)) = json::with_value(&existing, |value| {
                let Some(record) = value.get("structuredContent") else {
                    return Ok(None);
                };
                if record.get("managed").and_then(Value::boolean) == Some(true)
                    && record
                        .get("version")
                        .and_then(Value::string)
                        .is_some_and(|version| version.matches(VERSION))
                {
                    return record.get("path").map(json::text).transpose();
                }
                Ok(None)
            })
        {
            return Ok(receipt(client, "already_installed", &path));
        }
        let downloaded = run(&registration, DOWNLOAD_TIMEOUT)?;
        let id = json::with_value(&downloaded, |value| {
            let id = json::text(
                value
                    .get("structuredContent")
                    .and_then(|record| record.get("download_id"))
                    .ok_or("SSMCP did not provide a download receipt")?,
            )?;
            if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Ok(id)
            } else {
                Err("SSMCP returned an invalid download receipt".into())
            }
        })?;
        let installed = run(&["computer", "install", &id, "--json"], CONFIG_TIMEOUT)?;
        let path = json::with_value(&installed, |value| {
            let record = value
                .get("structuredContent")
                .ok_or("SSMCP did not provide an install receipt")?;
            if !record
                .get("version")
                .and_then(Value::string)
                .is_some_and(|version| version.matches(VERSION))
            {
                return Err("SSMCP installed an unexpected Zuse version".into());
            }
            json::text(
                record
                    .get("path")
                    .ok_or("SSMCP did not provide an executable path")?,
            )
        })?;
        return Ok(receipt(client, "installed", &path));
    }

    let registered = || {
        if client == "codex" {
            let listed = run(&["mcp", "list", "--json"], CONFIG_TIMEOUT)
                .map_err(|_| "could not read Codex MCP registrations")?;
            json::with_value(&listed, |root| {
                if root.kind() != Kind::Array {
                    return Err("Codex MCP registrations are not an array".into());
                }
                let entry = root.elements().find(|e| matches_string(*e, "name", NAME));
                entry
                    .map(|entry| {
                        let enabled = entry.get("enabled").and_then(Value::boolean) != Some(false);
                        let transport = entry
                            .get("transport")
                            .ok_or("Codex MCP registration has no transport")?;
                        Ok(enabled && entry_matches(transport, path))
                    })
                    .transpose()
            })
        } else {
            claude_registration(path)
        }
    };
    match registered()? {
        Some(true) => return Ok(receipt(client, "already_installed", path)),
        Some(false) => {
            return Err(format!(
                "{client} already has a different or disabled zuse registration"
            ));
        }
        None => {}
    }
    run(&registration, CONFIG_TIMEOUT)?;
    if registered()? != Some(true) {
        return Err(format!(
            "{client} did not retain the requested Zuse registration"
        ));
    }
    Ok(receipt(client, "installed", path))
}

fn ssmcp_managed_version(executable: &Path) -> Result<String> {
    // SSMCP probes PATH before its managed store. Hide standalone Zuse entries
    // from this child so a second install can find the existing managed release.
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let directories = std::env::split_paths(&inherited_path).filter(|directory| {
        !directory.as_os_str().is_empty()
            && !directory.join(NAME).is_file()
            && !directory.join("zuse.exe").is_file()
    });
    let path = std::env::join_paths(directories).map_err(|e| format!("filter PATH: {e}"))?;
    let executable = std::path::absolute(executable).map_err(|e| format!("locate SSMCP: {e}"))?;
    let mut command = Command::new(&executable);
    command
        .args(["computer", "version", "--json"])
        .env("PATH", path);
    integrations::utility_command(command, CONFIG_TIMEOUT)
}

fn claude_registration(path: &str) -> Result<Option<bool>> {
    let directory = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("cannot locate the Claude Code user configuration")?;
    let file = PathBuf::from(directory).join(".claude.json");
    match fs::metadata(&file) {
        Ok(meta) if meta.is_file() && meta.len() <= CONFIG_LIMIT => {}
        Ok(_) => {
            return Err(
                "Claude Code user configuration is not a regular JSON file within the size limit"
                    .into(),
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read Claude Code user configuration: {e}")),
    }
    let source = fs::read_to_string(&file)
        .map_err(|e| format!("read Claude Code user configuration: {e}"))?;
    json::with_value(&source, |root| {
        if root.kind() != Kind::Object {
            return Err("Claude Code user configuration is not a JSON object".into());
        }
        Ok(root
            .get("mcpServers")
            .and_then(|entries| entries.get(NAME))
            .map(|entry| entry_matches(entry, path)))
    })
}

fn matches_string(value: Value<'_>, key: &str, expected: &str) -> bool {
    value
        .get(key)
        .and_then(Value::string)
        .is_some_and(|v| v.matches(expected))
}

fn entry_matches(entry: Value<'_>, path: &str) -> bool {
    if !matches_string(entry, "type", "stdio") || !matches_string(entry, "command", path) {
        return false;
    }
    let Some(args) = entry.get("args") else {
        return false;
    };
    let mut actual = args.elements();
    SERVER_ARGS.into_iter().all(|expected| {
        actual
            .next()
            .and_then(Value::string)
            .is_some_and(|value| value.matches(expected))
    }) && actual.next().is_none()
}

fn receipt(client: &str, status: &str, path: &str) -> String {
    format!(
        r#"{{"status":{},"client":{},"path":{},"version":{}}}"#,
        json::quote(status),
        json::quote(client),
        json::quote(path),
        json::quote(VERSION),
    )
}
