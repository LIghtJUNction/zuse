#![cfg(feature = "std")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use zuse::{json, packages};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zuse-packages-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn directory(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir(&path).unwrap();
        path
    }

    fn app(&self) -> PathBuf {
        let path = self.directory("app");
        packages::run_at(&path, &["init", "app"]).unwrap();
        path
    }

    fn repository(&self, directory: &str, name: &str, deps: &[(&str, &Path, &str)]) -> PathBuf {
        let path = self.directory(directory);
        self.populate(&path, name, deps);
        path
    }

    fn populate(&self, path: &Path, name: &str, deps: &[(&str, &Path, &str)]) {
        let dependencies = deps
            .iter()
            .map(|(name, git, rev)| {
                format!(
                    "{}:{{\"git\":{},\"rev\":{}}}",
                    json::quote(name),
                    json::quote(git.to_str().unwrap()),
                    json::quote(rev)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        fs::write(path.join("zuse.json"), format!("{{\"schema\":1,\"name\":{},\"version\":\"0.1.0\",\"entry\":\"lib.zuse\",\"exports\":{{\"drawing\":\"drawing.zuse\"}},\"dependencies\":{{{dependencies}}}}}\n", json::quote(name))).unwrap();
        fs::write(path.join("lib.zuse"), "fn inspect(){timer.status()}\n").unwrap();
        fs::write(path.join("drawing.zuse"), "fn inspect(){timer.status()}\n").unwrap();
        fs::write(path.join(".gitignore"), ".zuse/\n").unwrap();
        git(path, &["init", "--quiet", "--initial-branch=main"]);
        commit(path);
        git(path, &["tag", "v0.1.0"]);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "user.name=Package Test",
            "-c",
            "user.email=package@example.test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit(path: &Path) {
    git(path, &["add", "--all"]);
    git(path, &["commit", "--quiet", "-m", "Package fixture"]);
}

fn add(root: &Path, repository: &Path, rev: &str) -> Result<String, String> {
    packages::run_at(root, &["add", repository.to_str().unwrap(), "--rev", rev])
}

fn locked_commit(root: &Path, name: &str) -> String {
    let lock = fs::read_to_string(root.join("zuse.lock.json")).unwrap();
    json::with_value(&lock, |root| {
        json::text(
            root.get("packages")
                .unwrap()
                .get(name)
                .unwrap()
                .get("commit")
                .unwrap(),
        )
    })
    .unwrap()
}

#[test]
fn offline_lifecycle_installs_transitives_exports_lists_and_removes() {
    let f = Fixture::new();
    let leaf = f.repository("leaf", "leaf", &[]);
    let kit = f.repository("kit", "kit", &[("leaf", &leaf, "v0.1.0")]);
    let app = f.app();
    add(&app, &kit, "v0.1.0").unwrap();
    assert_eq!(
        locked_commit(&app, "kit"),
        git(&kit, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        locked_commit(&app, "leaf"),
        git(&leaf, &["rev-parse", "HEAD"])
    );
    assert!(
        packages::resolve_import(&app, "pkg:kit")
            .unwrap()
            .ends_with("lib.zuse")
    );
    assert!(
        packages::resolve_import(&app, "pkg:kit/drawing")
            .unwrap()
            .ends_with("drawing.zuse")
    );
    assert!(
        packages::resolve_import(&app, "pkg:kit/missing")
            .unwrap_err()
            .contains("does not export")
    );
    let listed = packages::run_at(&app, &["list"]).unwrap();
    assert!(listed.contains("\"installed\":true"));
    packages::run_at(&app, &["remove", "kit"]).unwrap();
    let lock = fs::read_to_string(app.join("zuse.lock.json")).unwrap();
    assert!(lock.contains("\"packages\":{}"));
    assert!(
        packages::resolve_import(&app, "pkg:kit")
            .unwrap_err()
            .contains("run zuse pkg install")
    );
}

#[test]
fn locked_install_restores_exact_commit_and_update_refreshes_the_same_selector() {
    let f = Fixture::new();
    let source = f.repository("colors", "colors", &[]);
    let app = f.app();
    add(&app, &source, "main").unwrap();
    let first = locked_commit(&app, "colors");
    let first_lock = fs::read(app.join("zuse.lock.json")).unwrap();
    fs::write(source.join("lib.zuse"), "fn inspect(){wait(ms=0)}\n").unwrap();
    let text = fs::read_to_string(source.join("zuse.json"))
        .unwrap()
        .replace("0.1.0", "0.2.0");
    fs::write(source.join("zuse.json"), text).unwrap();
    commit(&source);
    let second = git(&source, &["rev-parse", "HEAD"]);
    assert_ne!(first, second);
    fs::remove_dir_all(app.join(".zuse/packages/colors").join(&first)).unwrap();
    packages::run_at(&app, &["install", "--locked"]).unwrap();
    assert_eq!(fs::read(app.join("zuse.lock.json")).unwrap(), first_lock);
    let installed = packages::resolve_import(&app, "pkg:colors").unwrap();
    assert!(
        fs::read_to_string(installed)
            .unwrap()
            .contains("timer.status")
    );
    packages::run_at(&app, &["install"]).unwrap();
    assert_eq!(locked_commit(&app, "colors"), first);
    packages::run_at(&app, &["update", "colors"]).unwrap();
    assert_eq!(locked_commit(&app, "colors"), second);
    let installed = packages::resolve_import(&app, "pkg:colors").unwrap();
    assert!(
        fs::read_to_string(installed)
            .unwrap()
            .contains("wait(ms=0)")
    );
}

#[test]
fn imports_reject_missing_lock_dirty_cache_and_index_hidden_changes() {
    let f = Fixture::new();
    let source = f.repository("kit", "kit", &[]);
    let app = f.app();
    assert!(
        packages::resolve_import(&app, "pkg:kit")
            .unwrap_err()
            .contains("run zuse pkg install")
    );
    add(&app, &source, "v0.1.0").unwrap();
    let path = packages::resolve_import(&app, "pkg:kit").unwrap();
    fs::write(&path, "fn inspect(){wait(ms=0)}\n").unwrap();
    assert!(
        packages::resolve_import(&app, "pkg:kit")
            .unwrap_err()
            .contains("modified")
    );
    git(
        path.parent().unwrap(),
        &["update-index", "--assume-unchanged", "lib.zuse"],
    );
    assert!(
        packages::resolve_import(&app, "pkg:kit")
            .unwrap_err()
            .contains("committed content")
    );
}

#[test]
fn selector_mismatch_and_failed_add_do_not_replace_the_previous_snapshot() {
    let f = Fixture::new();
    let first = f.repository("leaf-one", "leaf", &[]);
    let second = f.repository("leaf-two", "leaf", &[]);
    let kit = f.repository("kit", "kit", &[("leaf", &second, "main")]);
    let app = f.app();
    add(&app, &first, "main").unwrap();
    let manifest = fs::read(app.join("zuse.json")).unwrap();
    let lock = fs::read(app.join("zuse.lock.json")).unwrap();
    assert!(add(&app, &kit, "main").unwrap_err().contains("conflicting"));
    assert_eq!(fs::read(app.join("zuse.json")).unwrap(), manifest);
    assert_eq!(fs::read(app.join("zuse.lock.json")).unwrap(), lock);
    let altered = String::from_utf8(manifest)
        .unwrap()
        .replace("\"rev\":\"main\"", "\"rev\":\"v0.1.0\"");
    fs::write(app.join("zuse.json"), altered).unwrap();
    assert!(
        packages::run_at(&app, &["install", "--locked"])
            .unwrap_err()
            .contains("matching")
    );
    assert!(
        packages::resolve_import(&app, "pkg:leaf")
            .unwrap_err()
            .contains("differ")
    );
}

#[test]
fn cycles_invalid_manifests_and_executable_module_roots_fail_before_locking() {
    let f = Fixture::new();
    let a = f.directory("a");
    let b = f.directory("b");
    f.populate(&a, "a", &[("b", &b, "main")]);
    f.populate(&b, "b", &[("a", &a, "main")]);
    let app = f.app();
    let previous = fs::read(app.join("zuse.json")).unwrap();
    assert!(add(&app, &a, "main").unwrap_err().contains("cycle"));
    let invalid = f.repository("invalid", "invalid", &[]);
    let text = fs::read_to_string(invalid.join("zuse.json"))
        .unwrap()
        .replace("lib.zuse", "../outside.zuse");
    fs::write(invalid.join("zuse.json"), text).unwrap();
    commit(&invalid);
    assert!(
        add(&app, &invalid, "main")
            .unwrap_err()
            .contains("relative")
    );
    let actions = f.repository("actions", "actions", &[]);
    fs::write(actions.join("lib.zuse"), "eval(source=\"timer.start()\")\n").unwrap();
    commit(&actions);
    assert!(
        add(&app, &actions, "main")
            .unwrap_err()
            .contains("declarations")
    );
    assert_eq!(fs::read(app.join("zuse.json")).unwrap(), previous);
    assert!(!app.join("zuse.lock.json").exists());
}

#[cfg(unix)]
#[test]
fn package_symlinks_are_rejected_before_source_is_read() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let source = f.repository("linked", "linked", &[]);
    fs::remove_file(source.join("lib.zuse")).unwrap();
    symlink("../outside.zuse", source.join("lib.zuse")).unwrap();
    commit(&source);
    let app = f.app();
    assert!(add(&app, &source, "main").unwrap_err().contains("symlink"));
    assert!(!app.join("zuse.lock.json").exists());
}

#[test]
fn publication_dry_run_is_read_only_and_publish_pushes_only_the_version_tag() {
    let f = Fixture::new();
    let source = f.repository("published", "published", &[]);
    let origin = f.directory("origin.git");
    git(&origin, &["init", "--bare", "--quiet"]);
    git(
        &source,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let plan = packages::run_at(&source, &["publish", "--dry-run"]).unwrap();
    assert_eq!(json::field(&plan, "status").unwrap(), "planned");
    assert!(git(&source, &["ls-remote", "--tags", "origin"]).is_empty());
    let published = packages::run_at(&source, &["publish"]).unwrap();
    assert_eq!(json::field(&published, "status").unwrap(), "published");
    assert!(git(&source, &["ls-remote", "--tags", "origin"]).contains("refs/tags/v0.1.0"));
    assert!(git(&source, &["ls-remote", "--heads", "origin"]).is_empty());
    fs::write(source.join("lib.zuse"), "fn inspect(){wait(ms=0)}\n").unwrap();
    commit(&source);
    assert!(
        packages::run_at(&source, &["publish"])
            .unwrap_err()
            .contains("another commit")
    );
}

#[cfg(unix)]
#[test]
fn explicit_fetch_uses_configured_credentials_but_import_never_invokes_the_helper() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    let app = f.app();
    let source = f.repository("authenticated", "authenticated", &[]);
    let marker = f.0.join("credential-calls");
    let helper = f.0.join("credential-helper.sh");
    let shell_quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(&helper, format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> {}\nif [ \"$1\" = get ]; then\n  printf 'username=fixture\\npassword=fixture\\n\\n'\nfi\n", shell_quote(&marker))).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("fixture HTTP accept: {error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut request = Vec::new();
            while request.len() < 8192 {
                let mut bytes = [0; 1024];
                match stream.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => request.extend_from_slice(&bytes[..count]),
                }
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            let authenticated = String::from_utf8_lossy(&request)
                .to_ascii_lowercase()
                .contains("\r\nauthorization: basic ");
            let response = if authenticated {
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"fixture\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = stream.write_all(response.as_bytes());
            if authenticated {
                return true;
            }
        }
        false
    });
    let config = f.0.join("gitconfig");
    let config_name = config.to_str().unwrap();
    git(
        &app,
        &[
            "config",
            "--file",
            config_name,
            "credential.helper",
            &format!("!sh {}", shell_quote(&helper)),
        ],
    );
    git(
        &app,
        &[
            "config",
            "--file",
            config_name,
            &format!("url.http://{address}/.insteadOf"),
            &format!("https://{address}/"),
        ],
    );
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_zuse"))
            .current_dir(&app)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("NO_PROXY", "127.0.0.1")
            .output()
            .unwrap()
    };
    // The local server deliberately denies the authenticated fetch. Its role is
    // to prove Git obtained fake credentials without involving any real account.
    let attempted = invoke(&[
        "pkg",
        "add",
        &format!("https://{address}/repo.git"),
        "--rev",
        "main",
    ]);
    assert!(!attempted.status.success());
    assert!(
        server.join().unwrap(),
        "Git did not send the configured fixture credentials"
    );
    assert!(
        fs::read_to_string(&marker)
            .unwrap()
            .lines()
            .any(|line| line == "get")
    );
    let installed = invoke(&["pkg", "add", source.to_str().unwrap(), "--rev", "main"]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    fs::remove_file(&marker).unwrap();
    let imported = invoke(&[
        "-c",
        "import(path=\"pkg:authenticated\");authenticated.inspect()",
        "--read-only",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    assert!(
        !marker.exists(),
        "offline import invoked the credential helper"
    );
}
