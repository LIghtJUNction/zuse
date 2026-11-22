//! Bounded UTF-8 clipboard access without an external clipboard process.
//!
//! A successful write belongs to this Runtime until a different selection
//! replaces it or the Runtime is dropped. It is not durable clipboard storage.
use crate::{json::quote, observation::Cancellation};
use std::{fs::OpenOptions, io::Read, path::Path};

#[cfg(target_os = "linux")]
#[path = "clipboard/wayland.rs"]
mod wayland;

pub type Result<T> = std::result::Result<T, String>;
pub const MAX_BYTES: usize = 1_048_576;

#[derive(Default)]
pub struct Clipboard {
    #[cfg(target_os = "linux")]
    owner: Option<wayland::Owner>,
}

pub fn validate_text(text: &str) -> Result<()> {
    if text.len() > MAX_BYTES {
        return Err("clipboard text exceeds the 1 MiB limit".into());
    }
    if text.contains('\0') {
        return Err("clipboard text contains a NUL byte".into());
    }
    Ok(())
}

pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        return Err("clipboard restore path must contain 1..4096 non-NUL bytes".into());
    }
    Ok(())
}

fn restore_text(path: &str, cancellation: &Cancellation) -> Result<String> {
    validate_path(path)?;
    cancellation.check()?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Refuse special files after opening without waiting for a FIFO peer.
        options.custom_flags(0x800); // O_NONBLOCK
    }
    let mut file = options
        .open(Path::new(path))
        .map_err(|error| format!("open clipboard restore file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect clipboard restore file: {error}"))?;
    if !metadata.is_file() {
        return Err("clipboard restore requires a regular UTF-8 file".into());
    }
    if metadata.len() > MAX_BYTES as u64 {
        return Err("clipboard restore file exceeds the 1 MiB limit".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut chunk = [0u8; 8192];
    loop {
        cancellation.check()?;
        let size = file
            .read(&mut chunk)
            .map_err(|error| format!("read clipboard restore file: {error}"))?;
        if size == 0 {
            break;
        }
        if bytes.len().saturating_add(size) > MAX_BYTES {
            return Err("clipboard restore file exceeds the 1 MiB limit".into());
        }
        bytes.extend_from_slice(&chunk[..size]);
    }
    let text = String::from_utf8(bytes).map_err(|_| "clipboard restore file is not valid UTF-8")?;
    validate_text(&text)?;
    cancellation.check()?;
    Ok(text)
}

impl Clipboard {
    #[cfg(target_os = "linux")]
    fn owner(&mut self, cancellation: &Cancellation) -> Result<&mut wayland::Owner> {
        if self.owner.is_none() {
            self.owner = Some(wayland::Owner::start(cancellation)?);
        }
        Ok(self.owner.as_mut().unwrap())
    }

    /// Read the regular selection. No selection is distinct from empty text.
    pub fn read(&mut self, cancellation: &Cancellation) -> Result<String> {
        cancellation.check()?;
        #[cfg(target_os = "linux")]
        {
            let owner = self.owner(cancellation)?;
            let content = owner.read(cancellation)?;
            let backend = quote(owner.protocol());
            Ok(match content {
                Some((mime, text)) => format!(
                    r#"{{"present":true,"backend":{backend},"selection":"clipboard","mimeType":{},"bytes":{},"text":{}}}"#,
                    quote(&mime),
                    text.len(),
                    quote(&text)
                ),
                None => format!(
                    r#"{{"present":false,"backend":{backend},"selection":"clipboard","mimeType":null,"bytes":0,"text":null}}"#
                ),
            })
        }
        #[cfg(not(target_os = "linux"))]
        Err("unsupported_state: native clipboard access currently requires Wayland data-control on Linux".into())
    }

    /// Serve UTF-8 text from this process. The receipt never echoes the text.
    pub fn write(&mut self, text: &str, cancellation: &Cancellation) -> Result<String> {
        validate_text(text)?;
        cancellation.check()?;
        #[cfg(target_os = "linux")]
        {
            let owner = self.owner(cancellation)?;
            owner.write(text, cancellation)?;
            Ok(format!(
                r#"{{"ok":true,"backend":{},"selection":"clipboard","bytes":{},"owner":"runtime","delivery":"submitted","application_confirmation_required":true}}"#,
                quote(owner.protocol()),
                text.len()
            ))
        }
        #[cfg(not(target_os = "linux"))]
        Err("unsupported_state: native clipboard access currently requires Wayland data-control on Linux".into())
    }

    /// Restore only the explicit UTF-8 file; keep its source owned by Runtime.
    pub fn restore(&mut self, path: &str, cancellation: &Cancellation) -> Result<String> {
        let text = restore_text(path, cancellation)?;
        self.write(&text, cancellation)?;
        #[cfg(target_os = "linux")]
        {
            let owner = self.owner(cancellation)?;
            let actual = owner.read(cancellation)?;
            if !actual.is_some_and(|(_, actual)| actual.as_bytes() == text.as_bytes()) {
                return Err("clipboard restore was submitted but exact UTF-8 readback did not match; keep the restore file".into());
            }
            Ok(format!(
                r#"{{"ok":true,"verified":true,"backend":{},"selection":"clipboard","bytes":{},"owner":"runtime"}}"#,
                quote(owner.protocol()),
                text.len()
            ))
        }
        #[cfg(not(target_os = "linux"))]
        Err("unsupported_state: native clipboard access currently requires Wayland data-control on Linux".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn text_and_restore_are_bounded_without_echoing_private_content() {
        assert!(validate_text("").is_ok());
        assert!(validate_text("中文\n🙂").is_ok());
        assert!(validate_text(&"a".repeat(MAX_BYTES)).is_ok());
        assert!(
            validate_text(&"a".repeat(MAX_BYTES + 1))
                .unwrap_err()
                .contains("1 MiB")
        );
        assert!(validate_text("private\0text").unwrap_err().contains("NUL"));
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "zuse-clipboard-restore-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, "中文\n🙂").unwrap();
        assert_eq!(
            restore_text(path.to_str().unwrap(), &Cancellation::default()).unwrap(),
            "中文\n🙂"
        );
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(
            restore_text(path.to_str().unwrap(), &Cancellation::default())
                .unwrap_err()
                .contains("UTF-8")
        );
        let token = Cancellation::default();
        token.cancel();
        assert!(
            restore_text(path.to_str().unwrap(), &token)
                .unwrap_err()
                .starts_with("cancelled:")
        );
        std::fs::remove_file(path).unwrap();
    }
}
