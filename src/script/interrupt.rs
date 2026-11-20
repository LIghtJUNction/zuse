//! Explicit, process-wide signal ownership for the command-line runner.
//!
//! The library does not install this guard. An embedding application should
//! provide its own cancellation token instead of replacing its signal handlers.
//! On supported Unix targets the handler only sets an atomic flag; the runner
//! observes it and releases resources in normal Rust code. Windows and other
//! unimplemented targets keep their native Ctrl+C behavior, which can terminate
//! the process without running Rust destructors. Tokens still work there.

use std::sync::atomic::{AtomicBool, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Owns the CLI's SIGINT and SIGTERM handlers until dropped.
///
/// Only one guard can exist in a process. Other code must not replace these
/// process-wide handlers while the guard is alive. Drop restores the complete
/// previous dispositions, including their signal masks and flags.
pub struct InterruptGuard {
    previous: platform::Previous,
}

impl InterruptGuard {
    /// Whether this target has native signal handling implemented.
    /// Unsupported targets return a no-op guard from `install`.
    pub const fn is_supported() -> bool {
        platform::SUPPORTED
    }

    /// Explicitly install the CLI handlers before acquiring native resources.
    pub fn install() -> Result<Self, String> {
        ACTIVE
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| "the CLI interrupt guard is already installed".to_string())?;
        CANCELLED.store(false, Ordering::Relaxed);
        match platform::install() {
            Ok(previous) => Ok(Self { previous }),
            Err(error) => {
                ACTIVE.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    /// True after either SIGINT or SIGTERM was received during this guard.
    /// This stays true until the guard is dropped and a new guard is installed.
    pub fn cancelled(&self) -> bool {
        CANCELLED.load(Ordering::Relaxed)
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        platform::restore(&self.previous);
        ACTIVE.store(false, Ordering::Release);
    }
}

// These are the verified libc layouts for the desktop targets we support.
// Other targets deliberately do not guess at the sigaction ABI.
#[cfg(any(
    all(
        target_os = "linux",
        any(target_env = "gnu", target_env = "musl"),
        target_pointer_width = "64",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
mod platform {
    use super::{CANCELLED, Ordering};
    use std::{ffi::c_int, ptr};

    pub const SUPPORTED: bool = true;
    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;

    #[cfg(target_os = "linux")]
    type SignalSet = [u64; 16];
    #[cfg(target_os = "macos")]
    type SignalSet = u32;

    // Handler/restorer slots are stored as machine words: the handler union
    // can also contain SIG_DFL (0), SIG_IGN (1), or a three-argument handler.
    // None of those previous values is ever called through a Rust fn pointer.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SignalAction {
        handler: usize,
        mask: SignalSet,
        flags: c_int,
        #[cfg(target_os = "linux")]
        restorer: usize,
    }

    unsafe extern "C" {
        fn sigaction(
            signal: c_int,
            action: *const SignalAction,
            previous: *mut SignalAction,
        ) -> c_int;
        fn sigemptyset(set: *mut SignalSet) -> c_int;
    }

    pub struct Previous {
        interrupt: SignalAction,
        terminate: SignalAction,
    }

    extern "C" fn handle_interrupt(_signal: c_int) {
        // AtomicBool is lock-free on the explicitly supported desktop targets.
        // No allocation, locks, logging, unwinding, or native cleanup here.
        CANCELLED.store(true, Ordering::Relaxed);
    }

    fn empty_action() -> SignalAction {
        SignalAction {
            handler: 0,
            mask: SignalSet::default(),
            flags: 0,
            #[cfg(target_os = "linux")]
            restorer: 0,
        }
    }

    pub fn install() -> Result<Previous, String> {
        let mut action = empty_action();
        action.handler = handle_interrupt as *const () as usize;
        // No SA_RESTART: interrupted work can return to the cancellation check.
        // No SA_RESETHAND: repeated interrupts remain safe until cleanup ends.
        // SAFETY: action.mask has the verified sigset_t layout and is writable.
        if unsafe { sigemptyset(&mut action.mask) } != 0 {
            return Err(format!(
                "initialize CLI signal mask: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut previous = Previous {
            interrupt: empty_action(),
            terminate: empty_action(),
        };
        // SAFETY: these are valid POSIX signals and fully initialized, correctly
        // aligned libc sigaction structures. All pointers outlive the call.
        if unsafe { sigaction(SIGINT, &action, &mut previous.interrupt) } != 0 {
            return Err(format!(
                "install CLI SIGINT handler: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: same argument guarantees as the SIGINT call above.
        if unsafe { sigaction(SIGTERM, &action, &mut previous.terminate) } != 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: restore the complete disposition obtained above before
            // reporting the partial installation failure.
            unsafe { sigaction(SIGINT, &previous.interrupt, ptr::null_mut()) };
            return Err(format!("install CLI SIGTERM handler: {error}"));
        }
        Ok(previous)
    }

    pub fn restore(previous: &Previous) {
        // SAFETY: valid signals, unchanged dispositions returned by libc, and
        // sole ownership of this module's installation until both are restored.
        unsafe {
            sigaction(SIGINT, &previous.interrupt, ptr::null_mut());
            sigaction(SIGTERM, &previous.terminate, ptr::null_mut());
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::InterruptGuard;
        use super::*;
        use std::{process::Command, sync::atomic::AtomicUsize};

        static PREVIOUS_CALLS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" {
            fn raise(signal: c_int) -> c_int;
            fn sigaddset(set: *mut SignalSet, signal: c_int) -> c_int;
        }

        extern "C" fn previous_handler(_signal: c_int) {
            PREVIOUS_CALLS.fetch_add(1, Ordering::Relaxed);
        }

        fn read_action(signal: c_int) -> SignalAction {
            let mut action = empty_action();
            // SAFETY: query a valid signal into a writable initialized struct.
            assert_eq!(unsafe { sigaction(signal, ptr::null(), &mut action) }, 0);
            action
        }

        fn assert_restored(before: SignalAction, after: SignalAction) {
            assert_eq!(before.handler, after.handler);
            assert_eq!(before.flags, after.flags);
            assert_eq!(before.mask, after.mask);
            #[cfg(target_os = "linux")]
            assert_eq!(before.restorer, after.restorer);
        }

        #[test]
        fn signals_cancel_and_restore_in_subprocess() {
            // The regular test runner never receives a process signal or has
            // its dispositions changed. The ignored fixture runs alone.
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "interrupt::platform::tests::signal_fixture",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env("ZUSE_INTERRUPT_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "signal fixture failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }

        #[test]
        #[ignore = "process-global signals; invoked only by the subprocess test"]
        fn signal_fixture() {
            if std::env::var("ZUSE_INTERRUPT_TEST_CHILD").as_deref() != Ok("1") {
                return;
            }
            assert!(InterruptGuard::is_supported());
            let mut custom = empty_action();
            custom.handler = previous_handler as *const () as usize;
            #[cfg(target_os = "linux")]
            {
                custom.flags = 0x10000000; // SA_RESTART
            }
            #[cfg(target_os = "macos")]
            {
                custom.flags = 0x0002; // SA_RESTART
            }
            // SAFETY: initialized signal mask and valid supported signals.
            unsafe {
                assert_eq!(sigemptyset(&mut custom.mask), 0);
                assert_eq!(sigaddset(&mut custom.mask, SIGTERM), 0);
                assert_eq!(sigaction(SIGINT, &custom, ptr::null_mut()), 0);
                assert_eq!(sigaction(SIGTERM, &custom, ptr::null_mut()), 0);
            }
            let before_interrupt = read_action(SIGINT);
            let before_terminate = read_action(SIGTERM);

            let guard = InterruptGuard::install().unwrap();
            assert!(!guard.cancelled());
            assert!(InterruptGuard::install().is_err());
            assert!(
                std::thread::spawn(InterruptGuard::install)
                    .join()
                    .unwrap()
                    .is_err()
            );
            assert!(!guard.cancelled());
            // SAFETY: the active guard installed a non-terminating handler.
            unsafe {
                assert_eq!(raise(SIGINT), 0);
                assert_eq!(raise(SIGINT), 0);
                assert_eq!(raise(SIGTERM), 0);
            }
            assert!(guard.cancelled());
            assert_eq!(PREVIOUS_CALLS.load(Ordering::Relaxed), 0);
            drop(guard);

            assert_restored(before_interrupt, read_action(SIGINT));
            assert_restored(before_terminate, read_action(SIGTERM));
            // SAFETY: Drop restored our non-terminating previous handler.
            unsafe {
                assert_eq!(raise(SIGINT), 0);
                assert_eq!(raise(SIGTERM), 0);
            }
            assert_eq!(PREVIOUS_CALLS.load(Ordering::Relaxed), 2);

            let fresh = InterruptGuard::install().unwrap();
            assert!(!fresh.cancelled());
            drop(fresh);
            assert_restored(before_interrupt, read_action(SIGINT));
            assert_restored(before_terminate, read_action(SIGTERM));
        }
    }
}

#[cfg(not(any(
    all(
        target_os = "linux",
        any(target_env = "gnu", target_env = "musl"),
        target_pointer_width = "64",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod platform {
    pub const SUPPORTED: bool = false;
    pub struct Previous;

    pub fn install() -> Result<Previous, String> {
        Ok(Previous)
    }

    pub fn restore(_previous: &Previous) {}
}
