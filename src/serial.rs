//! Native bounded serial I/O. Linux/macOS use termios; Windows uses Win32 COM.
//!
//! Ports are explicit. Opening configures 8N1 without flow control and closing
//! restores the captured configuration. No shell or serial helper is spawned.
use crate::{
    device_io::{self, ReadResult},
    json::quote,
};

type Result<T> = std::result::Result<T, String>;

pub struct SerialPort {
    port: Option<native::Port>,
    path: String,
    baud: u32,
}

impl SerialPort {
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        validate_path(path)?;
        validate_baud(baud)?;
        let port = native::Port::open(path, baud)?;
        Ok(Self {
            port: Some(port),
            path: path.into(),
            baud,
        })
    }

    pub fn status(&self) -> String {
        format!(
            "{{\"connected\":{},\"path\":{},\"baud\":{},\"format\":\"8N1\",\"flow_control\":\"none\"}}",
            self.port.is_some(),
            quote(&self.path),
            self.baud
        )
    }

    pub fn read(&mut self, length: usize, timeout_ms: u32) -> Result<ReadResult> {
        device_io::validate_length(length)?;
        device_io::validate_timeout(timeout_ms)?;
        self.port
            .as_mut()
            .ok_or("serial port is not open")?
            .read(length, timeout_ms)
    }

    /// Write the complete buffer or report the number written before failure.
    pub fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize> {
        device_io::validate_timeout(timeout_ms)?;
        if bytes.len() > device_io::MAX_BYTES {
            return Err("serial write exceeds 65536 bytes".into());
        }
        self.port
            .as_mut()
            .ok_or("serial port is not open")?
            .write(bytes, timeout_ms)
    }

    pub fn close(&mut self) -> Result<()> {
        if let Some(port) = self.port.take() {
            port.close()?;
        }
        Ok(())
    }
}

pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 4096 || path.bytes().any(|b| b == 0 || b < 32) {
        return Err("serial path must be a nonempty device path without control characters".into());
    }
    #[cfg(windows)]
    {
        let name = path.strip_prefix(r"\\.\").unwrap_or(path);
        let number = name.strip_prefix("COM").and_then(|n| n.parse::<u16>().ok());
        if !matches!(number, Some(1..=4096)) {
            return Err("Windows serial path must be COM1 through COM4096".into());
        }
    }
    #[cfg(unix)]
    if !path.starts_with("/dev/") {
        return Err("serial path must explicitly select a device below /dev".into());
    }
    Ok(())
}

pub fn validate_baud(baud: u32) -> Result<()> {
    if ![
        50, 75, 110, 134, 150, 200, 300, 600, 1200, 1800, 2400, 4800, 9600, 19200, 38400, 57600,
        115200, 230400,
    ]
    .contains(&baud)
    {
        return Err("unsupported serial baud; use a standard rate from 50 through 230400".into());
    }
    Ok(())
}

/// Enumerate device names without opening or reconfiguring them.
pub fn ports() -> Result<String> {
    let mut names = native::ports()?;
    names.sort();
    names.dedup();
    Ok(format!(
        "[{}]",
        names.iter().map(|n| quote(n)).collect::<Vec<_>>().join(",")
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use super::*;
    use std::{
        ffi::{c_int, c_short},
        fs::{File, OpenOptions},
        io::{self, Read, Write},
        os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
        time::{Duration, Instant},
    };

    #[cfg(target_os = "linux")]
    type Flag = u32;
    #[cfg(target_os = "macos")]
    type Flag = u64;
    type Speed = Flag;
    #[cfg(target_os = "linux")]
    type Nfds = usize;
    #[cfg(target_os = "macos")]
    type Nfds = u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Termios {
        input: Flag,
        output: Flag,
        control: Flag,
        local: Flag,
        #[cfg(target_os = "linux")]
        line: u8,
        #[cfg(target_os = "linux")]
        characters: [u8; 32],
        #[cfg(target_os = "macos")]
        characters: [u8; 20],
        input_speed: Speed,
        output_speed: Speed,
    }

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: c_short,
        revents: c_short,
    }

    unsafe extern "C" {
        fn tcgetattr(fd: c_int, attr: *mut Termios) -> c_int;
        fn tcsetattr(fd: c_int, action: c_int, attr: *const Termios) -> c_int;
        fn cfmakeraw(attr: *mut Termios);
        fn cfsetspeed(attr: *mut Termios, speed: Speed) -> c_int;
        fn poll(fds: *mut PollFd, count: Nfds, timeout: c_int) -> c_int;
    }

    pub struct Port {
        file: File,
        original: Option<Termios>,
    }

    impl Port {
        pub fn open(path: &str, baud: u32) -> Result<Self> {
            #[cfg(target_os = "linux")]
            let flags = 0x100 | 0x800; // O_NOCTTY | O_NONBLOCK.
            #[cfg(target_os = "macos")]
            let flags = 0x20000 | 0x4;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(flags)
                .open(path)
                .map_err(|e| format!("open serial device: {e}"))?;
            let mut original = std::mem::MaybeUninit::<Termios>::uninit();
            // SAFETY: valid fd and correctly sized platform termios storage.
            if unsafe { tcgetattr(file.as_raw_fd(), original.as_mut_ptr()) } != 0 {
                return Err(format!(
                    "device is not a configurable serial/TTY port: {}",
                    io::Error::last_os_error()
                ));
            }
            let original = unsafe { original.assume_init() };
            let mut configured = original;
            unsafe { cfmakeraw(&mut configured) };
            #[cfg(target_os = "linux")]
            {
                configured.control &= !(0x8000_0000 | 0x40); // hardware flow, two stop bits.
                configured.control |= 0x800 | 0x80; // CLOCAL | CREAD.
                configured.input &= !(0x400 | 0x800 | 0x1000); // IXON | IXANY | IXOFF.
            }
            #[cfg(target_os = "macos")]
            {
                configured.control &= !(0x001f_0000 | 0x400);
                configured.control |= 0x8000 | 0x800; // CLOCAL | CREAD.
                configured.input &= !(0x200 | 0x400 | 0x800);
            }
            // GNU cfsetspeed accepts numeric rates across the pre/post-2.42
            // termios symbol ABI; BSD uses numeric rates as well. A libc that
            // requires legacy encoded B constants rejects these standard
            // numeric rates, so retry that representation without device I/O.
            let without_speed = configured;
            #[cfg(target_os = "linux")]
            let numeric_baud = baud;
            #[cfg(target_os = "macos")]
            let numeric_baud = u64::from(baud);
            if unsafe { cfsetspeed(&mut configured, numeric_baud) } != 0 {
                configured = without_speed;
                if unsafe { cfsetspeed(&mut configured, speed(baud)) } != 0 {
                    return Err(format!(
                        "configure serial baud: {}",
                        io::Error::last_os_error()
                    ));
                }
            }
            let port = Self {
                file,
                original: Some(original),
            };
            if unsafe { tcsetattr(port.file.as_raw_fd(), 0, &configured) } != 0 {
                return Err(format!(
                    "configure serial 8N1: {}",
                    io::Error::last_os_error()
                ));
            }
            Ok(port)
        }

        fn wait(&self, events: c_short, deadline: Instant) -> Result<bool> {
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(false);
                }
                let mut descriptor = PollFd {
                    fd: self.file.as_raw_fd(),
                    events,
                    revents: 0,
                };
                let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as c_int;
                let ready = unsafe { poll(&mut descriptor, 1, millis) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(format!("poll serial device: {error}"));
                }
                if ready == 0 {
                    return Ok(false);
                }
                // Buffered input may coexist with HUP; consume it first.
                if descriptor.revents & events != 0 {
                    return Ok(true);
                }
                if descriptor.revents & (0x8 | 0x10 | 0x20) != 0 {
                    return Err("serial device disconnected or unavailable".into());
                }
            }
        }

        pub fn read(&mut self, length: usize, timeout_ms: u32) -> Result<ReadResult> {
            let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
            let mut bytes = vec![0; length];
            loop {
                if !self.wait(1, deadline)? {
                    return Ok(ReadResult {
                        bytes: Vec::new(),
                        timed_out: true,
                    });
                }
                match self.file.read(&mut bytes) {
                    Ok(0) => return Err("serial device reached end of stream".into()),
                    Ok(count) => {
                        bytes.truncate(count);
                        return Ok(ReadResult {
                            bytes,
                            timed_out: false,
                        });
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(format!("read serial device: {e}")),
                }
            }
        }

        pub fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize> {
            let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
            let mut count = 0;
            while count < bytes.len() {
                if !self
                    .wait(4, deadline)
                    .map_err(|e| format!("{e}; serial bytes_written={count}"))?
                {
                    return Err(format!("serial write timed out; bytes_written={count}"));
                }
                match self.file.write(&bytes[count..]) {
                    Ok(0) => {
                        return Err(format!(
                            "serial write made no progress; bytes_written={count}"
                        ));
                    }
                    Ok(written) => count += written,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => {
                        return Err(format!("write serial device: {e}; bytes_written={count}"));
                    }
                }
            }
            Ok(count)
        }

        fn restore(&mut self) -> Result<()> {
            if let Some(original) = self.original {
                if unsafe { tcsetattr(self.file.as_raw_fd(), 0, &original) } != 0 {
                    return Err(format!(
                        "restore serial configuration: {}",
                        io::Error::last_os_error()
                    ));
                }
                self.original = None;
            }
            Ok(())
        }
        pub fn close(mut self) -> Result<()> {
            self.restore()
        }
    }

    impl Drop for Port {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }

    #[cfg(target_os = "linux")]
    fn speed(baud: u32) -> Speed {
        match baud {
            50 => 1,
            75 => 2,
            110 => 3,
            134 => 4,
            150 => 5,
            200 => 6,
            300 => 7,
            600 => 8,
            1200 => 9,
            1800 => 10,
            2400 => 11,
            4800 => 12,
            9600 => 13,
            19200 => 14,
            38400 => 15,
            57600 => 0x1001,
            115200 => 0x1002,
            230400 => 0x1003,
            _ => unreachable!(),
        }
    }
    #[cfg(target_os = "macos")]
    fn speed(baud: u32) -> Speed {
        baud.into()
    }

    pub fn ports() -> Result<Vec<String>> {
        let mut output = Vec::new();
        for entry in std::fs::read_dir("/dev").map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            #[cfg(target_os = "linux")]
            let selected = ["ttyUSB", "ttyACM", "ttyS", "ttyAMA", "ttyTHS"]
                .iter()
                .any(|prefix| {
                    name.strip_prefix(prefix)
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                });
            #[cfg(target_os = "macos")]
            let selected = name.starts_with("cu.") || name.starts_with("tty.");
            if selected {
                output.push(entry.path().to_string_lossy().into_owned());
            }
        }
        Ok(output)
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        ffi::c_void,
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
        time::{Duration, Instant},
    };

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Dcb {
        length: u32,
        baud: u32,
        flags: u32,
        reserved: u16,
        xon_limit: u16,
        xoff_limit: u16,
        byte_size: u8,
        parity: u8,
        stop_bits: u8,
        xon_char: i8,
        xoff_char: i8,
        error_char: i8,
        eof_char: i8,
        event_char: i8,
        reserved1: u16,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Timeouts {
        interval: u32,
        read_multiplier: u32,
        read_constant: u32,
        write_multiplier: u32,
        write_constant: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCommState(handle: *mut c_void, config: *mut Dcb) -> i32;
        fn SetCommState(handle: *mut c_void, config: *const Dcb) -> i32;
        fn GetCommTimeouts(handle: *mut c_void, timeouts: *mut Timeouts) -> i32;
        fn SetCommTimeouts(handle: *mut c_void, timeouts: *const Timeouts) -> i32;
        fn QueryDosDeviceW(name: *const u16, target: *mut u16, size: u32) -> u32;
    }

    fn last_error(context: &str) -> String {
        format!("{context}: {}", std::io::Error::last_os_error())
    }

    pub struct Port {
        file: File,
        original: Option<(Dcb, Timeouts)>,
    }
    impl Port {
        pub fn open(path: &str, baud: u32) -> Result<Self> {
            let path = if path.starts_with(r"\\.\") {
                path.to_owned()
            } else {
                format!(r"\\.\{path}")
            };
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(path)
                .map_err(|e| format!("open serial device: {e}"))?;
            let mut config: Dcb = unsafe { std::mem::zeroed() };
            config.length = std::mem::size_of::<Dcb>() as u32;
            let mut timeouts: Timeouts = unsafe { std::mem::zeroed() };
            if unsafe { GetCommState(file.as_raw_handle(), &mut config) } == 0 {
                return Err(last_error("read serial configuration"));
            }
            if unsafe { GetCommTimeouts(file.as_raw_handle(), &mut timeouts) } == 0 {
                return Err(last_error("read serial timeouts"));
            }
            let port = Self {
                file,
                original: Some((config, timeouts)),
            };
            config.baud = baud;
            config.byte_size = 8;
            config.parity = 0;
            config.stop_bits = 0;
            // Binary mode; no parity, software/hardware flow or character filtering.
            // Retain the prior explicit DTR/RTS enable bits, but disable handshakes.
            let dtr = (config.flags >> 4) & 3;
            let rts = (config.flags >> 12) & 3;
            config.flags = 1 | ((u32::from(dtr != 0)) << 4) | ((u32::from(rts != 0)) << 12);
            if unsafe { SetCommState(port.file.as_raw_handle(), &config) } == 0 {
                return Err(last_error("configure serial 8N1"));
            }
            Ok(port)
        }

        fn timeout(&self, millis: u32) -> Result<()> {
            let config = Timeouts {
                interval: u32::MAX,
                read_multiplier: u32::MAX,
                read_constant: millis.max(1),
                write_multiplier: 0,
                write_constant: millis.max(1),
            };
            if unsafe { SetCommTimeouts(self.file.as_raw_handle(), &config) } == 0 {
                return Err(last_error("configure serial timeout"));
            }
            Ok(())
        }

        pub fn read(&mut self, length: usize, timeout_ms: u32) -> Result<ReadResult> {
            self.timeout(timeout_ms)?;
            let mut bytes = vec![0; length];
            let count = self
                .file
                .read(&mut bytes)
                .map_err(|e| format!("read serial device: {e}"))?;
            bytes.truncate(count);
            Ok(ReadResult {
                bytes,
                timed_out: count == 0,
            })
        }

        pub fn write(&mut self, bytes: &[u8], timeout_ms: u32) -> Result<usize> {
            let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
            let mut count = 0;
            while count < bytes.len() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(format!("serial write timed out; bytes_written={count}"));
                }
                self.timeout(remaining.as_millis().clamp(1, u32::MAX as u128) as u32)
                    .map_err(|error| format!("{error}; bytes_written={count}"))?;
                let written = self
                    .file
                    .write(&bytes[count..])
                    .map_err(|e| format!("write serial device: {e}; bytes_written={count}"))?;
                if written == 0 {
                    return Err(format!("serial write timed out; bytes_written={count}"));
                }
                count += written;
            }
            Ok(count)
        }

        fn restore(&mut self) -> Result<()> {
            if let Some((config, timeouts)) = self.original {
                let mut failure = None;
                if unsafe { SetCommState(self.file.as_raw_handle(), &config) } == 0 {
                    failure = Some(last_error("restore serial configuration"));
                }
                if unsafe { SetCommTimeouts(self.file.as_raw_handle(), &timeouts) } == 0 {
                    let error = last_error("restore serial timeouts");
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
                if let Some(error) = failure {
                    return Err(error);
                }
                self.original = None;
            }
            Ok(())
        }
        pub fn close(mut self) -> Result<()> {
            self.restore()
        }
    }
    impl Drop for Port {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }

    pub fn ports() -> Result<Vec<String>> {
        let mut buffer = vec![0u16; 65_536];
        let count =
            unsafe { QueryDosDeviceW(std::ptr::null(), buffer.as_mut_ptr(), buffer.len() as u32) };
        if count == 0 {
            return Err(last_error("enumerate serial devices"));
        }
        Ok(buffer[..count as usize]
            .split(|c| *c == 0)
            .filter_map(|name| {
                let name = String::from_utf16(name).ok()?;
                if name
                    .strip_prefix("COM")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    Some(name)
                } else {
                    None
                }
            })
            .collect())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod native {
    use super::*;
    pub struct Port;
    impl Port {
        pub fn open(_: &str, _: u32) -> Result<Self> {
            Err("native serial I/O is unavailable on this target".into())
        }
        pub fn read(&mut self, _: usize, _: u32) -> Result<ReadResult> {
            Err("serial unavailable".into())
        }
        pub fn write(&mut self, _: &[u8], _: u32) -> Result<usize> {
            Err("serial unavailable".into())
        }
        pub fn close(self) -> Result<()> {
            Ok(())
        }
    }
    pub fn ports() -> Result<Vec<String>> {
        Err("serial enumeration unavailable".into())
    }
}
