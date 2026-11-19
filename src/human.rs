//! Durable local human handoff; credentials never appear in tool results.
use crate::json::{self, quote};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
pub const KINDS: &[&str] = &[
    "approval",
    "password",
    "yubikey",
    "login",
    "touch",
    "permission",
];
pub struct Request<'a> {
    pub kind: &'a str,
    pub message: &'a str,
    pub url: &'a str,
    pub window: &'a str,
    pub timeout_secs: u64,
}
#[derive(Clone)]
pub struct Queue {
    directory: PathBuf,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())
}
impl Queue {
    pub fn new() -> Result<Self, String> {
        let base = std::env::var_os("ZUSE_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if let Some(p) = std::env::var_os("XDG_STATE_HOME") {
                    return PathBuf::from(p).join("zuse");
                }
                if let Some(p) = std::env::var_os("LOCALAPPDATA") {
                    return PathBuf::from(p).join("zuse");
                }
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".local/state/zuse")
            });
        Self::at(base.join("human"))
    }
    pub fn at(directory: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        Ok(Self { directory })
    }
    fn path(&self, id: &str, extension: &str) -> Result<PathBuf, String> {
        if !id.starts_with("zuse_")
            || id.len() > 96
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err("invalid human request ID".into());
        }
        Ok(self.directory.join(format!("{id}.{extension}")))
    }
    pub fn request(
        &self,
        r: Request<'_>,
        continuation: &str,
        notify: bool,
    ) -> Result<String, String> {
        if !KINDS.contains(&r.kind) {
            return Err("unknown human request kind".into());
        }
        if r.message.is_empty() || r.message.len() > 4096 {
            return Err("human message must contain 1..4096 bytes".into());
        }
        if r.timeout_secs == 0 || r.timeout_secs > 86400 {
            return Err("human timeout must be 1..86400 seconds".into());
        }
        if !r.url.is_empty() && !r.url.starts_with("https://") && !r.url.starts_with("http://") {
            return Err("login URL must use http or https".into());
        }
        if r.kind == "password" && r.window.is_empty() {
            return Err("password requests require an explicit target window ID".into());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let id = format!(
            "zuse_{:x}_{:x}_{:x}",
            stamp.as_nanos(),
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let record = format!(
            "{{\"id\":{},\"kind\":{},\"message\":{},\"url\":{},\"window\":{},\"created\":{},\"expires\":{}}}",
            quote(&id),
            quote(r.kind),
            quote(r.message),
            quote(r.url),
            quote(r.window),
            now(),
            now() + r.timeout_secs
        );
        private_write(&self.path(&id, "zu")?, continuation.as_bytes())?;
        private_write(&self.path(&id, "json")?, record.as_bytes())?;
        let delivered = notify
            && notify_user(
                "zuse needs your help",
                &format!("{}\n\nzuse human assist {id}", r.message),
            )
            .is_ok();
        Ok(format!(
            "{{\"status\":\"waiting_for_human\",\"request\":{record},\"notification\":{delivered},\"resume\":{}}}",
            quote(&format!("zuse resume {id}"))
        ))
    }
    pub fn record(&self, id: &str) -> Result<String, String> {
        fs::read_to_string(self.path(id, "json")?).map_err(|e| e.to_string())
    }
    pub fn status(&self, id: &str) -> Result<&'static str, String> {
        let record = self.record(id)?;
        if self.path(id, "claimed")?.exists() {
            return Ok("consumed");
        }
        if self.path(id, "denied")?.exists() {
            return Ok("denied");
        }
        let expiry = json::with_value(&record, |v| {
            v.get("expires")
                .ok_or("missing expiry")?
                .integer()
                .map_err(|e| e.to_string())
        })?;
        if now() >= expiry as u64 {
            return Ok("expired");
        }
        Ok(if self.path(id, "approved")?.exists() {
            "approved"
        } else {
            "pending"
        })
    }
    pub fn list(&self) -> Result<String, String> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.directory)
            .map_err(|e| e.to_string())?
            .take(1024)
        {
            let p = entry.map_err(|e| e.to_string())?.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Some(id) = p.file_stem().and_then(|x| x.to_str()) {
                records.push(format!(
                    "{{\"status\":{},\"request\":{}}}",
                    quote(self.status(id)?),
                    self.record(id)?
                ))
            }
        }
        Ok(format!("{{\"requests\":[{}]}}", records.join(",")))
    }
    pub fn respond(&self, id: &str, approve: bool) -> Result<(), String> {
        if self.status(id) != (Ok("pending")) {
            return Err("request is no longer pending".into());
        }
        private_write(
            &self.path(id, if approve { "approved" } else { "denied" })?,
            b"operator\n",
        )
    }
    pub fn continuation(&self, id: &str) -> Result<String, String> {
        let status = self.status(id)?;
        if status != "approved" {
            return Err(format!("request is {status}; waiting for human approval"));
        }
        fs::read_to_string(self.path(id, "zu")?).map_err(|e| e.to_string())
    }
    pub fn take(&self, id: &str) -> Result<String, String> {
        let source = self.continuation(id)?;
        private_write(&self.path(id, "claimed")?, b"resumed\n")?;
        fs::remove_file(self.path(id, "zu")?).map_err(|e| e.to_string())?;
        Ok(source)
    }
    /// Operator-only entry point. MCP never exposes this method.
    pub fn assist(&self, id: &str) -> Result<(), String> {
        if self.status(id)? != "pending" {
            return Err("request is no longer pending".into());
        }
        let r = self.record(id)?;
        let kind = json::field(&r, "kind")?;
        let message = json::field(&r, "message")?;
        if kind == "password" {
            let mut secret = password_dialog(&message)?;
            while matches!(secret.last(), Some(b'\n' | b'\r')) {
                secret.pop();
            }
            let result = (|| {
                let mut d = crate::platform::Desktop::connect()?;
                d.focus(&json::field(&r, "window")?)?;
                d.type_text(std::str::from_utf8(&secret).map_err(|e| e.to_string())?)
            })();
            for byte in &mut secret {
                unsafe { std::ptr::write_volatile(byte, 0) }
            }
            result?;
            return self.respond(id, true);
        }
        let url = json::field(&r, "url")?;
        if kind == "login" && !url.is_empty() {
            open_url(&url)?
        }
        eprintln!(
            "{message}\nAfter completing the action, type yes to continue (anything else denies):"
        );
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?;
        self.respond(id, answer.trim() == "yes")
    }
}
pub fn notify_user(title: &str, message: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let mut c = {
        let mut c = Command::new("notify-send");
        c.args(["--app-name=zuse", "--", title, message]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut c = {
        let mut c = Command::new("osascript");
        c.args(["-e","on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run","--",title,message]);
        c
    };
    #[cfg(target_os = "windows")]
    let mut c = {
        let mut c = Command::new("powershell");
        c.env("ZUSE_TITLE",title).env("ZUSE_MESSAGE",message).args(["-NoProfile","-NonInteractive","-Command","Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.MessageBox]::Show($env:ZUSE_MESSAGE,$env:ZUSE_TITLE) | Out-Null"]);
        c
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err("notifications unsupported".into());
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let mut child = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}
fn password_dialog(message: &str) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "linux")]
    let output = Command::new("zenity")
        .args([
            "--password",
            "--title=zuse: enter password locally",
            &format!("--text={message}"),
        ])
        .output();
    #[cfg(target_os="macos")]
 let output=Command::new("osascript").args(["-e","on run argv\ntext returned of (display dialog (item 1 of argv) default answer \"\" with hidden answer with title \"zuse\")\nend run","--",message]).output();
    #[cfg(target_os="windows")]
 let output=Command::new("powershell").env("ZUSE_MESSAGE",message).args(["-NoProfile","-Command","$s=Read-Host $env:ZUSE_MESSAGE -AsSecureString; $p=[Runtime.InteropServices.Marshal]::SecureStringToBSTR($s); try {[Console]::Write([Runtime.InteropServices.Marshal]::PtrToStringBSTR($p))} finally {[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($p)}"]).output();
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err("password prompt unsupported".into());
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let o = output.map_err(|e| format!("password prompt unavailable: {e}"))?;
        if o.status.success() {
            Ok(o.stdout)
        } else {
            Err("password prompt cancelled".into())
        }
    }
}
pub fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must use http or https".into());
    }
    #[cfg(target_os = "linux")]
    let mut c = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "macos")]
    let mut c = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut c = {
        let mut c = Command::new("powershell");
        c.env("ZUSE_URL", url)
            .args(["-NoProfile", "-Command", "Start-Process $env:ZUSE_URL"]);
        c
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err("URL opener unsupported".into());
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let mut child = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}
