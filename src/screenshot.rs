//! Shared mobile screenshot framing and staged output.
//!
//! This checks PNG chunk boundaries and required structure, not pixel decoding
//! or CRCs. Complete transport output must be validated before replacing a file.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

type Result<T> = std::result::Result<T, String>;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn validate_output_path(path: &Path) -> Result<()> {
    if path.file_name().is_none()
        || path.as_os_str().is_empty()
        || path.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err("screenshot needs an output filename".into());
    }
    Ok(())
}

pub(crate) fn png_dimensions(bytes: &[u8], max_dimension: u32) -> Result<(u32, u32)> {
    if bytes.len() < 45
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes[8..12] != 13u32.to_be_bytes()
        || &bytes[12..16] != b"IHDR"
    {
        return Err("screenshot is not a complete PNG".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 || width > max_dimension || height > max_dimension {
        return Err("PNG dimensions exceed limits".into());
    }
    let mut offset = 8;
    let mut image_data = false;
    while offset + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(length)
            .and_then(|v| v.checked_add(12))
            .filter(|&end| end <= bytes.len())
            .ok_or("PNG chunk is truncated")?;
        let kind = &bytes[offset + 4..offset + 8];
        if kind == b"IHDR" && offset != 8 {
            return Err("PNG contains a duplicate image header".into());
        }
        if kind == b"IDAT" && length > 0 {
            image_data = true;
        }
        if kind == b"IEND" {
            if length != 0 || end != bytes.len() || !image_data {
                return Err("PNG terminator is invalid".into());
            }
            return Ok((width, height));
        }
        offset = end;
    }
    Err("PNG has no complete image terminator".into())
}

/// Keep an existing destination intact until writing and the final deadline
/// check succeed. Staging stays on the destination filesystem for one rename.
pub(crate) fn write_image(
    path: &Path,
    bytes: &[u8],
    before_replace: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_output_path(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(
        ".zuse-screenshot-{}-{}.png",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|e| format!("screenshot output: {e}"))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|e| format!("screenshot write: {e}"))?;
        file.flush().map_err(|e| format!("screenshot flush: {e}"))?;
        before_replace()?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| format!("screenshot replace: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_failure_preserves_previous_capture_and_removes_staging() {
        let directory = std::env::temp_dir().join(format!(
            "zuse-screenshot-deadline-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("screen.png");
        fs::write(&path, b"previous screenshot").unwrap();
        let result = write_image(&path, b"new screenshot", || {
            Err("operation deadline exceeded".into())
        });
        let previous = fs::read(&path).unwrap();
        let files = fs::read_dir(&directory).unwrap().count();
        fs::remove_dir_all(directory).unwrap();
        assert_eq!(result.unwrap_err(), "operation deadline exceeded");
        assert_eq!(previous, b"previous screenshot");
        assert_eq!(files, 1);
    }
}
