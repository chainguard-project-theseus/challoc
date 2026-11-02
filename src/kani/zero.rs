//! The axioms that read bytes known to be zero as typed values:
//! `vspec::zero::axiom_zero_word` (a `u64`) and `class::table`'s
//! `axiom_zero_owner` (an `AtomicU16`), `axiom_zero_desc` (a `SlabDesc`)
//! and `axiom_fresh_words` (`AtomicU64`s) (`verus/ledger/src/`), checked
//! against their contracts. Each turns a `ZeroRaw`, ghost bytes that read
//! as zero, into the permissions `PointsToRaw::into_typed` would give,
//! initialised with the value the bytes hold, which vstd's raw
//! permissions do not track.
//!
//! A harness states the premise as bytes: a buffer of arbitrary bytes,
//! aligned to 16, whose bytes in the range are then set to zero. At every
//! offset in it that the `requires` admits, it checks that the pointer is
//! what a typed read needs (`kani::mem::can_dereference`, `-Z
//! mem-predicates`: non-null, aligned, in bounds, and the bytes a valid
//! value; Kani 0.68 does not check alignment on the read itself), and
//! that the read gives the value the `ensures` names. `axiom_zero_word`'s
//! `w.ptr() == p` is checked by reading through `p` itself. `perm_at`'s
//! pointer carries `cast_provenance()`, the provenance of an address
//! cast, which Kani cannot observe: the harnesses read through a pointer
//! to their own buffer, so the ledger lists that clause as not checked.

use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use crate::slab::{SlabDesc, State};

/// The buffer's length: two descriptors, four words.
const N: usize = 32;

/// `N` bytes, aligned to 16, the alignment of the most aligned type
/// read here (`SlabDesc`).
#[repr(C, align(16))]
struct Buf([u8; N]);

/// A buffer of arbitrary bytes but for the `n` bytes from `i`, which are
/// zero: the axioms' premise, a `ZeroRaw` exactly over those bytes.
fn zeroed(i: usize, n: usize) -> Buf {
    let mut b = Buf(kani::any());
    for k in i..i + n {
        b.0[k] = 0;
    }
    b
}

/// The pointer to `b`'s byte `i`, as a `T`.
fn at<T>(b: &Buf, i: usize) -> *const T {
    b.0.as_ptr().wrapping_add(i).cast::<T>()
}

/// `vspec::zero::axiom_zero_word`, `requires p@.addr != 0`, `p` 8-aligned
/// and `z.is_range(p@.addr, 8)` of `p`'s provenance, `ensures w.ptr() ==
/// p, w.is_init(), w.value() == 0`: at every 8-aligned offset of the
/// buffer whose eight bytes there are zero (the rest arbitrary), `p` is
/// what a read of a `u64` needs and the read gives 0.
#[kani::proof]
fn zero_word_reads_zero() {
    let mut i = 0;
    while i + 8 <= N {
        let b = zeroed(i, 8);
        let p = at::<u64>(&b, i);
        assert!(kani::mem::can_dereference(p));
        // SAFETY: `can_dereference`, checked above.
        assert_eq!(unsafe { p.read() }, 0);
        i += 8;
    }
}

/// `class::table::axiom_zero_owner`, `requires z.is_range(addr, 2)`,
/// `0 < addr`, `addr % 2 == 0`, `ensures perm_at(r, addr)` (an
/// initialised `AtomicU16` at `addr`): at every even offset of the buffer
/// whose two bytes there are zero, the pointer is what a read of an
/// `AtomicU16` needs, and (beyond the contract, which states no value)
/// the word loads 0. Not checked: `perm_at`'s provenance.
#[kani::proof]
fn zero_owner_loads_zero() {
    let mut i = 0;
    while i + 2 <= N {
        let b = zeroed(i, 2);
        let p = at::<AtomicU16>(&b, i);
        assert!(kani::mem::can_dereference(p));
        // SAFETY: `can_dereference`, checked above.
        assert_eq!(unsafe { &*p }.load(Ordering::Relaxed), 0);
        i += 2;
    }
}

/// `class::table::axiom_zero_desc`, `requires z.is_range(addr, 16)`,
/// `0 < addr`, `addr % 16 == 0`, `ensures perm_at(r.0, addr)`,
/// `desc_id(&r.0.value()) == addr`, `desc_fresh(r.1, addr)`: at both
/// 16-aligned offsets of the buffer whose 16 bytes there are zero, the
/// pointer is what a read of a `SlabDesc` needs, and the descriptor's
/// accessors read what `desc_fresh` records: state byte 0
/// (`state_byte(State::Fresh)`, read as `Fresh`), `dirty_at` 0 and arena
/// 0. Not checked: `perm_at`'s provenance, and `desc_id` and the
/// permission's `id`, ghost identities.
#[kani::proof]
fn zero_desc_reads_fresh() {
    let mut i = 0;
    while i + 16 <= N {
        let b = zeroed(i, 16);
        let p = at::<SlabDesc>(&b, i);
        assert!(kani::mem::can_dereference(p));
        // SAFETY: `can_dereference`, checked above.
        let d = unsafe { &*p };
        assert_eq!(d.state_bytes().0, 0);
        assert!(matches!(d.state(), State::Fresh));
        assert_eq!(d.dirty_at(), 0);
        assert_eq!(d.arena(), 0);
        i += 16;
    }
}

/// `class::table::axiom_fresh_words`, `requires` the `8 * n` bytes from
/// `base` in `z`, `0 < base`, `base % 8 == 0`, `ensures` each word's
/// value permission holds 0 and `words_perms_ok` (each word's permission
/// initialised at `base + 8 * j`): for every `n` up to four words and
/// every 8-aligned `base` with the words in the buffer, the `8 * n` bytes
/// from `base` zero, each word's pointer is what a read of an `AtomicU64`
/// needs and the word loads 0. Not checked: `perm_at`'s provenance, the
/// one-shot token `once`, and the location identities (`WordPerm::addr`, `word_addr`), ghost.
#[kani::proof]
fn fresh_words_load_zero() {
    for n in 0..=N / 8 {
        let mut base = 0;
        while base + 8 * n <= N {
            let b = zeroed(base, 8 * n);
            for j in 0..n {
                let p = at::<AtomicU64>(&b, base + 8 * j);
                assert!(kani::mem::can_dereference(p));
                // SAFETY: `can_dereference`, checked above.
                assert_eq!(unsafe { &*p }.load(Ordering::Relaxed), 0);
            }
            base += 8;
        }
    }
}
