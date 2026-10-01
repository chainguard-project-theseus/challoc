//! The abort path (design.md section 19): `challoc: <msg>` on stderr, then
//! `SIGABRT` delivered the way glibc's `abort` delivers it. Nothing here
//! allocates; the sink is a fixed buffer on the stack.

use super::{SIG_UNBLOCK, SIGABRT, SIGSET_WORDS_BYTES, STDERR_FILENO};
use super::{exit_group, getpid, gettid, nr, raw, write_all};
use core::fmt;
use vstd::prelude::*;

verus! {

/// The kernel's `struct sigaction` for `rt_sigaction`. Both x86_64 and
/// aarch64 define `SA_RESTORER`, so the layout is the same.
#[repr(C)]
#[derive(Copy, Clone)]
struct KernelSigaction {
    handler: usize,
    flags: usize,
    restorer: usize,
    mask: u64,
}

/// Fixed-size stderr sink for abort messages. Buffers so a message is
/// usually one `write(2)`, flushing when full. Never allocates.
pub struct Stderr {
    buf: [u8; 512],
    len: usize,
}

impl Stderr {
    /// At most the buffer is pending.
    #[verifier::type_invariant]
    spec fn wf(&self) -> bool {
        self.len <= 512
    }

    pub const fn new() -> Self {
        Stderr {
            buf: [0; 512],
            len: 0,
        }
    }

    /// The bytes waiting in the buffer.
    pub closed spec fn pending(&self) -> nat {
        self.len as nat
    }

    pub fn flush(&mut self)
        ensures
            final(self).pending() == 0,
    {
        proof {
            use_type_invariant(&*self);
        }
        if self.len > 0 {
            write_all(STDERR_FILENO, &self.buf[..self.len]);
            self.len = 0;
        }
    }
}

impl Default for Stderr {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Write for Stderr {
    /// Copies whole characters: a chunk ends on a character boundary, and a
    /// character that does not fit in what is left of the buffer goes out
    /// after a flush, so the abort path never slices inside one.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut b = s.as_bytes();
        while !b.is_empty()
            decreases 2 * b@.len() + (if self.len > 0 { 1int } else { 0int }),
        {
            proof {
                use_type_invariant(&*self);
            }
            let room = self.buf.len() - self.len;
            if room == 0 {
                self.flush();
                continue;
            }
            // Cut on a character boundary: a UTF-8 continuation byte
            // (`0b10xx_xxxx`) is never the first byte of a character.
            let mut n = b.len().min(room);
            while n > 0 && n < b.len() && (b[n] & 0xC0) == 0x80
                invariant
                    n <= b@.len(),
                    n <= room,
                    room == 512 - self.len,
                decreases n,
            {
                n -= 1;
            }
            if n == 0 {
                if self.len > 0 {
                    self.flush();
                    continue;
                }
                // An empty buffer that cannot hold the first character: only
                // bytes that are not UTF-8 get here, which a `str` never is.
                n = b.len().min(room);
            }
            let mut i = 0;
            while i < n
                invariant
                    self.len + n <= 512,
                    n <= b@.len(),
                    i <= n,
                decreases n - i,
            {
                self.buf[self.len + i] = b[i];
                i += 1;
            }
            self.len += n;
            b = &b[n..];
        }
        Ok(())
    }
}

/// Write `challoc: <msg>` to stderr and terminate with `SIGABRT`.
/// Formatting is outside what Verus checks.
#[verifier::external_body]
#[cold]
#[inline(never)]
pub fn abort(msg: &str) -> ! {
    abort_fmt(format_args!("{msg}"))
}

/// Formatting variant of [`abort`]. Use through the `abort!` macro.
#[verifier::external_body]
#[cold]
#[inline(never)]
pub fn abort_fmt(args: fmt::Arguments<'_>) -> ! {
    use fmt::Write as _;
    let mut out = Stderr::new();
    // Errors from the sink are impossible (it never fails), so ignore.
    let _ = out.write_str("challoc: ");
    let _ = out.write_fmt(args);
    let _ = out.write_str("\n");
    out.flush();
    die()
}

/// Raise `SIGABRT` on the calling thread, mirroring glibc's `abort`:
/// unblock the signal, deliver it, and if a handler swallowed it reset the
/// disposition to default and deliver again. `exit_group` is the last
/// resort so the process never continues past an inconsistency.
#[cold]
fn die() -> ! {
    let set: u64 = 1 << (SIGABRT - 1);
    let pid = getpid() as isize as usize;
    let tid = gettid() as isize as usize;
    // SAFETY: `set` is a live 8-byte sigset; the kernel validates the rest.
    unsafe {
        raw::syscall4(
            nr::RT_SIGPROCMASK,
            SIG_UNBLOCK as usize,
            core::ptr::from_ref(&set) as usize,
            0,
            SIGSET_WORDS_BYTES,
        );
        raw::syscall3(nr::TGKILL, pid, tid, SIGABRT as usize);
        // A handler returned. Reset to SIG_DFL and try once more.
        let dfl = KernelSigaction {
            handler: 0,
            flags: 0,
            restorer: 0,
            mask: 0,
        };
        raw::syscall4(
            nr::RT_SIGACTION,
            SIGABRT as usize,
            core::ptr::from_ref(&dfl) as usize,
            0,
            SIGSET_WORDS_BYTES,
        );
        raw::syscall3(nr::TGKILL, pid, tid, SIGABRT as usize);
    }
    // 128 + SIGABRT, the shell convention for death by signal 6.
    exit_group(134)
}

} // verus!

/// Abort the process with a formatted `challoc: ...` message on stderr.
#[macro_export]
macro_rules! abort {
    ($($arg:tt)*) => {
        $crate::sys::abort_fmt(::core::format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use std::{env, format};

    const ABORT_ENV: &str = "CHALLOC_SYS_TEST_ABORT";

    #[test]
    fn abort_child_hook() {
        match env::var(ABORT_ENV).as_deref() {
            Ok("plain") => abort("test abort"),
            Ok("fmt") => abort!("bad free of {:#x} ({})", 0xdead_beef_usize, "unknown"),
            Ok("handler") => {
                extern "C" fn ignore(_: i32) {}
                // SAFETY: installing a trivial handler in a throwaway child.
                unsafe { libc::signal(libc::SIGABRT, ignore as *const () as usize) };
                abort("with handler")
            }
            Ok("blocked") => {
                // SAFETY: plain sigprocmask in a throwaway child.
                unsafe {
                    let mut set: libc::sigset_t = core::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    libc::sigaddset(&mut set, libc::SIGABRT);
                    libc::pthread_sigmask(libc::SIG_BLOCK, &set, core::ptr::null_mut());
                }
                abort("while blocked")
            }
            Ok("utf8") => {
                // `challoc: ` (9 bytes) and 502 ASCII bytes leave one byte of
                // the 512-byte buffer when the two-byte `\u{e9}` arrives.
                let s = format!("{}\u{e9} tail", "x".repeat(502));
                abort!("{s}")
            }
            Ok("long") => {
                // Longer than the 512-byte Stderr buffer: exercises flush.
                let long = "x".repeat(1500);
                abort!("{long}")
            }
            _ => {} // parent run: nothing to do
        }
    }

    fn run_abort_child(mode: &str) -> (Option<i32>, std::string::String) {
        let exe = env::current_exe().unwrap();
        let output = Command::new(exe)
            .args([
                "--exact",
                "sys::abort::tests::abort_child_hook",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ABORT_ENV, mode)
            .output()
            .unwrap();
        let stderr = std::string::String::from_utf8_lossy(&output.stderr).into_owned();
        (output.status.signal(), stderr)
    }

    #[test]
    fn abort_splits_a_character_at_the_buffer_end() {
        let (sig, err) = run_abort_child("utf8");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("x\u{e9} tail\n"), "stderr: {err}");
    }

    #[test]
    fn abort_writes_message_and_raises_sigabrt() {
        let (sig, err) = run_abort_child("plain");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("challoc: test abort\n"), "stderr: {err}");
    }

    #[test]
    fn abort_macro_formats() {
        let (sig, err) = run_abort_child("fmt");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(
            err.contains("challoc: bad free of 0xdeadbeef (unknown)\n"),
            "stderr: {err}"
        );
    }

    #[test]
    fn abort_defeats_a_returning_handler() {
        let (sig, err) = run_abort_child("handler");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("challoc: with handler\n"), "stderr: {err}");
    }

    #[test]
    fn abort_defeats_a_blocked_signal() {
        let (sig, err) = run_abort_child("blocked");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        assert!(err.contains("challoc: while blocked\n"), "stderr: {err}");
    }

    #[test]
    fn abort_flushes_long_messages() {
        let (sig, err) = run_abort_child("long");
        assert_eq!(sig, Some(SIGABRT), "stderr: {err}");
        let expected = format!("challoc: {}\n", "x".repeat(1500));
        assert!(err.contains(&expected), "stderr len {}", err.len());
    }
}
