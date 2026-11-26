//! Linux uinput v5, single-finger type-B frames. Closing the fd removes this device.
use crate::{drawing::Input, motion::Point};
use std::{
    ffi::{c_int, c_long, c_ulong},
    fs::{File, OpenOptions},
    io::{self, Write},
    os::fd::AsRawFd,
};

const SYN: u16 = 0;
const KEY: u16 = 1;
const ABS: u16 = 3;
const X: u16 = 0;
const Y: u16 = 1;
const SLOT: u16 = 0x2f;
const MT_X: u16 = 0x35;
const MT_Y: u16 = 0x36;
const TRACK: u16 = 0x39;
const LEFT: u16 = 0x110;
const FINGER: u16 = 0x145;
const TOUCH: u16 = 0x14a;
const DOUBLE_TAP: u16 = 0x14d;

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

// asm-generic/ioctl.h; Linux's MIPS, PowerPC and SPARC variants use different direction bits.
const ALTERNATE_IOC: bool = cfg!(any(
    target_arch = "mips",
    target_arch = "mips64",
    target_arch = "mips32r6",
    target_arch = "mips64r6",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "sparc",
    target_arch = "sparc64"
));
fn request(number: u8, size: usize) -> c_ulong {
    let dir = match (size == 0, ALTERNATE_IOC) {
        (true, false) => 0,
        (true, true) => 1 << 29,
        (false, false) => 1 << 30,
        (false, true) => 4 << 29,
    };
    dir | ((size as c_ulong) << 16) | (c_ulong::from(b'U') << 8) | c_ulong::from(number)
}

fn checked(ret: c_int, what: &str) -> Result<(), String> {
    if ret < 0 {
        Err(format!("uinput {what}: {}", io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn set_bit(fd: &File, number: u8, bit: u16) -> Result<(), String> {
    // SAFETY: UI_SET_{EV,KEY,PROP}BIT take an integer bit index, not a pointer.
    checked(
        unsafe { ioctl(fd.as_raw_fd(), request(number, 4), c_ulong::from(bit)) },
        "enable input capability",
    )
}

fn setup(fd: &File, number: u8, bytes: &[u8]) -> Result<(), String> {
    // SAFETY: initialized buffers are encoded to the exact kernel UAPI lengths below.
    checked(
        unsafe { ioctl(fd.as_raw_fd(), request(number, bytes.len()), bytes.as_ptr()) },
        "configure device",
    )
}

fn axis(fd: &File, code: u16, max: i32, resolution: i32) -> Result<(), String> {
    // struct uinput_abs_setup: u16 code, u16 padding, six native i32 absinfo fields.
    let mut bytes = [0u8; 28];
    bytes[..2].copy_from_slice(&code.to_ne_bytes());
    bytes[12..16].copy_from_slice(&max.to_ne_bytes());
    bytes[24..28].copy_from_slice(&resolution.to_ne_bytes());
    setup(fd, 4, &bytes)
}

pub(super) struct Surface {
    fd: File,
    size: (u16, u16),
    contact: Contact,
}

impl Surface {
    pub(super) fn open(width: u16, height: u16, direct: bool) -> Result<Self, String> {
        let fd = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|error| format!("open /dev/uinput for virtual handwriting: {error}"))?;
        set_bit(&fd, 100, KEY)?;
        set_bit(&fd, 100, ABS)?;
        set_bit(&fd, 110, u16::from(direct))?; // INPUT_PROP_POINTER or INPUT_PROP_DIRECT
        set_bit(&fd, 101, TOUCH)?;
        if !direct {
            for key in [FINGER, DOUBLE_TAP, LEFT] {
                set_bit(&fd, 101, key)?;
            }
            set_bit(&fd, 110, 2)?; // INPUT_PROP_BUTTONPAD
        }
        // Normalized 100 x 60 mm surface. Logical dimensions only affect coordinate mapping.
        for (code, max, resolution) in [
            (X, 10000, 100),
            (Y, 6000, 100),
            (MT_X, 10000, 100),
            (MT_Y, 6000, 100),
            (SLOT, 1, 0),
            (TRACK, 65535, 0),
        ] {
            axis(&fd, code, max, resolution)?;
        }
        // struct uinput_setup: input_id (4 × u16), char name[80], u32 ff_effects_max.
        let mut device = [0u8; 92];
        device[..2].copy_from_slice(&6u16.to_ne_bytes()); // BUS_VIRTUAL
        device[6..8].copy_from_slice(&1u16.to_ne_bytes());
        let name: &[u8] = if direct {
            b"Zuse Handwriting Touchscreen"
        } else {
            b"Zuse Handwriting Touchpad"
        };
        device[8..8 + name.len()].copy_from_slice(name);
        setup(&fd, 3, &device)?;
        // SAFETY: UI_DEV_CREATE has no data argument. File ownership tears down the device.
        checked(
            unsafe { ioctl(fd.as_raw_fd(), request(1, 0), 0 as c_ulong) },
            "create virtual surface",
        )?;
        Ok(Self {
            fd,
            size: (width, height),
            contact: Contact {
                finger_key: !direct,
                ..Contact::default()
            },
        })
    }
}

// The kernel ignores the timestamp for uinput. No native padding is read from Rust objects.
const EVENT_SIZE: usize = 2 * std::mem::size_of::<c_long>() + 8;
fn emit(frame: &mut Vec<u8>, kind: u16, code: u16, value: i32) {
    frame.resize(frame.len() + EVENT_SIZE - 8, 0);
    frame.extend(kind.to_ne_bytes());
    frame.extend(code.to_ne_bytes());
    frame.extend(value.to_ne_bytes());
}

#[derive(Default)]
struct Contact {
    finger_key: bool,
    held: bool,
    id: u16,
    position: (i32, i32),
}

impl Contact {
    fn frame(&self, phase: Phase) -> Vec<u8> {
        let mut events = Vec::with_capacity(9 * EVENT_SIZE);
        if !matches!(phase, Phase::Move) {
            let down = i32::from(matches!(phase, Phase::Down));
            // BTN_TOUCH leads the frame for compatibility with legacy single-touch consumers.
            emit(&mut events, KEY, TOUCH, down);
            if self.finger_key {
                emit(&mut events, KEY, FINGER, down);
            }
            emit(&mut events, ABS, SLOT, 0);
            emit(
                &mut events,
                ABS,
                TRACK,
                if down == 1 { i32::from(self.id) } else { -1 },
            );
        }
        if !matches!(phase, Phase::Up) {
            for (code, value) in [
                (MT_X, self.position.0),
                (MT_Y, self.position.1),
                (X, self.position.0),
                (Y, self.position.1),
            ] {
                emit(&mut events, ABS, code, value);
            }
        }
        emit(&mut events, SYN, 0, 0);
        events
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Down,
    Move,
    Up,
}

impl Input for Surface {
    fn holds(&self, _: &str) -> bool {
        self.contact.held
    }

    fn move_to(&mut self, point: Point) -> Result<(), String> {
        self.contact.position = (
            (point.x * 10000. / f64::from(self.size.0 - 1)).round() as i32,
            (point.y * 6000. / f64::from(self.size.1 - 1)).round() as i32,
        );
        if self.contact.held {
            self.fd
                .write_all(&self.contact.frame(Phase::Move))
                .map_err(|e| format!("uinput move: {e}"))?;
        }
        Ok(())
    }

    fn button(&mut self, _: &str, action: &str) -> Result<(), String> {
        let phase = match action {
            "down" => {
                self.contact.id = self.contact.id.wrapping_add(1);
                self.contact.held = true;
                Phase::Down
            }
            "up" => Phase::Up,
            _ => return Err("invalid handwriting contact phase".into()),
        };
        self.fd
            .write_all(&self.contact.frame(phase))
            .map_err(|e| format!("uinput contact {action}: {e}"))?;
        if matches!(phase, Phase::Up) {
            self.contact.held = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fields(bytes: Vec<u8>) -> Vec<(u16, u16, i32)> {
        bytes
            .as_chunks::<EVENT_SIZE>()
            .0
            .iter()
            .map(|event| {
                let f = &event[EVENT_SIZE - 8..];
                (
                    u16::from_ne_bytes(f[0..2].try_into().unwrap()),
                    u16::from_ne_bytes(f[2..4].try_into().unwrap()),
                    i32::from_ne_bytes(f[4..8].try_into().unwrap()),
                )
            })
            .collect()
    }
    #[test]
    fn type_b_frames_have_single_contact_lifetime_and_distinct_touchpad_flags() {
        for touchpad in [false, true] {
            let contact = Contact {
                finger_key: touchpad,
                held: true,
                id: 42,
                position: (300, 900),
            };
            let down = fields(contact.frame(Phase::Down));
            assert_eq!(down[0], (KEY, TOUCH, 1));
            assert_eq!(down.contains(&(KEY, FINGER, 1)), touchpad);
            assert!(down.contains(&(ABS, TRACK, 42)));
            let moving = fields(contact.frame(Phase::Move));
            assert_eq!(
                moving,
                [
                    (ABS, MT_X, 300),
                    (ABS, MT_Y, 900),
                    (ABS, X, 300),
                    (ABS, Y, 900),
                    (SYN, 0, 0)
                ]
            );
            let up = fields(contact.frame(Phase::Up));
            assert!(up.contains(&(ABS, TRACK, -1)));
            assert_eq!(up.first(), Some(&(KEY, TOUCH, 0)));
            assert_eq!(up.last(), Some(&(SYN, 0, 0)));
        }
    }
}
