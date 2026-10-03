use super::*;
use crate::sys::{self, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};

/// Resident pages of `[addr, addr + len)` via `mincore`.
fn resident(addr: usize, len: usize, page: usize) -> usize {
    let n = len / page;
    let mut vec = std::vec![0u8; n];
    let rc = unsafe { ::libc::mincore(addr as *mut _, len, vec.as_mut_ptr()) };
    assert_eq!(rc, 0);
    vec.iter().filter(|&&b| b & 1 != 0).count()
}

/// A batch discards every range and leaves the pages zero.
#[test]
fn batch_discards_every_range() {
    let _advice = super::tests::serial();
    let page = crate::sys::features::page_size();
    let len = 8 * page;
    let addr = unsafe {
        sys::mmap(
            0,
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    }
    .unwrap();
    for p in 0..8 {
        unsafe { *((addr + p * page) as *mut u8) = 0xA5 };
    }
    assert_eq!(resident(addr, len, page), 8);
    let mut b = Batch::new_t();
    // Two ranges with a hole between them; the hole stays resident.
    b.push(addr, 3 * page);
    b.push(addr + 4 * page, 4 * page);
    b.flush();
    assert_eq!(resident(addr, len, page), 1);
    assert_eq!(unsafe { *((addr + 3 * page) as *const u8) }, 0xA5);
    for p in [0usize, 1, 2, 4, 5, 6, 7] {
        assert_eq!(
            unsafe { *((addr + p * page) as *const u8) },
            0,
            "page {p} not zero"
        );
    }
    unsafe { sys::munmap(addr, len) }.unwrap();
}
