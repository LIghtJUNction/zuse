//! Exact, bounded access to an existing native keyboard. No grabs, keymaps,
//! device creation, permission changes or events for another owner's keys.
use super::Result;
use std::{
    ffi::{c_int, c_long, c_ulong},
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

const MAX_DEVICES: usize = 128;
const EVENT_SIZE: usize = 2 * std::mem::size_of::<c_long>() + 8;
const O_NONBLOCK: i32 = 0x800;
const O_NOFOLLOW: i32 = 0x20000;

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

// Hyprland v0.57.2 MiscFunctions::deviceNameToInternalString and
// InputManager::getNameForNewDevice. Native names reach this through
// AQ CLibinputKeyboard -> libinput_device_get_name -> EVIOCGNAME.
// ASCII is the deliberately restricted, locale-independent supported subset.
fn normalized(name: &str) -> Result<String> {
    if name.is_empty() || name.len() > 255 || !name.is_ascii() || name.contains('\0') {
        return Err(
            "unsupported_state: native keyboard identity requires a nonempty ASCII device name"
                .into(),
        );
    }
    Ok(name
        .bytes()
        .map(|byte| match byte {
            b' ' | b'\n' | b',' => '-',
            _ => byte.to_ascii_lowercase() as char,
        })
        .collect())
}

fn numbered_variant(name: &str, base: &str) -> bool {
    name.strip_prefix(base)
        .and_then(|suffix| suffix.strip_prefix('-'))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn collides(name: &str, target: &str) -> bool {
    name == target || numbered_variant(name, target) || numbered_variant(target, name)
}

pub(super) fn check_compositor_names(target: &str, names: &[String]) -> Result<()> {
    let numbered = target.rsplit_once('-').is_some_and(|(_, suffix)| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    });
    if normalized(target)? != target
        || numbered
        || target.starts_with("hl-virtual-")
        || matches!(target, "unknown-device" | "wl_keyboard" | "wl_pointer")
    {
        return Err(
            "unsupported_state: virtual, unknown or numbered keyboard identity is ambiguous".into(),
        );
    }
    if names.len() > MAX_DEVICES
        || names.iter().filter(|name| collides(name, target)).count() != 1
        || names.iter().filter(|name| name.as_str() == target).count() != 1
    {
        return Err(
            "unsupported_state: keyboard name is not unique across compositor devices".into(),
        );
    }
    Ok(())
}

fn unique_index(target: &str, compositor: &[String], kernel: &[String]) -> Result<usize> {
    check_compositor_names(target, compositor)?;
    if kernel.len() > MAX_DEVICES {
        return Err("unsupported_state: too many native input devices to inspect".into());
    }
    let mut selected = None;
    for (index, name) in kernel.iter().enumerate() {
        let name = normalized(name)?;
        if collides(&name, target) {
            if name != target || selected.is_some() {
                return Err("unsupported_state: native keyboard identity has a duplicate or numbered collision".into());
            }
            selected = Some(index);
        }
    }
    selected.ok_or_else(|| {
        "unsupported_state: no unique native keyboard matches the compositor identity".into()
    })
}

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    rdev: u64,
    name: String,
}

fn device_name(file: &File) -> Result<String> {
    let mut bytes = [0u8; 256];
    let request = 0x8000_4506u32 | ((bytes.len() as u32) << 16); // EVIOCGNAME
    let size = unsafe { ioctl(file.as_raw_fd(), request as c_ulong, bytes.as_mut_ptr()) };
    if size <= 0 || size as usize > bytes.len() {
        return Err(format!(
            "inspect native keyboard name: {}",
            std::io::Error::last_os_error()
        ));
    }
    let end = bytes[..size as usize]
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("unsupported_state: native keyboard name was truncated")?;
    let name = std::str::from_utf8(&bytes[..end])
        .map_err(|_| "unsupported_state: native keyboard name is not UTF-8")?;
    normalized(name)?;
    Ok(name.into())
}

fn identity(file: &File) -> Result<Identity> {
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.file_type().is_char_device() {
        return Err("unsupported_state: native keyboard is not a character device".into());
    }
    Ok(Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
        rdev: metadata.rdev(),
        name: device_name(file)?,
    })
}

fn open(path: &Path, write: bool) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(O_NONBLOCK | O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("open native keyboard {}: {error}", path.display()))
}

fn bits(file: &File, operation: u32) -> Result<[u8; 96]> {
    let mut bits = [0; 96];
    let request = operation | ((bits.len() as u32) << 16);
    if unsafe { ioctl(file.as_raw_fd(), request as c_ulong, bits.as_mut_ptr()) } < 0 {
        return Err(format!(
            "inspect native keyboard keys: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(bits)
}

fn contains(bits: &[u8; 96], code: u16) -> bool {
    code < 768 && bits[usize::from(code / 8)] & (1u8 << (code % 8)) != 0
}

fn events(code: u16, down: bool) -> Vec<u8> {
    let mut bytes = vec![0; EVENT_SIZE * 2];
    let at = 2 * std::mem::size_of::<c_long>();
    bytes[at..at + 2].copy_from_slice(&1u16.to_ne_bytes());
    bytes[at + 2..at + 4].copy_from_slice(&code.to_ne_bytes());
    bytes[at + 4..at + 8].copy_from_slice(&i32::from(down).to_ne_bytes());
    bytes // EV_KEY followed by the zero-valued EV_SYN/SYN_REPORT.
}

struct OwnedKey<W: Write> {
    writer: W,
    code: u16,
    pressed: bool,
}

impl<W: Write> OwnedKey<W> {
    fn submit(&mut self, down: bool) -> Result<()> {
        let bytes = events(self.code, down);
        match self.writer.write(&bytes) {
            Ok(count) if count == bytes.len() => Ok(()),
            Ok(count) => Err(format!(
                "native key input was incomplete ({count}/{} bytes); input was not replayed",
                bytes.len()
            )),
            Err(error) => Err(format!(
                "native key input failed: {error}; input was not replayed"
            )),
        }
    }

    fn press(&mut self, already_held: bool) -> Result<()> {
        if already_held {
            return Err("unsupported_state: requested key is already held; it was not pressed or released by this call".into());
        }
        // A partial write can have delivered keydown. Arm only after the
        // immediate EVIOCGKEY ownership check, but before that first write.
        self.pressed = true;
        self.submit(true)
    }

    fn release(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.pressed) {
            return Ok(());
        }
        self.submit(false)
    }
}

impl<W: Write> Drop for OwnedKey<W> {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub(super) struct Keyboard {
    key: OwnedKey<File>,
    path: PathBuf,
    identity: Identity,
}

impl Keyboard {
    pub(super) fn open_unique(target: &str, compositor: &[String], xkb_code: u32) -> Result<Self> {
        check_compositor_names(target, compositor)?;
        let code = u16::try_from(
            xkb_code
                .checked_sub(8)
                .filter(|code| *code != 0 && *code < 768)
                .ok_or("unsupported_state: invalid native key code")?,
        )
        .map_err(|_| "invalid native key code")?;
        let mut candidates = Vec::new();
        for entry in fs::read_dir("/dev/input")
            .map_err(|error| format!("inspect native input devices: {error}"))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name();
            let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix("event")) else {
                continue;
            };
            if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            if candidates.len() >= MAX_DEVICES {
                return Err("unsupported_state: too many native input devices to inspect".into());
            }
            let path = entry.path();
            let file = open(&path, false)?;
            candidates.push((path, identity(&file)?));
        }
        let names = candidates
            .iter()
            .map(|(_, identity)| identity.name.clone())
            .collect::<Vec<_>>();
        let index = unique_index(target, compositor, &names)?;
        let (path, observed) = candidates.swap_remove(index);
        let file = open(&path, true)?;
        if identity(&file)? != observed {
            return Err(
                "stale_target: native keyboard changed while opening its input handle".into(),
            );
        }
        if !contains(&bits(&file, 0x8000_4521)?, code) {
            // EVIOCGBIT(EV_KEY)
            return Err(
                "unsupported_state: current native keyboard does not support the requested key"
                    .into(),
            );
        }
        Ok(Self {
            key: OwnedKey {
                writer: file,
                code,
                pressed: false,
            },
            path,
            identity: observed,
        })
    }

    pub(super) fn verify(&self) -> Result<()> {
        let path = fs::symlink_metadata(&self.path)
            .map_err(|error| format!("stale_target: native keyboard path changed: {error}"))?;
        if !path.file_type().is_char_device()
            || path.dev() != self.identity.device
            || path.ino() != self.identity.inode
            || path.rdev() != self.identity.rdev
            || identity(&self.key.writer)? != self.identity
        {
            return Err("stale_target: native keyboard identity changed".into());
        }
        Ok(())
    }

    pub(super) fn held_codes(&self) -> Result<Vec<u32>> {
        let held = bits(&self.key.writer, 0x8000_4518)?; // EVIOCGKEY
        Ok((0..768u16)
            .filter(|code| contains(&held, *code))
            .map(|code| u32::from(code) + 8)
            .collect())
    }

    pub(super) fn pressed_by_us(&self) -> bool {
        self.key.pressed
    }

    pub(super) fn press(&mut self) -> Result<()> {
        self.verify()?;
        let held = contains(&bits(&self.key.writer, 0x8000_4518)?, self.key.code);
        self.key.press(held)
    }

    pub(super) fn release(&mut self) -> Result<()> {
        self.key.release()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn native_identity_uses_exact_normalization_and_rejects_both_sides_collisions() {
        assert_eq!(normalized(" AT,Keyboard\n").unwrap(), "-at-keyboard-");
        let compositor = names(&[
            "at-translated-set-2-keyboard",
            "video-bus",
            "video-bus-1",
            "hl-virtual-keyboard-fcitx5",
        ]);
        let kernel = names(&["Video Bus", "AT Translated Set 2 keyboard", "Video Bus"]);
        assert_eq!(
            unique_index("at-translated-set-2-keyboard", &compositor, &kernel).unwrap(),
            1
        );
        assert!(unique_index("video-bus", &compositor, &kernel).is_err());
        assert!(unique_index("a-b", &names(&["a-b"]), &names(&["A B", "a,b"])).is_err());
        assert!(unique_index("board", &names(&["board", "board-1"]), &names(&["board"])).is_err());
        assert!(unique_index("board", &names(&["board"]), &names(&["board", "board-1"])).is_err());
        for name in [
            "board-1",
            "hl-virtual-keyboard",
            "hl-virtual-keyboard-fcitx5",
            "wl_keyboard",
            "unknown-device",
        ] {
            assert!(
                unique_index(name, &names(&[name]), &names(&[name])).is_err(),
                "{name}"
            );
        }
        assert!(normalized("键盘").is_err());
        assert!(unique_index("board", &names(&["board"]), &names(&["other board"])).is_err());
    }

    fn decoded(bytes: &[u8]) -> Vec<(u16, u16, i32)> {
        bytes
            .as_chunks::<EVENT_SIZE>()
            .0
            .iter()
            .map(|event| {
                let at = EVENT_SIZE - 8;
                (
                    u16::from_ne_bytes(event[at..at + 2].try_into().unwrap()),
                    u16::from_ne_bytes(event[at + 2..at + 4].try_into().unwrap()),
                    i32::from_ne_bytes(event[at + 4..at + 8].try_into().unwrap()),
                )
            })
            .collect()
    }

    #[test]
    fn native_key_writer_never_releases_a_rejected_foreign_target() {
        let mut key = OwnedKey {
            writer: Vec::new(),
            code: 105,
            pressed: false,
        };
        assert!(key.press(true).unwrap_err().contains("already held"));
        key.release().unwrap();
        assert!(key.writer.is_empty());
        key.press(false).unwrap();
        key.release().unwrap();
        key.release().unwrap();
        assert_eq!(
            decoded(&key.writer),
            [(1, 105, 1), (0, 0, 0), (1, 105, 0), (0, 0, 0)]
        );
    }

    #[test]
    fn native_partial_press_only_releases_its_key_without_replaying() {
        #[derive(Default)]
        struct ShortWrite(Vec<Vec<u8>>);
        impl Write for ShortWrite {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.push(bytes.to_vec());
                Ok(if self.0.len() == 1 {
                    EVENT_SIZE
                } else {
                    bytes.len()
                })
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut key = OwnedKey {
            writer: ShortWrite::default(),
            code: 105,
            pressed: false,
        };
        assert!(key.press(false).is_err());
        key.release().unwrap();
        assert_eq!(key.writer.0.len(), 2);
        assert_eq!(decoded(&key.writer.0[1]), [(1, 105, 0), (0, 0, 0)]);
    }
}
