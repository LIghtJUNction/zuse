//! Explicit Git-backed packages. Imports only verify local locked checkouts;
//! network access and publication belong to the `zuse pkg` commands.
use crate::{json, script::syntax};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
use zmcpio::json::{Kind, Value};

type Result<T> = std::result::Result<T, String>;
const MANIFEST: &str = "zuse.json";
const LOCK: &str = "zuse.lock.json";
const MAX_PACKAGES: usize = 64;
const MAX_DEPTH: usize = 16;
const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    git: String,
    rev: String,
}
type Dependencies = BTreeMap<String, Source>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Manifest {
    name: String,
    version: String,
    entry: String,
    exports: BTreeMap<String, String>,
    dependencies: Dependencies,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Package {
    git: String,
    rev: String,
    commit: String,
    version: String,
    dependencies: Dependencies,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Lockfile {
    dependencies: Dependencies,
    packages: BTreeMap<String, Package>,
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
    {
        return Err(format!(
            "invalid package/export name {name:?}; use a lowercase letter followed by lowercase letters, digits, '-' or '_'"
        ));
    }
    Ok(())
}

fn valid_version(version: &str) -> Result<()> {
    if version.len() > 128 {
        return Err("package version exceeds 128 bytes".into());
    }
    let (core, build) = version
        .split_once('+')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    let (core, pre) = core
        .split_once('-')
        .map_or((core, None), |(a, b)| (a, Some(b)));
    for (parts, numeric_zero_rule) in [(pre, true), (build, false)] {
        if let Some(parts) = parts {
            for part in parts.split('.') {
                if part.is_empty()
                    || !part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    || (numeric_zero_rule
                        && part.len() > 1
                        && part.starts_with('0')
                        && part.bytes().all(|c| c.is_ascii_digit()))
                {
                    return Err(format!("invalid semantic version {version:?}"));
                }
            }
        }
    }
    let parts = core.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|c| c.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
                || part.parse::<u64>().is_err()
        })
    {
        return Err(format!(
            "invalid semantic version {version:?}; expected MAJOR.MINOR.PATCH"
        ));
    }
    Ok(())
}

fn valid_source(source: &Source) -> Result<()> {
    let url = &source.git;
    if url.is_empty()
        || url.len() > 4096
        || url.chars().any(char::is_control)
        || !(url.starts_with("https://")
            || url.starts_with("ssh://")
            || url.starts_with("file:///")
            || url.starts_with("git@")
            || Path::new(url).is_absolute())
        || (url.starts_with("https://")
            && url
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap_or("")
                .contains('@'))
    {
        return Err("package Git source must be HTTPS without embedded credentials, SSH, file://, or an absolute local path".into());
    }
    if source.rev.is_empty()
        || source.rev.len() > 256
        || source.rev.starts_with('-')
        || source.rev.starts_with('+')
        || source.rev.contains([':', '*', '?', '[', '\\', '^', '~'])
        || source.rev.contains("..")
        || source.rev.contains("@{")
        || source
            .rev
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("package rev must be one exact Git tag, branch or commit, not a refspec or revision expression".into());
    }
    Ok(())
}

fn valid_commit(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn module_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 1024
        || !path.ends_with(".zuse")
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "module path {path:?} must be a relative .zuse file without '..' or platform prefixes"
        ));
    }
    Ok(())
}

fn object(value: Value<'_>, allowed: Option<&[&str]>) -> Result<()> {
    if value.kind() != Kind::Object {
        return Err("expected a JSON object".into());
    }
    let mut keys = BTreeSet::new();
    for (key, _) in value.members() {
        let key = key.chars().collect::<String>();
        if !keys.insert(key.clone()) {
            return Err(format!("duplicate JSON key {key:?}"));
        }
        if allowed.is_some_and(|allowed| !allowed.contains(&key.as_str())) {
            return Err(format!("unsupported package metadata field {key:?}"));
        }
    }
    Ok(())
}

fn field(value: Value<'_>, key: &str) -> Result<String> {
    json::text(
        value
            .get(key)
            .ok_or_else(|| format!("missing package field {key}"))?,
    )
}

fn schema(value: Value<'_>) -> Result<()> {
    if value.get("schema").and_then(|v| v.integer().ok()) != Some(1) {
        return Err("package metadata requires schema: 1".into());
    }
    Ok(())
}

fn dependencies(value: Option<Value<'_>>) -> Result<Dependencies> {
    let mut result = BTreeMap::new();
    if let Some(value) = value {
        object(value, None)?;
        for (key, item) in value.members() {
            let name = key.chars().collect::<String>();
            valid_name(&name)?;
            object(item, Some(&["git", "rev"]))?;
            let source = Source {
                git: field(item, "git")?,
                rev: field(item, "rev")?,
            };
            valid_source(&source)?;
            result.insert(name, source);
        }
    }
    if result.len() > MAX_PACKAGES {
        return Err("package dependency count exceeds 64".into());
    }
    Ok(result)
}

impl Manifest {
    fn parse(text: &str) -> Result<Self> {
        json::with_value(text, |value| {
            object(
                value,
                Some(&[
                    "schema",
                    "name",
                    "version",
                    "entry",
                    "exports",
                    "dependencies",
                ]),
            )?;
            schema(value)?;
            let name = field(value, "name")?;
            let version = field(value, "version")?;
            let entry = field(value, "entry")?;
            valid_name(&name)?;
            valid_version(&version)?;
            module_path(&entry)?;
            let mut exports = BTreeMap::new();
            if let Some(value) = value.get("exports") {
                object(value, None)?;
                for (key, value) in value.members() {
                    let name = key.chars().collect::<String>();
                    let path = json::text(value)?;
                    valid_name(&name)?;
                    module_path(&path)?;
                    exports.insert(name, path);
                }
            }
            if exports.len() > MAX_FILES {
                return Err("package export count exceeds 512".into());
            }
            let dependencies = dependencies(value.get("dependencies"))?;
            if dependencies.contains_key(&name) {
                return Err("a package cannot depend on itself".into());
            }
            Ok(Self {
                name,
                version,
                entry,
                exports,
                dependencies,
            })
        })
    }

    fn json(&self) -> String {
        let exports = self
            .exports
            .iter()
            .map(|(k, v)| format!("{}:{}", json::quote(k), json::quote(v)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema\":1,\"name\":{},\"version\":{},\"entry\":{},\"exports\":{{{exports}}},\"dependencies\":{}}}\n",
            json::quote(&self.name),
            json::quote(&self.version),
            json::quote(&self.entry),
            dependencies_json(&self.dependencies)
        )
    }
}

fn dependencies_json(dependencies: &Dependencies) -> String {
    format!(
        "{{{}}}",
        dependencies
            .iter()
            .map(|(name, source)| format!(
                "{}:{{\"git\":{},\"rev\":{}}}",
                json::quote(name),
                json::quote(&source.git),
                json::quote(&source.rev)
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

impl Lockfile {
    fn parse(text: &str) -> Result<Self> {
        json::with_value(text, |value| {
            object(value, Some(&["schema", "dependencies", "packages"]))?;
            schema(value)?;
            let roots = dependencies(value.get("dependencies"))?;
            let entries = value
                .get("packages")
                .ok_or("lockfile is missing packages")?;
            object(entries, None)?;
            let mut packages = BTreeMap::new();
            for (key, entry) in entries.members() {
                let name = key.chars().collect::<String>();
                valid_name(&name)?;
                object(
                    entry,
                    Some(&["git", "rev", "commit", "version", "dependencies"]),
                )?;
                let source = Source {
                    git: field(entry, "git")?,
                    rev: field(entry, "rev")?,
                };
                valid_source(&source)?;
                let commit = field(entry, "commit")?;
                if !valid_commit(&commit) {
                    return Err(format!(
                        "package {name} lock requires a full lowercase Git commit"
                    ));
                }
                let version = field(entry, "version")?;
                valid_version(&version)?;
                packages.insert(
                    name,
                    Package {
                        git: source.git,
                        rev: source.rev,
                        commit,
                        version,
                        dependencies: dependencies(entry.get("dependencies"))?,
                    },
                );
            }
            if packages.len() > MAX_PACKAGES {
                return Err("lockfile exceeds 64 packages".into());
            }
            let result = Self {
                dependencies: roots,
                packages,
            };
            result.validate()?;
            Ok(result)
        })
    }

    fn validate(&self) -> Result<()> {
        fn visit(
            lock: &Lockfile,
            name: &str,
            source: &Source,
            stack: &mut Vec<String>,
            seen: &mut BTreeSet<String>,
        ) -> Result<()> {
            if stack.iter().any(|item| item == name) {
                return Err(format!("package dependency cycle through {name}"));
            }
            if stack.len() >= MAX_DEPTH {
                return Err("package dependency depth exceeds 16".into());
            }
            let package = lock
                .packages
                .get(name)
                .ok_or_else(|| format!("lockfile is missing package {name}"))?;
            if package.git != source.git || package.rev != source.rev {
                return Err(format!(
                    "conflicting Git sources or revisions for package {name}"
                ));
            }
            if !seen.insert(name.to_owned()) {
                return Ok(());
            }
            stack.push(name.to_owned());
            for (child, source) in &package.dependencies {
                visit(lock, child, source, stack, seen)?;
            }
            stack.pop();
            Ok(())
        }
        let mut seen = BTreeSet::new();
        for (name, source) in &self.dependencies {
            visit(self, name, source, &mut Vec::new(), &mut seen)?;
        }
        if seen.len() != self.packages.len() {
            return Err("lockfile contains unreachable packages; run zuse pkg install".into());
        }
        Ok(())
    }

    fn json(&self) -> String {
        let packages = self
            .packages
            .iter()
            .map(|(name, p)| {
                format!(
                    "{}:{{\"git\":{},\"rev\":{},\"commit\":{},\"version\":{},\"dependencies\":{}}}",
                    json::quote(name),
                    json::quote(&p.git),
                    json::quote(&p.rev),
                    json::quote(&p.commit),
                    json::quote(&p.version),
                    dependencies_json(&p.dependencies)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema\":1,\"dependencies\":{},\"packages\":{{{packages}}}}}\n",
            dependencies_json(&self.dependencies)
        )
    }
}

fn read(path: &Path, limit: usize) -> Result<String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "{} must be a regular file, not a symlink",
            path.display()
        ));
    }
    let mut data = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()));
    }
    String::from_utf8(data).map_err(|_| format!("{} is not UTF-8", path.display()))
}

fn manifest(root: &Path) -> Result<Manifest> {
    Manifest::parse(&read(&root.join(MANIFEST), MAX_FILE_BYTES)?)
}

fn safe_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut result = root.to_owned();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err("package path escapes its root".into());
        };
        result.push(component);
        let metadata =
            fs::symlink_metadata(&result).map_err(|e| format!("{}: {e}", result.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "package path contains a symlink: {}",
                result.display()
            ));
        }
    }
    Ok(result)
}

fn ensure_dir(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut result = root.to_owned();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err("invalid package cache directory".into());
        };
        result.push(component);
        match fs::symlink_metadata(&result) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
            Ok(_) => {
                return Err(format!(
                    "{} must be a directory, not a symlink",
                    result.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&result).map_err(|e| e.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(result)
}

/// Find the nearest manifest once, before resolving imports for a preparation.
pub fn project_root(base: &Path) -> Result<PathBuf> {
    let base = base
        .canonicalize()
        .map_err(|e| format!("project directory: {e}"))?;
    let start = if base.is_file() {
        base.parent().ok_or("project has no parent")?
    } else {
        &base
    };
    for directory in start.ancestors() {
        if fs::symlink_metadata(directory.join(MANIFEST)).is_ok() {
            manifest(directory)?;
            return Ok(directory.to_owned());
        }
    }
    Err("no zuse.json project found; run zuse pkg init".into())
}

fn git_output(root: &Path, args: &[&str]) -> Result<Output> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let network_settings = matches!(args.first(), Some(&"fetch" | &"push"))
        || args.starts_with(&["remote", "get-url", "--push"]);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.attributesFile=",
            "-c",
            "http.lowSpeedLimit=1",
            "-c",
            "http.lowSpeedTime=30",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null());
    // Explicit fetch/push use the user's configured credentials and SSH/URL
    // settings. Local validation and checkout never load those configurations.
    if !network_settings {
        command
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS");
    }
    let result = command
        .output()
        .map_err(|e| format!("Git is required for packages: {e}"))?;
    if result.stdout.len().saturating_add(result.stderr.len()) > 4 * 1024 * 1024 {
        return Err("Git output exceeded 4 MiB".into());
    }
    Ok(result)
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let result = git_output(root, args)?;
    if !result.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    String::from_utf8(result.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|_| "Git returned non-UTF-8 output".into())
}

fn verify_cache_config(root: &Path) -> Result<()> {
    let config = read(&safe_path(root, Path::new(".git/config"))?, 16 * 1024)?;
    let mut section = "";
    for line in config
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
    {
        if line.starts_with('[') {
            section = match line {
                "[core]" => "core",
                "[remote \"origin\"]" => "origin",
                _ => return Err("unexpected Git configuration in package cache".into()),
            };
        } else {
            let (key, _) = line
                .split_once('=')
                .ok_or("invalid package Git configuration")?;
            let allowed = match section {
                "core" => &[
                    "repositoryformatversion",
                    "filemode",
                    "bare",
                    "logallrefupdates",
                    "ignorecase",
                    "symlinks",
                ] as &[&str],
                "origin" => &["url", "fetch"],
                _ => &[],
            };
            if !allowed.contains(&key.trim()) {
                return Err("unexpected Git configuration in package cache".into());
            }
        }
    }
    Ok(())
}

fn scan_files(root: &Path) -> Result<()> {
    fn visit(path: &Path, depth: usize, files: &mut usize, bytes: &mut u64) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err("package directory depth exceeds 16".into());
        }
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() == ".git" || entry.file_name() == ".zuse" {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "package contains a symlink: {}",
                    entry.path().display()
                ));
            }
            if metadata.is_dir() {
                visit(&entry.path(), depth + 1, files, bytes)?;
            } else if metadata.is_file() {
                *files += 1;
                *bytes += metadata.len();
                if *files > MAX_FILES || *bytes > MAX_PACKAGE_BYTES as u64 {
                    return Err("package exceeds 512 files or 16 MiB".into());
                }
            } else {
                return Err("package contains a non-regular file".into());
            }
        }
        Ok(())
    }
    visit(root, 0, &mut 0, &mut 0)
}

fn parse_spec(spec: &str) -> Result<(&str, Option<&str>)> {
    let spec = spec
        .strip_prefix("pkg:")
        .ok_or("package imports must start with pkg:")?;
    let (name, export) = spec
        .split_once('/')
        .map_or((spec, None), |(a, b)| (a, Some(b)));
    valid_name(name)?;
    if let Some(export) = export {
        valid_name(export)?;
    }
    Ok((name, export))
}

fn validate_modules(root: &Path, package: &Manifest) -> Result<()> {
    fn source(
        root: &Path,
        base: &Path,
        text: &str,
        package: &Manifest,
        stack: &mut Vec<PathBuf>,
        seen: &mut BTreeSet<PathBuf>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err("package import depth exceeds 16".into());
        }
        let program = syntax::parse_module(text).map_err(|e| e.to_string())?;
        for statement in program.statements {
            match statement.kind {
                syntax::StatementKind::Function { .. } => (),
                syntax::StatementKind::Call(call) if call.receiver.is_none() && matches!(call.name.as_str(), "import" | "eval") => {
                    let mut path = None;
                    let mut inline = None;
                    for arg in call.args {
                        match (arg.name.as_deref(), arg.value) {
                            (Some("path"), syntax::Expr::String(value)) => path = Some(value),
                            (Some("source"), syntax::Expr::String(value)) => inline = Some(value),
                            (Some("as"), syntax::Expr::String(_)) | (Some("replace"), syntax::Expr::Bool(_)) => (),
                            _ => return Err("package imports require literal path/source and optional as/replace arguments".into()),
                        }
                    }
                    if path.is_some() == inline.is_some() { return Err("package import requires exactly one path or source".into()); }
                    if let Some(path) = path {
                        if path.starts_with("pkg:") {
                            let (name, _) = parse_spec(&path)?;
                            if !package.dependencies.contains_key(name) { return Err(format!("package {} imports undeclared dependency {name}", package.name)); }
                        } else {
                            if path.contains(['\\', ':']) || !path.ends_with(".zuse") { return Err("package imports must use relative .zuse paths or pkg:name".into()); }
                            let mut relative = base.strip_prefix(root).map_err(|_| "package import escapes its root")?.to_owned();
                            for component in Path::new(&path).components() {
                                match component { Component::Normal(item) => relative.push(item), Component::CurDir => (), Component::ParentDir if relative.pop() => (), _ => return Err("package import escapes its root".into()) }
                            }
                            file(root, &relative, package, stack, seen, depth + 1)?;
                        }
                    } else if let Some(inline) = inline {
                        source(root, base, &inline, package, stack, seen, depth + 1)?;
                    }
                }
                _ => return Err("package modules may contain only function declarations and declarative imports/evals".into()),
            }
        }
        Ok(())
    }
    fn file(
        root: &Path,
        relative: &Path,
        package: &Manifest,
        stack: &mut Vec<PathBuf>,
        seen: &mut BTreeSet<PathBuf>,
        depth: usize,
    ) -> Result<()> {
        let path = safe_path(root, relative)?;
        if stack.contains(&path) {
            return Err("package has a cyclic file import".into());
        }
        if !seen.insert(path.clone()) {
            return Ok(());
        }
        if seen.len() > MAX_FILES {
            return Err("package imported file count exceeds 512".into());
        }
        let text = read(&path, MAX_FILE_BYTES)?;
        verify_blob(root, relative, text.as_bytes())?;
        stack.push(path.clone());
        source(
            root,
            path.parent().ok_or("module has no parent")?,
            &text,
            package,
            stack,
            seen,
            depth,
        )?;
        stack.pop();
        Ok(())
    }
    if !git(root, &["ls-files", "--", ".zuse"])?.is_empty() {
        return Err(".zuse is reserved for local package caches and must not be tracked".into());
    }
    scan_files(root)?;
    let mut seen = BTreeSet::new();
    for entry in std::iter::once(&package.entry).chain(package.exports.values()) {
        file(
            root,
            Path::new(entry),
            package,
            &mut Vec::new(),
            &mut seen,
            0,
        )?;
    }
    Ok(())
}

fn cache(root: &Path, name: &str, commit: &str) -> PathBuf {
    root.join(".zuse/packages").join(name).join(commit)
}

fn verify_blob(root: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    let relative = relative
        .to_str()
        .ok_or("package paths must be UTF-8")?
        .replace('\\', "/");
    let result = git_output(root, &["cat-file", "blob", &format!("HEAD:{relative}")])?;
    if !result.status.success() || result.stdout != bytes {
        return Err(format!(
            "package file {relative} is untracked or differs from its committed content"
        ));
    }
    Ok(())
}

fn verify_checkout(path: &Path, name: &str, package: &Package) -> Result<Manifest> {
    verify_cache_config(path)?;
    if git(path, &["rev-parse", "HEAD"])? != package.commit {
        return Err(format!("package {name} cache HEAD differs from its lock"));
    }
    if git(path, &["remote", "get-url", "origin"])? != package.git {
        return Err(format!("package {name} cache source differs from its lock"));
    }
    if !git(path, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty() {
        return Err(format!(
            "package {name} cache has modified or untracked files; move it aside before reinstalling"
        ));
    }
    let text = read(&path.join(MANIFEST), MAX_FILE_BYTES)?;
    verify_blob(path, Path::new(MANIFEST), text.as_bytes())?;
    let manifest = Manifest::parse(&text)?;
    if manifest.name != name
        || manifest.version != package.version
        || manifest.dependencies != package.dependencies
    {
        return Err(format!("package {name} manifest differs from its lock"));
    }
    validate_modules(path, &manifest)?;
    Ok(manifest)
}

/// Resolve a locked package locally. Never fetch, install, run hooks or execute code.
pub fn resolve_import(root: &Path, spec: &str) -> Result<PathBuf> {
    (|| {
        let (name, export) = parse_spec(spec)?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let manifest = manifest(&root)?;
        let lock = Lockfile::parse(&read(&root.join(LOCK), MAX_FILE_BYTES)?)?;
        if lock.dependencies != manifest.dependencies {
            return Err("package manifest and lockfile differ".into());
        }
        let package = lock
            .packages
            .get(name)
            .ok_or_else(|| format!("package {name} is not installed"))?;
        let path = safe_path(
            &root,
            &Path::new(".zuse/packages").join(name).join(&package.commit),
        )?;
        let package_manifest = verify_checkout(&path, name, package)?;
        let entry = if let Some(export) = export {
            package_manifest
                .exports
                .get(export)
                .ok_or_else(|| format!("package {name} does not export {export}"))?
        } else {
            &package_manifest.entry
        };
        safe_path(&path, Path::new(entry))
    })()
    .map_err(|error: String| format!("{error}; run zuse pkg install"))
}

struct Temporary(PathBuf);
impl Temporary {
    fn new(root: &Path) -> Result<Self> {
        let directory = ensure_dir(root, Path::new(".zuse"))?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = directory.join(format!(
            ".pkg-stage-{}-{nonce}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn atomic_write(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let destination = root.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => (),
        Ok(_) => return Err(format!("{} must be a regular file", destination.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.to_string()),
    }
    let temporary = root.join(format!(
        ".{name}-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, &destination).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn read_lock(root: &Path) -> Result<Option<Lockfile>> {
    match fs::symlink_metadata(root.join(LOCK)) {
        Ok(_) => Lockfile::parse(&read(&root.join(LOCK), MAX_FILE_BYTES)?).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

struct Resolver<'a> {
    root: &'a Path,
    stage: Temporary,
    old: Lockfile,
    refresh: BTreeSet<String>,
    refresh_all: bool,
    packages: BTreeMap<String, Package>,
    staged: Vec<(String, String, PathBuf)>,
    prefetched: BTreeMap<(String, String), (PathBuf, Manifest, String)>,
    fetches: usize,
}

impl<'a> Resolver<'a> {
    fn new(root: &'a Path, old: Lockfile) -> Result<Self> {
        Ok(Self {
            root,
            stage: Temporary::new(root)?,
            old,
            refresh: BTreeSet::new(),
            refresh_all: false,
            packages: BTreeMap::new(),
            staged: Vec::new(),
            prefetched: BTreeMap::new(),
            fetches: 0,
        })
    }

    fn fetch(&mut self, source: &Source, selector: &str) -> Result<(PathBuf, Manifest, String)> {
        if self.fetches > MAX_PACKAGES {
            return Err("package fetch count exceeds 65".into());
        }
        let path = self.stage.0.join(format!("checkout-{}", self.fetches));
        self.fetches += 1;
        let destination = path.to_str().ok_or("package cache path must be UTF-8")?;
        git(
            &self.stage.0,
            &["init", "--quiet", "--template=", destination],
        )?;
        git(&path, &["remote", "add", "origin", &source.git])?;
        git(
            &path,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--depth=1",
                "origin",
                selector,
            ],
        )?;
        git(&path, &["checkout", "--quiet", "--detach", "FETCH_HEAD"])?;
        let commit = git(&path, &["rev-parse", "HEAD"])?;
        if !valid_commit(&commit) {
            return Err("Git did not return a full package commit".into());
        }
        let manifest = manifest(&path)?;
        validate_modules(&path, &manifest)?;
        Ok((path, manifest, commit))
    }

    fn discover(&mut self, source: &Source) -> Result<String> {
        let fetched = self.fetch(source, &source.rev)?;
        let name = fetched.1.name.clone();
        self.prefetched
            .insert((source.git.clone(), source.rev.clone()), fetched);
        self.refresh.insert(name.clone());
        Ok(name)
    }

    fn locked_checkout(&mut self, name: &str, package: &Package) -> Result<PathBuf> {
        let path = cache(self.root, name, &package.commit);
        if fs::symlink_metadata(&path).is_ok() {
            safe_path(
                self.root,
                &Path::new(".zuse/packages").join(name).join(&package.commit),
            )?;
            verify_checkout(&path, name, package)?;
            return Ok(path);
        }
        let source = Source {
            git: package.git.clone(),
            rev: package.rev.clone(),
        };
        let (path, _, commit) = self.fetch(&source, &package.commit)?;
        if commit != package.commit {
            return Err(format!("package {name} fetch returned another commit"));
        }
        verify_checkout(&path, name, package)?;
        self.staged
            .push((name.to_owned(), package.commit.clone(), path.clone()));
        Ok(path)
    }

    fn resolve(&mut self, name: &str, source: &Source, stack: &mut Vec<String>) -> Result<()> {
        if stack.iter().any(|item| item == name) {
            return Err(format!("package dependency cycle through {name}"));
        }
        if stack.len() >= MAX_DEPTH {
            return Err("package dependency depth exceeds 16".into());
        }
        if let Some(package) = self.packages.get(name) {
            if package.git != source.git || package.rev != source.rev {
                return Err(format!(
                    "conflicting Git sources or revisions for package {name}"
                ));
            }
            return Ok(());
        }
        if self.packages.len() + stack.len() >= MAX_PACKAGES {
            return Err("package graph exceeds 64 packages".into());
        }
        let previous = self
            .old
            .packages
            .get(name)
            .filter(|p| {
                p.git == source.git
                    && p.rev == source.rev
                    && !self.refresh_all
                    && !self.refresh.contains(name)
            })
            .cloned();
        let package = if let Some(package) = previous {
            self.locked_checkout(name, &package)?;
            package
        } else {
            let (path, manifest, commit) = if let Some(value) = self
                .prefetched
                .remove(&(source.git.clone(), source.rev.clone()))
            {
                value
            } else {
                self.fetch(source, &source.rev)?
            };
            if manifest.name != name {
                return Err(format!(
                    "dependency {name} declares package name {}",
                    manifest.name
                ));
            }
            let package = Package {
                git: source.git.clone(),
                rev: source.rev.clone(),
                commit: commit.clone(),
                version: manifest.version,
                dependencies: manifest.dependencies,
            };
            verify_checkout(&path, name, &package)?;
            self.staged.push((name.to_owned(), commit, path));
            package
        };
        stack.push(name.to_owned());
        for (child, source) in &package.dependencies {
            self.resolve(child, source, stack)?;
        }
        stack.pop();
        self.packages.insert(name.to_owned(), package);
        Ok(())
    }

    fn finish(self, manifest: &Manifest, change_manifest: bool) -> Result<String> {
        let lock = Lockfile {
            dependencies: manifest.dependencies.clone(),
            packages: self.packages.clone(),
        };
        lock.validate()?;
        let lock_text = lock.json();
        if lock_text.len() > MAX_FILE_BYTES {
            return Err("package lock exceeds 1 MiB".into());
        }
        for (name, commit, path) in &self.staged {
            let destination = cache(self.root, name, commit);
            if fs::symlink_metadata(&destination).is_ok() {
                safe_path(
                    self.root,
                    &Path::new(".zuse/packages").join(name).join(commit),
                )?;
                verify_checkout(
                    &destination,
                    name,
                    lock.packages.get(name).ok_or("missing resolved package")?,
                )?;
            } else {
                ensure_dir(self.root, &Path::new(".zuse/packages").join(name))?;
                fs::rename(path, destination).map_err(|e| e.to_string())?;
            }
        }
        let previous = if change_manifest {
            Some(read(&self.root.join(MANIFEST), MAX_FILE_BYTES)?)
        } else {
            None
        };
        if change_manifest {
            atomic_write(self.root, MANIFEST, manifest.json().as_bytes())?;
        }
        if let Err(error) = atomic_write(self.root, LOCK, lock_text.as_bytes()) {
            if let Some(previous) = previous {
                atomic_write(self.root, MANIFEST, previous.as_bytes())
                    .map_err(|rollback| format!("{error}; manifest restore failed: {rollback}"))?;
            }
            return Err(error);
        }
        Ok(format!(
            "{{\"status\":\"installed\",\"packages\":{},\"lock\":{}}}",
            lock.packages.len(),
            json::quote(LOCK)
        ))
    }
}

fn initialize(base: &Path, name: Option<&str>) -> Result<String> {
    let name = name
        .or_else(|| base.file_name().and_then(|s| s.to_str()))
        .ok_or("pkg init needs a package name")?;
    valid_name(name)?;
    if fs::symlink_metadata(base.join(MANIFEST)).is_ok() {
        return Err("zuse.json already exists; edit it directly".into());
    }
    let manifest = Manifest {
        name: name.into(),
        version: "0.1.0".into(),
        entry: "lib.zuse".into(),
        exports: BTreeMap::new(),
        dependencies: BTreeMap::new(),
    };
    if fs::symlink_metadata(base.join("lib.zuse")).is_err() {
        atomic_write(
            base,
            "lib.zuse",
            b"# Package functions and declarative imports belong here.\n",
        )?;
    } else {
        read(&base.join("lib.zuse"), MAX_FILE_BYTES)?;
    }
    let ignore = base.join(".gitignore");
    let mut ignored = if ignore.exists() {
        read(&ignore, MAX_FILE_BYTES)?
    } else {
        String::new()
    };
    if !ignored
        .lines()
        .any(|line| matches!(line.trim(), ".zuse" | ".zuse/" | "/.zuse/"))
    {
        if !ignored.is_empty() && !ignored.ends_with('\n') {
            ignored.push('\n');
        }
        ignored.push_str(".zuse/\n");
        atomic_write(base, ".gitignore", ignored.as_bytes())?;
    }
    atomic_write(base, MANIFEST, manifest.json().as_bytes())?;
    Ok(format!(
        "{{\"status\":\"initialized\",\"name\":{},\"manifest\":{}}}",
        json::quote(name),
        json::quote(MANIFEST)
    ))
}

fn publish(root: &Path, manifest: &Manifest, dry_run: bool) -> Result<String> {
    let top = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if top != root {
        return Err("publish requires zuse.json at the Git repository root".into());
    }
    if !git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty() {
        return Err("publish requires a clean Git checkout; commit package changes first".into());
    }
    let text = read(&root.join(MANIFEST), MAX_FILE_BYTES)?;
    verify_blob(root, Path::new(MANIFEST), text.as_bytes())?;
    validate_modules(root, manifest)?;
    if !manifest.dependencies.is_empty() {
        let lock = read_lock(root)?
            .ok_or("package dependencies need zuse.lock.json; run zuse pkg install")?;
        if lock.dependencies != manifest.dependencies {
            return Err("package manifest differs from its lock; run zuse pkg install".into());
        }
        for (name, package) in &lock.packages {
            let path = safe_path(
                root,
                &Path::new(".zuse/packages").join(name).join(&package.commit),
            )?;
            verify_checkout(&path, name, package)?;
        }
    }
    let origins = git(root, &["remote", "get-url", "--push", "--all", "origin"])?;
    let mut origins = origins.lines();
    let origin = origins.next().ok_or("origin has no push URL")?.to_owned();
    if origins.next().is_some() {
        return Err("publish requires exactly one origin push URL".into());
    }
    valid_source(&Source {
        git: origin.clone(),
        rev: "HEAD".into(),
    })?;
    let commit = git(root, &["rev-parse", "HEAD"])?;
    let tag = format!("v{}", manifest.version);
    let reference = format!("refs/tags/{tag}");
    let existing = git_output(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    if existing.status.success() && String::from_utf8_lossy(&existing.stdout).trim() != commit {
        return Err(format!("tag {tag} already points to another commit"));
    }
    if !dry_run {
        if !existing.status.success() {
            git(root, &["tag", "--no-sign", &tag, &commit])?;
        }
        git(
            root,
            &[
                "push",
                "--porcelain",
                &origin,
                &format!("{reference}:{reference}"),
            ],
        )?;
    }
    Ok(format!(
        "{{\"status\":{},\"name\":{},\"version\":{},\"origin\":{},\"commit\":{},\"tag\":{},\"pushes_branch\":false}}",
        json::quote(if dry_run { "planned" } else { "published" }),
        json::quote(&manifest.name),
        json::quote(&manifest.version),
        json::quote(&origin),
        json::quote(&commit),
        json::quote(&tag)
    ))
}

pub const HELP: &str = "zuse pkg init [NAME]\nzuse pkg add URL --rev REV\nzuse pkg install [--locked]\nzuse pkg update [NAME]\nzuse pkg remove NAME\nzuse pkg list\nzuse pkg publish [--dry-run]\n";

/// Run a package command relative to an explicit directory, without changing cwd.
pub fn run_at(base: &Path, args: &[&str]) -> Result<String> {
    let base = base
        .canonicalize()
        .map_err(|e| format!("package directory: {e}"))?;
    if !base.is_dir() {
        return Err("package commands require a directory".into());
    }
    match args {
        [] | ["help"] | ["--help"] | ["-h"] => {
            return Ok(format!(
                "{{\"status\":\"help\",\"usage\":{}}}",
                json::quote(HELP)
            ));
        }
        ["init"] => return initialize(&base, None),
        ["init", name] if !name.starts_with('-') => return initialize(&base, Some(name)),
        _ => (),
    }
    let root = project_root(&base)?;
    let mut manifest = manifest(&root)?;
    if matches!(args, ["publish"] | ["publish", "--dry-run"]) {
        return publish(&root, &manifest, args.len() == 2);
    }
    let old = read_lock(&root)?;
    if args == ["list"] {
        let packages = old.as_ref().map(|lock| lock.packages.iter().map(|(name, package)| {
            let installed = safe_path(&root, &Path::new(".zuse/packages").join(name).join(&package.commit)).and_then(|p| verify_checkout(&p, name, package)).is_ok();
            format!("{{\"name\":{},\"version\":{},\"commit\":{},\"installed\":{installed}}}", json::quote(name), json::quote(&package.version), json::quote(&package.commit))
        }).collect::<Vec<_>>().join(",")).unwrap_or_default();
        return Ok(format!(
            "{{\"status\":\"listed\",\"locked\":{},\"dependencies\":{},\"packages\":[{packages}]}}",
            old.is_some(),
            dependencies_json(&manifest.dependencies)
        ));
    }
    let locked = args == ["install", "--locked"];
    if locked
        && old
            .as_ref()
            .is_none_or(|lock| lock.dependencies != manifest.dependencies)
    {
        return Err("--locked requires a matching zuse.lock.json; run zuse pkg install".into());
    }
    let mut resolver = Resolver::new(&root, old.unwrap_or_default())?;
    let mut change_manifest = false;
    match args {
        ["install"] | ["install", "--locked"] => (),
        ["add", url, "--rev", rev] => {
            let source = Source {
                git: (*url).into(),
                rev: (*rev).into(),
            };
            valid_source(&source)?;
            let name = resolver.discover(&source)?;
            if name == manifest.name {
                return Err("a package cannot depend on itself".into());
            }
            manifest.dependencies.insert(name, source);
            change_manifest = true;
        }
        ["update"] => resolver.refresh_all = true,
        ["update", name] => {
            valid_name(name)?;
            if !resolver.old.packages.contains_key(*name)
                && !manifest.dependencies.contains_key(*name)
            {
                return Err(format!("package {name} is not installed"));
            }
            resolver.refresh.insert((*name).to_owned());
        }
        ["remove", name] => {
            valid_name(name)?;
            if manifest.dependencies.remove(*name).is_none() {
                return Err(format!("package {name} is not a direct dependency"));
            }
            change_manifest = true;
        }
        _ => return Err(format!("invalid package command; usage:\n{HELP}")),
    }
    if locked {
        let packages = resolver.old.packages.clone();
        for (name, package) in &packages {
            resolver.locked_checkout(name, package)?;
        }
        resolver.packages = packages;
    } else {
        for (name, source) in &manifest.dependencies {
            resolver.resolve(name, source, &mut vec![manifest.name.clone()])?;
        }
    }
    resolver.finish(&manifest, change_manifest)
}

pub fn run(args: &[&str]) -> Result<String> {
    run_at(&std::env::current_dir().map_err(|e| e.to_string())?, args)
}
