use super::*;
use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};
use crate::sys::features::Features;

/// Both tests below switch the process-wide backend; take turns.
static BACKEND_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resident pages of `[addr, addr + len)` via `mincore`.
fn resident(addr: usize, len: usize, page: usize) -> usize {
    let n = len / page;
    let mut vec = std::vec![0u8; n];
    let rc = unsafe { ::libc::mincore(addr as *mut _, len, vec.as_mut_ptr()) };
    assert_eq!(rc, 0);
    vec.iter().filter(|&&b| b & 1 != 0).count()
}

/// A batch through whatever backend the kernel offers (and, when that
/// is `process_madvise`, again through the loop) discards every range
/// and leaves the pages zero.
#[test]
fn batch_discards_through_the_probed_backend_and_the_loop() {
    let _serial = BACKEND_TEST.lock().unwrap();
    let _advice = super::tests::serial();
    let page = crate::sys::features::page_size();
    let f = Features::probe();
    let backends = [f.process_madvise, ProcessMadvise::Unavailable];
    for (i, backend) in backends.iter().enumerate() {
        configure(*backend);
        assert_eq!(uses_process_madvise(), *backend != ProcessMadvise::Unavailable);
        let len = 8 * page;
        let addr = unsafe { sys::mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) }.unwrap();
        for p in 0..8 {
            unsafe { *((addr + p * page) as *mut u8) = 0xA5 };
        }
        assert_eq!(resident(addr, len, page), 8);
        let mut b = Batch::new();
        // Two ranges with a hole between them; the hole stays resident.
        b.push(addr, 3 * page);
        b.push(addr + 4 * page, 4 * page);
        b.flush();
        assert_eq!(resident(addr, len, page), 1, "backend {i}: {backend:?}");
        assert_eq!(unsafe { *((addr + 3 * page) as *const u8) }, 0xA5);
        for p in [0usize, 1, 2, 4, 5, 6, 7] {
            assert_eq!(unsafe { *((addr + p * page) as *const u8) }, 0, "page {p} not zero ({backend:?})");
        }
        unsafe { sys::munmap(addr, len) }.unwrap();
    }
    configure(ProcessMadvise::Unavailable);
    if let ProcessMadvise::Pidfd(fd) = f.process_madvise {
        let _ = sys::close(fd);
    }
}

/// The fork child drops an inherited pidfd and keeps `PIDFD_SELF`.
#[test]
fn fork_child_downgrades_a_pidfd_backend() {
    let _serial = BACKEND_TEST.lock().unwrap();
    configure(ProcessMadvise::PidfdSelf);
    assert_eq!(fork_child(), ProcessMadvise::PidfdSelf);
    assert!(uses_process_madvise());
    let fd = sys::open(b"/dev/null\0", sys::O_RDONLY).unwrap();
    configure(ProcessMadvise::Pidfd(fd));
    assert_eq!(fork_child(), ProcessMadvise::Unavailable);
    assert!(!uses_process_madvise());
    // The fd was closed.
    assert!(sys::close(fd).is_err());
    configure(ProcessMadvise::Unavailable);
    assert_eq!(fork_child(), ProcessMadvise::Unavailable);
}
