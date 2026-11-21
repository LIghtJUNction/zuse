//! CLI updates delegate installation and tracking to Cargo. No system prefix,
//! untracked executable or different package is overwritten by this module.
use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zmcpio::json::{Kind, Value};
use zuse::integrations::ProcessManager;
use zuse::json::{self, quote};

const PACKAGE: &str = "zuse-computer";
const REPOSITORY: &str = "https://github.com/LIghtJUNction/zuse.git";
const REGISTRY_API: &str = "https://crates.io/api/v1/crates/zuse-computer";
const RELEASE_API: &str = "https://api.github.com/repos/LIghtJUNction/zuse/releases/latest";
const METADATA_LIMIT: usize = 128 * 1024;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(35);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, PartialEq, Eq)]
struct Version {
    core: [u64; 3],
    prerelease: bool,
    build: bool,
}

impl Version {
    fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 128 {
            return Err("version exceeds 128 bytes".into());
        }
        let (number, build) = text
            .split_once('+')
            .map_or((text, None), |(a, b)| (a, Some(b)));
        let (number, pre) = number
            .split_once('-')
            .map_or((number, None), |(a, b)| (a, Some(b)));
        for (identifiers, numeric_zero_rule) in [(pre, true), (build, false)] {
            if let Some(identifiers) = identifiers {
                for part in identifiers.split('.') {
                    if part.is_empty()
                        || !part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                        || (numeric_zero_rule
                            && part.len() > 1
                            && part.starts_with('0')
                            && part.bytes().all(|c| c.is_ascii_digit()))
                    {
                        return Err(format!("invalid semantic version: {text}"));
                    }
                }
            }
        }
        let parts = number.split('.').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(format!("expected MAJOR.MINOR.PATCH version: {text}"));
        }
        let mut core = [0; 3];
        for (output, part) in core.iter_mut().zip(parts) {
            if part.is_empty()
                || !part.bytes().all(|c| c.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(format!("invalid semantic version: {text}"));
            }
            *output = part
                .parse()
                .map_err(|_| format!("version component overflows: {text}"))?;
        }
        Ok(Self {
            core,
            prerelease: pre.is_some(),
            build: build.is_some(),
        })
    }

    fn stable(text: &str) -> Result<Self, String> {
        let version = Self::parse(text)?;
        if version.prerelease || version.build {
            return Err("update target must be a plain stable MAJOR.MINOR.PATCH version".into());
        }
        Ok(version)
    }

    fn newer_than(&self, current: &Self) -> bool {
        match self.core.cmp(&current.core) {
            Ordering::Greater => true,
            Ordering::Equal => current.prerelease && !self.prerelease,
            Ordering::Less => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Registry,
    Git,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Self::Registry => "cargo_registry",
            Self::Git => "cargo_git",
        }
    }
    fn channel(self) -> &'static str {
        match self {
            Self::Registry => "crates.io-stable",
            Self::Git => "github-stable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CargoInstall {
    source: Source,
    features: Vec<String>,
    all_features: bool,
    no_default_features: bool,
    profile: String,
    target: String,
}

#[derive(Debug)]
struct Installation {
    executable: PathBuf,
    root: Option<PathBuf>,
    cargo: Option<CargoInstall>,
    kind: &'static str,
    reason: Option<String>,
}

fn field(value: Value<'_>, name: &str) -> Result<String, String> {
    json::text(value.get(name).ok_or_else(|| format!("missing {name}"))?)
}

fn official_repository(url: &str) -> bool {
    let url = url.strip_suffix('/').unwrap_or(url);
    url.strip_suffix(".git")
        .unwrap_or(url)
        .eq_ignore_ascii_case("https://github.com/LIghtJUNction/zuse")
}

fn cargo_source(identity: &str, current: &str) -> Result<Source, String> {
    let prefix = format!("{PACKAGE} {current} (");
    let source = identity
        .strip_prefix(&prefix)
        .and_then(|s| s.strip_suffix(')'))
        .ok_or("Cargo tracks this executable as another package or version")?;
    if matches!(
        source,
        "registry+https://github.com/rust-lang/crates.io-index"
            | "registry+https://index.crates.io/"
            | "sparse+https://index.crates.io/"
    ) {
        return Ok(Source::Registry);
    }
    if let Some(git) = source.strip_prefix("git+") {
        let (location, revision) = git
            .rsplit_once('#')
            .ok_or("Cargo Git source has no revision")?;
        if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("Cargo Git source has an invalid revision".into());
        }
        let (url, selector) = location
            .split_once('?')
            .map_or((location, None), |(a, b)| (a, Some(b)));
        if !official_repository(url) {
            return Err("Cargo Git source is not the official Zuse repository".into());
        }
        if let Some(selector) = selector {
            let tag = selector
                .strip_prefix("tag=v")
                .ok_or("an explicitly pinned Cargo branch/revision needs a manual update")?;
            Version::stable(tag)?;
        }
        return Ok(Source::Git);
    }
    Err("this Cargo source needs a manual update; only crates.io and official Zuse Git installs are supported".into())
}

fn parse_tracking(text: &str, current: &str) -> Result<CargoInstall, String> {
    json::with_value(text, |root| {
        let installs = root
            .get("installs")
            .filter(|v| v.kind() == Kind::Object)
            .ok_or("Cargo tracking metadata has no installs object")?;
        let mut owner = None;
        for (identity, value) in installs.members() {
            let bins = value
                .get("bins")
                .filter(|v| v.kind() == Kind::Array)
                .ok_or("Cargo tracking entry has no bins array")?;
            let bins = bins
                .elements()
                .map(json::text)
                .collect::<Result<Vec<_>, _>>()?;
            if !bins.iter().any(|name| name == "zuse" || name == "zuse.exe") {
                continue;
            }
            if owner.is_some() || bins.len() != 1 {
                return Err(
                    "ambiguous or multi-binary Cargo installation; update it manually".into(),
                );
            }
            let source = cargo_source(&identity.chars().collect::<String>(), current)?;
            let features = value
                .get("features")
                .filter(|v| v.kind() == Kind::Array)
                .ok_or("Cargo tracking entry has no features array")?
                .elements()
                .map(json::text)
                .collect::<Result<Vec<_>, _>>()?;
            if features.len() > 64
                || features.iter().any(|v| {
                    v.is_empty()
                        || v.len() > 128
                        || !v
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_+-./:".contains(&c))
                })
            {
                return Err("unsupported Cargo feature metadata".into());
            }
            let boolean = |key| {
                value
                    .get(key)
                    .and_then(|v| v.boolean())
                    .ok_or_else(|| format!("Cargo tracking entry has no boolean {key}"))
            };
            let profile = field(value, "profile")?;
            let target = field(value, "target")?;
            for item in [&profile, &target] {
                if item.is_empty()
                    || item.len() > 128
                    || !item
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                {
                    return Err("custom Cargo profile/target metadata needs a manual update".into());
                }
            }
            owner = Some(CargoInstall {
                source,
                features,
                all_features: boolean("all_features")?,
                no_default_features: boolean("no_default_features")?,
                profile,
                target,
            });
        }
        owner.ok_or_else(|| "Cargo does not track this zuse executable".into())
    })
}

fn system_prefix(path: &Path) -> bool {
    #[cfg(unix)]
    {
        ["/usr", "/bin", "/sbin", "/opt", "/nix", "/snap"]
            .iter()
            .any(|p| path.starts_with(p))
    }
    #[cfg(windows)]
    {
        let path = path
            .to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\");
        path.contains("\\program files\\")
            || path.contains("\\program files (x86)\\")
            || path.contains("\\programdata\\chocolatey\\")
            || path.contains("\\scoop\\apps\\")
            || path.contains("\\windowsapps\\")
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

fn inspect(executable: &Path, current: &str) -> Result<Installation, String> {
    let executable =
        std::fs::canonicalize(executable).map_err(|e| format!("locate current executable: {e}"))?;
    let mut installation = Installation {
        executable,
        root: None,
        cargo: None,
        kind: "untracked",
        reason: None,
    };
    if system_prefix(&installation.executable) {
        installation.kind = "system_or_package_manager";
        installation.reason = Some("Use the package manager that installed this executable. For Arch/AUR installations, use your normal paru -Syu or yay -Syu workflow. System prefixes are never changed by zuse update.".into());
        return Ok(installation);
    }
    let bin = installation
        .executable
        .parent()
        .ok_or("executable has no parent")?;
    let is_binary = installation
        .executable
        .file_name()
        .is_some_and(|v| v == "zuse" || v == "zuse.exe");
    if !is_binary || bin.file_name().is_none_or(|v| v != "bin") {
        installation.reason = Some("This executable is outside a tracked Cargo bin directory. Update the source build or download the matching official release manually: https://github.com/LIghtJUNction/zuse/releases".into());
        return Ok(installation);
    }
    let root = bin
        .parent()
        .ok_or("Cargo bin directory has no installation root")?
        .to_path_buf();
    let tracking = root.join(".crates2.json");
    installation.root = Some(root);
    let metadata = match std::fs::symlink_metadata(&tracking) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            installation.reason = Some("No Cargo .crates2.json ownership record exists beside bin. Portable, --no-track and legacy installations require a manual update; no alternative installation prefix is selected.".into());
            return Ok(installation);
        }
        Err(e) => return Err(format!("read Cargo tracking metadata: {e}")),
    };
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
        return Err(
            "Cargo tracking metadata must be a regular, non-symlink file of at most 2 MiB".into(),
        );
    }
    let mut text = String::new();
    std::fs::File::open(tracking)
        .and_then(|f| f.take(2 * 1024 * 1024 + 1).read_to_string(&mut text))
        .map_err(|e| format!("read Cargo tracking metadata: {e}"))?;
    let cargo = parse_tracking(&text, current)?;
    installation.kind = cargo.source.name();
    installation.cargo = Some(cargo);
    Ok(installation)
}

fn latest_version(text: &str, source: Source) -> Result<String, String> {
    json::with_value(text, |root| {
        let version = match source {
            Source::Registry => {
                let package = root.get("crate").ok_or("registry response has no crate")?;
                if field(package, "name")? != PACKAGE
                    || !official_repository(&field(package, "repository")?)
                {
                    return Err(
                        "registry response does not identify the official zuse-computer package"
                            .into(),
                    );
                }
                let version = field(package, "max_stable_version")?;
                let versions = root
                    .get("versions")
                    .filter(|v| v.kind() == Kind::Array)
                    .ok_or("registry response has no versions array")?;
                if !versions.elements().any(|v| {
                    field(v, "num").is_ok_and(|n| n == version)
                        && v.get("yanked").and_then(|v| v.boolean()) == Some(false)
                }) {
                    return Err("registry stable version is missing or yanked".into());
                }
                version
            }
            Source::Git => {
                if root.get("draft").and_then(|v| v.boolean()) != Some(false)
                    || root.get("prerelease").and_then(|v| v.boolean()) != Some(false)
                {
                    return Err("GitHub response is not a public stable release".into());
                }
                let tag = field(root, "tag_name")?;
                let expected = format!("https://github.com/LIghtJUNction/zuse/releases/tag/{tag}");
                if !field(root, "html_url")?.eq_ignore_ascii_case(&expected) {
                    return Err("GitHub response is not from the official Zuse repository".into());
                }
                tag.strip_prefix('v')
                    .ok_or("release tag must begin with v")?
                    .to_owned()
            }
        };
        Version::stable(&version)?;
        Ok(version)
    })
}

fn program(name: &str) -> Result<PathBuf, String> {
    let paths = std::env::var_os("PATH").ok_or("PATH is not set")?;
    for directory in std::env::split_paths(&paths) {
        let path = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        let Ok(metadata) = path.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        // Keep the final filename: rustup dispatches Cargo by argv[0].
        return if path.is_absolute() {
            Ok(path)
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .map_err(|e| e.to_string())
        };
    }
    Err(format!(
        "{name} is required for zuse update but was not found on PATH"
    ))
}

fn process(
    program: &Path,
    args: &[String],
    timeout: Duration,
    progress: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    if cancelled() {
        return Err("update cancelled before starting a child process".into());
    }
    let program = program
        .to_str()
        .ok_or("update executable path must be UTF-8")?;
    if args.iter().any(|a| a.chars().any(char::is_control)) {
        return Err("update arguments must not contain control characters".into());
    }
    let arguments = args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
    let mut manager = ProcessManager::new();
    manager.start(program, &arguments)?;
    let deadline = Instant::now() + timeout;
    let mut stdout = String::new();
    let mut stderr = String::new();
    loop {
        if cancelled() {
            return Err("update cancelled; owned child processes stopped; check zuse --version before retrying an installation".into());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{program} timed out after {} seconds; owned child processes stopped",
                timeout.as_secs()
            ));
        }
        let receipt = manager.read(1)?;
        let state = json::field(&receipt, "state")?;
        let out = json::field(&receipt, "stdout")?;
        let err = json::field(&receipt, "stderr")?;
        let code = json::with_value(&receipt, |v| {
            for name in ["stdout_dropped", "stderr_dropped"] {
                if v.get(name).and_then(|v| v.integer().ok()) != Some(0) {
                    return Err("update child output exceeded its bounded buffer".into());
                }
            }
            Ok(v.get("exit_code").and_then(|v| v.integer().ok()))
        })?;
        if progress {
            eprint!("{out}{err}");
        } else {
            if stdout.len().saturating_add(out.len()) > METADATA_LIMIT {
                return Err("update response exceeds 128 KiB".into());
            }
            stdout.push_str(&out);
        }
        stderr.push_str(&err);
        if stderr.len() > 8192 {
            let mut start = stderr.len() - 8192;
            while !stderr.is_char_boundary(start) {
                start += 1;
            }
            stderr.drain(..start);
        }
        if state != "running" {
            if code == Some(0) {
                return Ok(stdout);
            }
            return Err(format!(
                "{program} failed (exit {}): {}",
                code.map_or("signal".into(), |n| n.to_string()),
                stderr.trim()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fetch(source: Source, cancelled: &dyn Fn() -> bool) -> Result<String, String> {
    let url = match source {
        Source::Registry => REGISTRY_API,
        Source::Git => RELEASE_API,
    };
    let args = [
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-redirs",
        "3",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "10",
        "--max-time",
        "30",
        "--max-filesize",
        "131072",
        "--output",
        "-",
        "--user-agent",
        concat!("zuse/", env!("CARGO_PKG_VERSION")),
        "--header",
        "Accept: application/json",
        url,
    ]
    .map(String::from);
    let text = process(&program("curl")?, &args, LOOKUP_TIMEOUT, false, cancelled)?;
    latest_version(&text, source)
}

fn verify_git_tag(version: &str, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
    let tag = format!("refs/tags/v{version}");
    let args = ["ls-remote", "--exit-code", "--tags", "--", REPOSITORY, &tag].map(String::from);
    let output = process(&program("git")?, &args, LOOKUP_TIMEOUT, false, cancelled)?;
    let lines = output.lines().collect::<Vec<_>>();
    if lines.len() != 1 {
        return Err(format!("official stable tag {tag} is absent or ambiguous"));
    }
    let fields = lines[0].split_whitespace().collect::<Vec<_>>();
    if fields.len() != 2
        || fields[1] != tag
        || !matches!(fields[0].len(), 40 | 64)
        || !fields[0].bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(format!(
            "official stable tag {tag} did not return a valid object identity"
        ));
    }
    Ok(())
}

fn cargo_arguments(installation: &Installation, latest: &str) -> Result<Vec<String>, String> {
    let cargo = installation
        .cargo
        .as_ref()
        .ok_or("installation is not managed by Cargo")?;
    let root = installation
        .root
        .as_ref()
        .and_then(|p| p.to_str())
        .ok_or("Cargo root must be UTF-8")?;
    let mut args = vec![
        "install".into(),
        PACKAGE.into(),
        "--locked".into(),
        "--bin".into(),
        "zuse".into(),
        "--root".into(),
        root.into(),
    ];
    match cargo.source {
        Source::Registry => args.extend([
            "--registry".into(),
            "crates-io".into(),
            "--version".into(),
            format!("={latest}"),
        ]),
        Source::Git => args.extend([
            "--git".into(),
            REPOSITORY.into(),
            "--tag".into(),
            format!("v{latest}"),
        ]),
    }
    args.extend([
        "--profile".into(),
        cargo.profile.clone(),
        "--target".into(),
        cargo.target.clone(),
    ]);
    if cargo.all_features {
        args.push("--all-features".into());
    }
    if cargo.no_default_features {
        args.push("--no-default-features".into());
    }
    if !cargo.features.is_empty() {
        args.extend(["--features".into(), cargo.features.join(",")]);
    }
    Ok(args)
}

fn receipt(
    status: &str,
    install: &Installation,
    latest: &str,
    available: bool,
    args: &[String],
    error: Option<&str>,
) -> String {
    let source = install
        .cargo
        .as_ref()
        .map_or(Source::Registry, |c| c.source);
    format!(
        "{{\"status\":{},\"current_version\":{},\"latest_version\":{},\"channel\":{},\"update_available\":{available},\"can_update\":{},\"installation\":{{\"kind\":{},\"executable\":{},\"root\":{}}},\"command\":[{}],\"reason\":{}{} }}",
        quote(status),
        quote(if status == "updated" {
            latest
        } else {
            zuse::VERSION
        }),
        quote(latest),
        quote(source.channel()),
        install.cargo.is_some(),
        quote(install.kind),
        quote(&install.executable.to_string_lossy()),
        install
            .root
            .as_ref()
            .map_or("null".into(), |p| quote(&p.to_string_lossy())),
        args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(","),
        install.reason.as_deref().map_or("null".into(), quote),
        error.map_or(String::new(), |e| format!(",\"error\":{}", quote(e)))
    )
}

pub fn run(check_only: bool, dry_run: bool) -> Result<String, String> {
    let current = Version::parse(zuse::VERSION)?;
    let executable = std::env::current_exe().map_err(|e| format!("locate running Zuse: {e}"))?;
    let installation = inspect(&executable, zuse::VERSION)?;
    let source = installation
        .cargo
        .as_ref()
        .map_or(Source::Registry, |c| c.source);
    let interrupt = zuse::script::interrupt::InterruptGuard::install()?;
    let cancelled = || interrupt.cancelled();
    eprintln!(
        "Checking the latest stable Zuse version ({})…",
        source.channel()
    );
    let latest = fetch(source, &cancelled)?;
    let available = Version::stable(&latest)?.newer_than(&current);
    let mut command = Vec::new();
    if available && installation.cargo.is_some() {
        if source == Source::Git {
            verify_git_tag(&latest, &cancelled)?;
        }
        command.push("cargo".into());
        command.extend(cargo_arguments(&installation, &latest)?);
    }
    if check_only || dry_run || !available {
        let status = if !available {
            "up_to_date"
        } else if dry_run {
            "dry_run"
        } else {
            "update_available"
        };
        return Ok(receipt(
            status,
            &installation,
            &latest,
            available,
            &command,
            None,
        ));
    }
    if installation.cargo.is_none() {
        return Ok(receipt(
            "failed",
            &installation,
            &latest,
            true,
            &[],
            Some("automatic update is unavailable for this installation; follow the reason field"),
        ));
    }
    let fresh = inspect(&executable, zuse::VERSION)?;
    if fresh.executable != installation.executable
        || fresh.root != installation.root
        || fresh.cargo != installation.cargo
    {
        return Err(
            "installation changed during update discovery; inspect it before retrying".into(),
        );
    }
    let cargo = program("cargo")?;
    eprintln!(
        "Installing Zuse {latest} into {}…",
        installation.root.as_ref().unwrap().display()
    );
    process(&cargo, &command[1..], INSTALL_TIMEOUT, true, &cancelled)?;
    let installed = inspect(&installation.executable, &latest).map_err(|e| {
        format!("Cargo returned success but installed ownership verification failed: {e}")
    })?;
    if installed.cargo.as_ref().map(|c| c.source) != Some(source) {
        return Err("Cargo returned success but the installation source changed".into());
    }
    let version = process(
        &installation.executable,
        &["--version".into()],
        Duration::from_secs(5),
        false,
        &cancelled,
    )?;
    if version.trim() != format!("zuse {latest}") {
        return Err(format!(
            "Cargo returned success but the executable reports an unexpected version: {}",
            version.trim()
        ));
    }
    Ok(receipt(
        "updated",
        &installation,
        &latest,
        false,
        &command,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "zuse update {} {}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn executable(&self) -> PathBuf {
            let bin = self.0.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let executable = bin.join(format!("zuse{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&executable, b"private test executable").unwrap();
            executable
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tracking(name: &str, version: &str, source: &str) -> String {
        format!(
            r#"{{"installs":{{{}:{{"bins":["zuse"],"features":["vision"],"all_features":false,"no_default_features":true,"profile":"release","target":"x86_64-unknown-linux-gnu"}}}}}}"#,
            quote(&format!("{name} {version} ({source})"))
        )
    }

    fn registry(version: &str) -> String {
        format!(
            r#"{{"crate":{{"name":"zuse-computer","repository":"https://github.com/LIghtJUNction/zuse","max_stable_version":{0}}},"versions":[{{"num":{0},"yanked":false}}]}}"#,
            quote(version)
        )
    }

    #[test]
    fn stable_comparison_never_downgrades_a_newer_development_version() {
        for (current, latest, needed) in [
            ("0.4.0", "0.4.1", true),
            ("0.4.1", "0.4.1", false),
            ("0.10.0", "0.9.9", false),
            ("0.5.0-dev.1", "0.4.9", false),
            ("0.4.1-dev.1", "0.4.1", true),
            ("0.4.1+git.abc", "0.4.1", false),
        ] {
            assert_eq!(
                Version::stable(latest)
                    .unwrap()
                    .newer_than(&Version::parse(current).unwrap()),
                needed,
                "{current} -> {latest}"
            );
        }
    }

    #[test]
    fn malformed_and_nonstable_targets_are_rejected() {
        for version in [
            "",
            "v0.4.1",
            "0.4",
            "0.04.1",
            "0.4.1;id",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-a..b",
            "1.2.3-01",
            "1.2.3+a+b",
            "18446744073709551616.1.1",
        ] {
            assert!(Version::parse(version).is_err(), "{version}");
        }
        assert!(Version::stable("0.4.1-rc.1").is_err());
        assert!(Version::stable("0.4.1+build").is_err());
    }

    #[test]
    fn registry_requires_the_exact_package_repository_and_non_yanked_version() {
        let response = registry("0.4.1");
        assert_eq!(
            latest_version(&response, Source::Registry).unwrap(),
            "0.4.1"
        );
        for bad in [
            response.replace("zuse-computer", "zuse"),
            response.replace("LIghtJUNction/zuse", "someone/zuse"),
            response.replace("LIghtJUNction/zuse", "LIghtJUNction/zuse.git.git"),
            response.replace("false", "true"),
            registry("0.5.0-beta.1"),
        ] {
            assert!(latest_version(&bad, Source::Registry).is_err());
        }
    }

    #[test]
    fn github_requires_a_public_stable_release_from_the_official_repository() {
        let response = r#"{"tag_name":"v0.4.1","html_url":"https://github.com/LIghtJUNction/zuse/releases/tag/v0.4.1","draft":false,"prerelease":false}"#;
        assert_eq!(latest_version(response, Source::Git).unwrap(), "0.4.1");
        for bad in [
            response.replace("false", "true"),
            response.replace("LIghtJUNction", "other"),
            response.replace("v0.4.1", "v0.4.1-rc.1"),
        ] {
            assert!(latest_version(&bad, Source::Git).is_err());
        }
    }

    #[test]
    fn tracking_preserves_features_and_rejects_a_wrong_package_or_source() {
        let text = tracking(PACKAGE, "0.4.0", REGISTRY);
        let info = parse_tracking(&text, "0.4.0").unwrap();
        assert_eq!(info.features, ["vision"]);
        assert!(info.no_default_features);
        assert_eq!(info.source, Source::Registry);
        for bad in [
            tracking("other-package", "0.4.0", REGISTRY),
            tracking(PACKAGE, "0.3.0", REGISTRY),
            tracking(PACKAGE, "0.4.0", "registry+https://example.invalid/index"),
            text.replace("[\"zuse\"]", "[\"zuse\",\"helper\"]"),
            text.replace("\"release\"", "\"../../profile\""),
        ] {
            assert!(parse_tracking(&bad, "0.4.0").is_err());
        }
    }

    #[test]
    fn canonical_git_sources_allow_stable_tags_but_preserve_explicit_branch_pins() {
        let revision = "a".repeat(40);
        for location in [
            REPOSITORY.to_string(),
            "https://github.com/LIghtJUNction/zuse".into(),
            format!("{REPOSITORY}?tag=v0.4.0"),
        ] {
            assert_eq!(
                cargo_source(
                    &format!("{PACKAGE} 0.4.0 (git+{location}#{revision})"),
                    "0.4.0"
                )
                .unwrap(),
                Source::Git
            );
        }
        for location in [
            format!("{REPOSITORY}?branch=private"),
            format!("{REPOSITORY}?rev=aaaa"),
            "https://github.com/other/zuse.git".into(),
            "https://github.com/LIghtJUNction/zuse.git.evil".into(),
        ] {
            assert!(
                cargo_source(
                    &format!("{PACKAGE} 0.4.0 (git+{location}#{revision})"),
                    "0.4.0"
                )
                .is_err()
            );
        }
    }

    #[test]
    fn cargo_plan_keeps_the_actual_user_prefix_features_profile_and_target() {
        let fixture = Fixture::new();
        let executable = fixture.executable();
        std::fs::write(
            fixture.0.join(".crates2.json"),
            tracking(PACKAGE, "0.4.0", REGISTRY),
        )
        .unwrap();
        let install = inspect(&executable, "0.4.0").unwrap();
        let args = cargo_arguments(&install, "0.4.1").unwrap();
        let option = |name| args[args.iter().position(|v| v == name).unwrap() + 1].clone();
        assert_eq!(
            option("--root"),
            std::fs::canonicalize(&fixture.0).unwrap().to_str().unwrap()
        );
        assert_eq!(option("--version"), "=0.4.1");
        assert_eq!(option("--profile"), "release");
        assert_eq!(option("--target"), "x86_64-unknown-linux-gnu");
        assert_eq!(option("--features"), "vision");
        assert!(args.iter().any(|v| v == "--no-default-features"));
        assert!(!args.iter().any(|v| v == "--force"));
    }

    #[test]
    fn untracked_and_mismatched_installations_cannot_form_an_install_plan() {
        let fixture = Fixture::new();
        let executable = fixture.executable();
        let untracked = inspect(&executable, "0.4.0").unwrap();
        assert!(untracked.cargo.is_none());
        assert!(cargo_arguments(&untracked, "0.4.1").is_err());
        std::fs::write(
            fixture.0.join(".crates2.json"),
            tracking(PACKAGE, "0.4.1", REGISTRY),
        )
        .unwrap();
        assert!(inspect(&executable, "0.4.0").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn path_components_and_tracking_symlinks_do_not_cross_installation_boundaries() {
        assert!(system_prefix(Path::new("/usr/local/bin/zuse")));
        assert!(system_prefix(Path::new(
            "/opt/homebrew/Cellar/zuse/bin/zuse"
        )));
        assert!(!system_prefix(Path::new("/usr-local-user/bin/zuse")));
        let fixture = Fixture::new();
        let executable = fixture.executable();
        let other = fixture.0.join("other.json");
        std::fs::write(&other, tracking(PACKAGE, "0.4.0", REGISTRY)).unwrap();
        std::os::unix::fs::symlink(other, fixture.0.join(".crates2.json")).unwrap();
        assert!(inspect(&executable, "0.4.0").is_err());
    }

    #[test]
    fn manual_update_receipt_is_machine_readable_and_fails_the_cli_contract() {
        let fixture = Fixture::new();
        let install = inspect(&fixture.executable(), "0.4.0").unwrap();
        let result = receipt(
            "failed",
            &install,
            "0.4.1",
            true,
            &[],
            Some("manual update required"),
        );
        assert!(zuse::runtime::failed(&result));
        assert_eq!(
            json::field(&result, "error").unwrap(),
            "manual update required"
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_arguments_are_literal_and_failures_retain_the_exit_code() {
        let text = "a user path ' \\";
        let args = [
            "-c".into(),
            "printf '%s' \"$1\"".into(),
            "fixture".into(),
            text.into(),
        ];
        assert_eq!(
            process(
                Path::new("/bin/sh"),
                &args,
                Duration::from_secs(2),
                false,
                &|| false
            )
            .unwrap(),
            text
        );
        let failure = process(
            Path::new("/bin/sh"),
            &["-c".into(), "printf 'fixture failure' >&2; exit 7".into()],
            Duration::from_secs(2),
            false,
            &|| false,
        )
        .unwrap_err();
        assert!(failure.contains("exit 7"));
        assert!(failure.contains("fixture failure"));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_and_deadline_stop_owned_child_processes() {
        let args = ["-c".into(), "sleep 30".into()];
        let begin = Instant::now();
        let cancelled = process(
            Path::new("/bin/sh"),
            &args,
            Duration::from_secs(2),
            false,
            &|| begin.elapsed() > Duration::from_millis(30),
        )
        .unwrap_err();
        assert!(cancelled.contains("cancelled"));
        assert!(begin.elapsed() < Duration::from_secs(2));
        let begin = Instant::now();
        let timeout = process(
            Path::new("/bin/sh"),
            &args,
            Duration::from_millis(30),
            false,
            &|| false,
        )
        .unwrap_err();
        assert!(timeout.contains("timed out"));
        assert!(begin.elapsed() < Duration::from_secs(2));
        assert!(
            process(
                Path::new("/nonexistent/not-started"),
                &[],
                Duration::from_secs(1),
                false,
                &|| true
            )
            .unwrap_err()
            .contains("before starting")
        );
    }
}
