//! Explicit native USB interfaces through the system libusb ABI and zsys loader.
//!
//! Enumeration never opens a device. A connection requires bus/address plus
//! matching vendor/product IDs, and claims one interface without detaching its
//! kernel driver. Transfers are bounded and partial writes are never retried.
use crate::{
    device_io::{self, ReadResult},
    platform::dylib::Library,
};
use std::{
    ffi::{CStr, c_int, c_void},
    ptr,
};

type Result<T> = std::result::Result<T, String>;

#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    length: u8,
    descriptor_type: u8,
    usb_version: u16,
    class: u8,
    subclass: u8,
    protocol: u8,
    max_packet_size: u8,
    vendor: u16,
    product: u16,
    device_version: u16,
    manufacturer_index: u8,
    product_index: u8,
    serial_index: u8,
    configurations: u8,
}

const _: () = assert!(std::mem::size_of::<Descriptor>() == 18);

type Init = unsafe extern "system" fn(*mut *mut c_void) -> c_int;
type Exit = unsafe extern "system" fn(*mut c_void);
type GetList = unsafe extern "system" fn(*mut c_void, *mut *mut *mut c_void) -> isize;
type FreeList = unsafe extern "system" fn(*mut *mut c_void, c_int);
type GetDescriptor = unsafe extern "system" fn(*mut c_void, *mut Descriptor) -> c_int;
type GetNumber = unsafe extern "system" fn(*mut c_void) -> u8;
type Open = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> c_int;
type Close = unsafe extern "system" fn(*mut c_void);
type Interface = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;
type Transfer =
    unsafe extern "system" fn(*mut c_void, u8, *mut u8, c_int, *mut c_int, u32) -> c_int;
type Control = unsafe extern "system" fn(*mut c_void, u8, u8, u16, u16, *mut u8, u16, u32) -> c_int;

struct Api {
    init: Init,
    exit: Exit,
    list: GetList,
    free_list: FreeList,
    descriptor: GetDescriptor,
    bus: GetNumber,
    address: GetNumber,
    open: Open,
    close: Close,
    claim: Interface,
    release: Interface,
    bulk: Transfer,
    interrupt: Transfer,
    control: Control,
    _library: Library,
}

impl Api {
    fn load() -> Result<Self> {
        #[cfg(target_os = "linux")]
        let candidates: &[&CStr] = &[c"libusb-1.0.so.0"];
        #[cfg(target_os = "macos")]
        let candidates: &[&CStr] = &[
            c"libusb-1.0.dylib",
            c"/opt/homebrew/lib/libusb-1.0.dylib",
            c"/usr/local/lib/libusb-1.0.dylib",
        ];
        #[cfg(windows)]
        let candidates: &[&CStr] = &[c"libusb-1.0.dll"];
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        let candidates: &[&CStr] = &[];
        let mut failure =
            "native USB requires system libusb-1.0 on Linux, macOS or Windows".to_string();
        for name in candidates {
            let library = match Library::open(name) {
                Ok(library) => library,
                Err(error) => {
                    failure = error;
                    continue;
                }
            };
            // SAFETY: signatures are from libusb 1.0's public system ABI. The
            // owner remains alive until contexts, devices and transfers finish.
            unsafe {
                return Ok(Self {
                    init: library.symbol(c"libusb_init")?,
                    exit: library.symbol(c"libusb_exit")?,
                    list: library.symbol(c"libusb_get_device_list")?,
                    free_list: library.symbol(c"libusb_free_device_list")?,
                    descriptor: library.symbol(c"libusb_get_device_descriptor")?,
                    bus: library.symbol(c"libusb_get_bus_number")?,
                    address: library.symbol(c"libusb_get_device_address")?,
                    open: library.symbol(c"libusb_open")?,
                    close: library.symbol(c"libusb_close")?,
                    claim: library.symbol(c"libusb_claim_interface")?,
                    release: library.symbol(c"libusb_release_interface")?,
                    bulk: library.symbol(c"libusb_bulk_transfer")?,
                    interrupt: library.symbol(c"libusb_interrupt_transfer")?,
                    control: library.symbol(c"libusb_control_transfer")?,
                    _library: library,
                });
            }
        }
        Err(format!("USB backend unavailable: {failure}"))
    }
}

fn error(code: c_int, context: &str) -> String {
    let reason = match code {
        -1 => "I/O error",
        -2 => "invalid parameter",
        -3 => "permission denied",
        -4 => "device disconnected",
        -5 => "not found",
        -6 => "interface busy",
        -7 => "timeout",
        -8 => "overflow",
        -9 => "pipe stalled",
        -10 => "interrupted",
        -11 => "out of memory",
        -12 => "not supported",
        _ => "native USB failure",
    };
    format!("{context}: {reason} (libusb {code})")
}

struct Context {
    api: Api,
    raw: *mut c_void,
}
impl Context {
    fn new() -> Result<Self> {
        let api = Api::load()?;
        let mut raw = ptr::null_mut();
        let result = unsafe { (api.init)(&mut raw) };
        if result != 0 {
            return Err(error(result, "initialize USB"));
        }
        if raw.is_null() {
            return Err("libusb returned a null context".into());
        }
        Ok(Self { api, raw })
    }
    fn devices(&self) -> Result<DeviceList<'_>> {
        let mut raw = ptr::null_mut();
        let count = unsafe { (self.api.list)(self.raw, &mut raw) };
        if count < 0 {
            return Err(error(count as c_int, "enumerate USB"));
        }
        let list = DeviceList {
            context: self,
            raw,
            count: count as usize,
        };
        if count > 4096 || (count != 0 && raw.is_null()) {
            return Err("USB inventory exceeds bounds or has an invalid list".into());
        }
        Ok(list)
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        unsafe { (self.api.exit)(self.raw) };
    }
}
struct DeviceList<'a> {
    context: &'a Context,
    raw: *mut *mut c_void,
    count: usize,
}
impl DeviceList<'_> {
    fn at(&self, index: usize) -> Result<*mut c_void> {
        if index >= self.count {
            return Err("USB device index out of bounds".into());
        }
        let device = unsafe { *self.raw.add(index) };
        if device.is_null() {
            return Err("USB inventory contains a null device".into());
        }
        Ok(device)
    }
}
impl Drop for DeviceList<'_> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { (self.context.api.free_list)(self.raw, 1) };
        }
    }
}

fn descriptor(context: &Context, device: *mut c_void) -> Result<Descriptor> {
    let mut output = std::mem::MaybeUninit::<Descriptor>::uninit();
    let code = unsafe { (context.api.descriptor)(device, output.as_mut_ptr()) };
    if code != 0 {
        return Err(error(code, "read USB descriptor"));
    }
    let output = unsafe { output.assume_init() };
    if output.length != 18 || output.descriptor_type != 1 {
        return Err("USB device returned an invalid device descriptor".into());
    }
    Ok(output)
}

/// Bus/address and VID/PID are identifiers, not permission or claim checks.
pub fn devices() -> Result<String> {
    let context = Context::new()?;
    let list = context.devices()?;
    let mut output = Vec::new();
    for index in 0..list.count {
        let device = list.at(index)?;
        let info = descriptor(&context, device)?;
        let bus = unsafe { (context.api.bus)(device) };
        let address = unsafe { (context.api.address)(device) };
        output.push(format!(
            "{{\"bus\":{bus},\"address\":{address},\"vendor_id\":{},\"product_id\":{},\"class\":{},\"configurations\":{}}}",
            info.vendor, info.product, info.class, info.configurations
        ));
    }
    Ok(format!("[{}]", output.join(",")))
}

pub struct UsbDevice {
    context: Context,
    handle: *mut c_void,
    interface: u8,
    claimed: bool,
    bus: u8,
    address: u8,
    vendor: u16,
    product: u16,
}

impl UsbDevice {
    /// Does not detach a kernel driver or issue set_configuration.
    /// Releasing an interface follows libusb semantics, including resetting its
    /// alternate setting to zero; arbitrary device configuration is not restored.
    pub fn open(bus: u8, address: u8, vendor: u16, product: u16, interface: u8) -> Result<Self> {
        if !(1..=127).contains(&address) {
            return Err("USB address must be in 1..127".into());
        }
        let context = Context::new()?;
        let handle = {
            let list = context.devices()?;
            let mut selected = None;
            for index in 0..list.count {
                let device = list.at(index)?;
                if unsafe { (context.api.bus)(device) } != bus
                    || unsafe { (context.api.address)(device) } != address
                {
                    continue;
                }
                let info = descriptor(&context, device)?;
                if info.vendor != vendor || info.product != product {
                    return Err(
                        "USB bus/address no longer matches the explicitly selected VID/PID".into(),
                    );
                }
                if selected.is_some() {
                    return Err("USB selection is ambiguous".into());
                }
                selected = Some(device);
            }
            let device = selected.ok_or("selected USB device is absent")?;
            let mut handle = ptr::null_mut();
            let code = unsafe { (context.api.open)(device, &mut handle) };
            if code != 0 {
                return Err(error(code, "open selected USB device"));
            }
            if handle.is_null() {
                return Err("libusb returned a null device handle".into());
            }
            handle
        };
        let mut result = Self {
            context,
            handle,
            interface,
            claimed: false,
            bus,
            address,
            vendor,
            product,
        };
        let code = unsafe { (result.context.api.claim)(result.handle, interface.into()) };
        if code != 0 {
            return Err(format!(
                "{}; existing kernel drivers are not detached",
                error(code, "claim USB interface")
            ));
        }
        result.claimed = true;
        Ok(result)
    }

    pub fn status(&self) -> String {
        format!(
            "{{\"connected\":{},\"bus\":{},\"address\":{},\"vendor_id\":{},\"product_id\":{},\"interface\":{},\"kernel_driver_detached\":false}}",
            !self.handle.is_null(),
            self.bus,
            self.address,
            self.vendor,
            self.product,
            self.interface
        )
    }

    fn handle(&self) -> Result<*mut c_void> {
        if self.handle.is_null() || !self.claimed {
            Err("USB interface is not connected".into())
        } else {
            Ok(self.handle)
        }
    }

    /// One USB transfer; a short packet is returned without an implicit retry.
    pub fn read(
        &self,
        endpoint: u8,
        length: usize,
        timeout_ms: u32,
        interrupt: bool,
    ) -> Result<ReadResult> {
        validate_endpoint(endpoint, true)?;
        device_io::validate_length(length)?;
        device_io::validate_timeout(timeout_ms)?;
        let handle = self.handle()?;
        let mut bytes = vec![0; length];
        let mut count = 0;
        let transfer = if interrupt {
            self.context.api.interrupt
        } else {
            self.context.api.bulk
        };
        let code = unsafe {
            transfer(
                handle,
                endpoint,
                bytes.as_mut_ptr(),
                length as c_int,
                &mut count,
                timeout_ms,
            )
        };
        if count < 0 || count as usize > length {
            return Err("USB transfer reported an invalid byte count".into());
        }
        if code != 0 && code != -7 {
            return Err(format!("{}; bytes_read={count}", error(code, "USB read")));
        }
        bytes.truncate(count as usize);
        Ok(ReadResult {
            bytes,
            timed_out: code == -7,
        })
    }

    /// Return success only after one complete transfer; never retry a partial write.
    pub fn write(
        &self,
        endpoint: u8,
        bytes: &[u8],
        timeout_ms: u32,
        interrupt: bool,
    ) -> Result<usize> {
        validate_endpoint(endpoint, false)?;
        device_io::validate_timeout(timeout_ms)?;
        if bytes.len() > device_io::MAX_BYTES {
            return Err("USB write exceeds 65536 bytes".into());
        }
        let handle = self.handle()?;
        let mut bytes = bytes.to_vec();
        let mut count = 0;
        let transfer = if interrupt {
            self.context.api.interrupt
        } else {
            self.context.api.bulk
        };
        let code = unsafe {
            transfer(
                handle,
                endpoint,
                bytes.as_mut_ptr(),
                bytes.len() as c_int,
                &mut count,
                timeout_ms,
            )
        };
        if count < 0 || count as usize > bytes.len() {
            return Err("USB transfer reported an invalid byte count".into());
        }
        if code != 0 {
            return Err(format!(
                "{}; bytes_written={count}; not retried",
                error(code, "USB write")
            ));
        }
        if count as usize != bytes.len() {
            return Err(format!(
                "short USB write; bytes_written={count}; not retried"
            ));
        }
        Ok(count as usize)
    }

    pub fn control_read(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        length: usize,
        timeout_ms: u32,
    ) -> Result<ReadResult> {
        if request_type & 0x80 == 0 {
            return Err("USB control read requires the IN direction bit".into());
        }
        validate_control_length(length)?;
        device_io::validate_timeout(timeout_ms)?;
        let mut bytes = vec![0; length];
        let count = unsafe {
            (self.context.api.control)(
                self.handle()?,
                request_type,
                request,
                value,
                index,
                bytes.as_mut_ptr(),
                length as u16,
                timeout_ms,
            )
        };
        if count < 0 {
            return Err(error(
                count,
                "USB control read; transfer completion is unknown",
            ));
        }
        if count as usize > length {
            return Err("USB control read reported an invalid byte count".into());
        }
        bytes.truncate(count as usize);
        Ok(ReadResult {
            bytes,
            timed_out: false,
        })
    }

    pub fn control_write(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        bytes: &[u8],
        timeout_ms: u32,
    ) -> Result<usize> {
        if request_type & 0x80 != 0 {
            return Err("USB control write requires the OUT direction bit".into());
        }
        validate_control_length(bytes.len())?;
        device_io::validate_timeout(timeout_ms)?;
        let mut bytes = bytes.to_vec();
        let count = unsafe {
            (self.context.api.control)(
                self.handle()?,
                request_type,
                request,
                value,
                index,
                bytes.as_mut_ptr(),
                bytes.len() as u16,
                timeout_ms,
            )
        };
        if count < 0 {
            return Err(error(
                count,
                "USB control write; transfer completion is unknown; not retried",
            ));
        }
        if count as usize != bytes.len() {
            return Err(format!(
                "short USB control write; bytes_written={count}; not retried"
            ));
        }
        Ok(count as usize)
    }

    pub fn close(mut self) -> Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> Result<()> {
        let mut result = Ok(());
        if self.claimed {
            let code = unsafe { (self.context.api.release)(self.handle, self.interface.into()) };
            self.claimed = false;
            if code != 0 {
                result = Err(error(code, "release USB interface"));
            }
        }
        if !self.handle.is_null() {
            unsafe { (self.context.api.close)(self.handle) };
            self.handle = ptr::null_mut();
        }
        result
    }
}
impl Drop for UsbDevice {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

pub fn validate_endpoint(endpoint: u8, input: bool) -> Result<()> {
    if endpoint & 0x70 != 0 || endpoint & 0x0f == 0 || (endpoint & 0x80 != 0) != input {
        return Err("USB endpoint must be 1..15 with the requested IN/OUT direction bit".into());
    }
    Ok(())
}
pub fn validate_control_length(length: usize) -> Result<()> {
    if length > u16::MAX as usize {
        return Err("USB control data exceeds 65535 bytes".into());
    }
    Ok(())
}
