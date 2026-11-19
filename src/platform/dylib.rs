//! Local ownership policy around Project Z's native loader.
//!
//! zsys owns the platform ABI. This module only supplies contextual errors,
//! checked pointer conversion and the unload/pin policy required by our adapters.
use std::ffi::{CStr, c_void};

use super::Result;

/// A thread-local native image retained until its adapter finishes all work.
///
/// zsys's handle is neither Send nor Sync. Function/data pointers do not carry
/// a Rust borrow: every unsafe symbol caller must keep this owner alive until
/// native objects, callbacks, threads and data references have finished.
pub(crate) struct Library {
    native: zsys::dylib::Library,
    name: String,
    close_on_drop: bool,
}

impl Library {
    pub(crate) fn open(name: &CStr) -> Result<Self> {
        let name_text = name.to_string_lossy().into_owned();
        // SAFETY: this crate requests trusted platform libraries by fixed
        // system names. Their native initializers run with process privileges.
        let native = unsafe { zsys::dylib::Library::open(name) }.map_err(|error| {
            if error == zsys::dylib::Error::Unsupported {
                format!("unsupported native dynamic loading on this target ({name_text})")
            } else {
                format!("cannot load native library {name_text}: {error}")
            }
        })?;
        Ok(Self {
            native,
            name: name_text,
            close_on_drop: true,
        })
    }

    /// Resolve an ABI-compatible function pointer or pointer to a data export.
    ///
    /// # Safety
    ///
    /// T must be the exact exported function-pointer ABI or a correctly typed
    /// data-symbol pointer, not the pointed-to object. All uses must end before
    /// this library closes, unless pin() retains its image for the process.
    pub(crate) unsafe fn symbol<T: Copy>(&self, name: &CStr) -> Result<T> {
        if std::mem::size_of::<T>() != std::mem::size_of::<*mut c_void>() {
            return Err(format!(
                "unsupported pointer ABI for native symbol {} in {}",
                name.to_string_lossy(),
                self.name
            ));
        }
        let pointer = self.native.symbol(name).map_err(|error| {
            format!(
                "native symbol {} in {}: {error}",
                name.to_string_lossy(),
                self.name
            )
        })?;
        // SAFETY: the caller guarantees T's ABI and lifetime; the size check
        // prevents copying past the raw address for an unsupported pointer ABI.
        Ok(unsafe { std::mem::transmute_copy(&pointer) })
    }

    /// Retain this native reference for process-global runtimes such as AT-SPI.
    ///
    /// The Rust wrapper is dropped normally. zsys deliberately does not unload
    /// a dropped handle, so no Rust allocation needs to be forgotten here.
    pub(crate) fn pin(mut self) {
        self.close_on_drop = false;
    }

    /// Explicitly close after all uses have ended; an already closed owner is OK.
    ///
    /// # Safety
    ///
    /// No object, callback, thread, buffer or symbol use may still require this
    /// reference. Native finalizers must be valid on the current thread.
    pub(crate) unsafe fn close(&mut self) -> Result<()> {
        // SAFETY: the caller supplies the native lifetime/finalizer contract.
        unsafe { self.native.close() }
            .map_err(|error| format!("cannot close native library {}: {error}", self.name))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        if self.close_on_drop {
            // SAFETY: adapter owners finish native work before dropping this
            // field; symbol() callers must uphold the same lifetime contract.
            // On failure zsys retains the OS reference. Drop must not panic,
            // retry unpredictably, or write diagnostics into an MCP stream.
            let _ = unsafe { self.close() };
        }
    }
}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use super::*;
    use std::ffi::c_int;

    #[test]
    fn symbols_validate_pointer_size_and_explicit_close_is_idempotent() {
        let mut library = Library::open(c"libc.so.6").unwrap();
        {
            // SAFETY: getpid has this C ABI and its pointer is used while loaded.
            let getpid =
                unsafe { library.symbol::<unsafe extern "C" fn() -> c_int>(c"getpid") }.unwrap();
            assert_eq!(unsafe { getpid() } as u32, std::process::id());
            // Data lookup must return the address of stdout, not its contents.
            let stdout = unsafe { library.symbol::<*const *mut c_void>(c"stdout") }.unwrap();
            assert!(!stdout.is_null());
        }
        let wrong_size = unsafe { library.symbol::<u8>(c"getpid") }.unwrap_err();
        assert!(wrong_size.contains("pointer ABI"));
        assert!(wrong_size.contains("getpid"));
        let missing =
            unsafe { library.symbol::<*const c_void>(c"zuse_test_missing_symbol_6c76d271") }
                .unwrap_err();
        assert!(missing.contains("zuse_test_missing_symbol_6c76d271"));
        assert!(missing.contains("libc.so.6"));
        // SAFETY: no symbol or native work is used after this point.
        unsafe { library.close() }.unwrap();
        unsafe { library.close() }.unwrap();
        let closed = unsafe { library.symbol::<*const c_void>(c"getpid") }.unwrap_err();
        assert!(closed.contains("closed"));
    }

    #[test]
    fn missing_library_errors_include_the_requested_image() {
        let error = Library::open(c"libzuse_missing_fixture_6c76d271.so")
            .err()
            .unwrap();
        assert!(error.contains("libzuse_missing_fixture_6c76d271.so"));
    }
}

#[cfg(all(test, target_env = "musl", target_feature = "crt-static"))]
mod static_musl_tests {
    use super::*;

    #[test]
    fn static_musl_reports_unsupported_before_loading() {
        let error = Library::open(c"libc.so").err().unwrap();
        assert!(error.contains("unsupported native dynamic loading"));
    }
}
