//! Verus spike: one word of `AtomicBitmap` (`src/bitmap.rs`), a whole-word
//! slab (`shift == 0`), with ownership of each slot as a ghost token.
//!
//! The word's invariant holds the tokens of exactly the slab's clear bits.
//! `claim_word` moves the tokens of the bits it won to its caller, so two
//! callers can never both own a slot. `release` takes the tokens back, and
//! because tokens from one authority are disjoint, the check the Rust makes
//! after `fetch_and` (`prev & mask == mask`, "released twice") is proved to
//! hold for every caller that got its mask from `claim_word`.
//!
//! Verus atomics are sequentially consistent: the `Relaxed`, `Acquire` and
//! `Release` orderings of the Rust are not modelled. vstd also compiles
//! every atomic operation as `SeqCst`.
//!
//! This is a copy of the logic, not `src/bitmap.rs` itself. Verify with
//! `verus slot_word.rs` (Verus 0.2026.09.27, Rust 1.98.1).

use vstd::atomic_ghost::*;
use vstd::prelude::*;
use vstd::resource::Loc;
use vstd::resource::set::{GhostSetAuth, GhostSubset};

verus! {

broadcast use {vstd::set_lib::range_set_properties, vstd::set::lemma_set_filter};

pub open spec fn bit(x: u64, b: u64) -> bool {
    (x >> b) & 1u64 == 1u64
}

/// The slots whose bits are set in `x`.
pub open spec fn bits(x: u64) -> Set<u64> {
    Set::<u64>::range(0u64, 64u64).filter(|b: u64| bit(x, b))
}

proof fn lemma_bit_ops(x: u64, y: u64, b: u64)
    requires b < 64,
    ensures
        bit(x | y, b) == (bit(x, b) || bit(y, b)),
        bit(x & y, b) == (bit(x, b) && bit(y, b)),
        bit(!x, b) == !bit(x, b),
{
    assert(((x | y) >> b) & 1u64 == 1u64 <==> ((x >> b) & 1u64 == 1u64 || (y >> b) & 1u64 == 1u64)) by (bit_vector)
        requires b < 64;
    assert(((x & y) >> b) & 1u64 == 1u64 <==> ((x >> b) & 1u64 == 1u64 && (y >> b) & 1u64 == 1u64)) by (bit_vector)
        requires b < 64;
    assert(((!x) >> b) & 1u64 == 1u64 <==> !((x >> b) & 1u64 == 1u64)) by (bit_vector)
        requires b < 64;
}

proof fn lemma_bits_ops(x: u64, y: u64)
    ensures
        bits(x | y) == bits(x).union(bits(y)),
        bits(x & y) == bits(x).intersect(bits(y)),
        bits(x & !y) == bits(x).difference(bits(y)),
{
    assert forall|b: u64| b < 64 implies
        (bit(x | y, b) == (bit(x, b) || bit(y, b))
        && bit(x & y, b) == (bit(x, b) && bit(y, b))
        && bit(x & !y, b) == (bit(x, b) && !bit(y, b))) by {
        lemma_bit_ops(x, y, b);
        lemma_bit_ops(x, !y, b);
        lemma_bit_ops(y, 0u64, b);
    }
    assert(bits(x | y) =~= bits(x).union(bits(y)));
    assert(bits(x & y) =~= bits(x).intersect(bits(y)));
    assert(bits(x & !y) =~= bits(x).difference(bits(y)));
}

/// A word with no set bit is zero.
proof fn lemma_zero_of_no_bits(x: u64)
    requires forall|b: u64| b < 64 ==> !#[trigger] bit(x, b),
    ensures x == 0u64,
{
    assert(!bit(x, 0u64));
    assert(!bit(x, 1u64));
    assert(!bit(x, 2u64));
    assert(!bit(x, 3u64));
    assert(!bit(x, 4u64));
    assert(!bit(x, 5u64));
    assert(!bit(x, 6u64));
    assert(!bit(x, 7u64));
    assert(!bit(x, 8u64));
    assert(!bit(x, 9u64));
    assert(!bit(x, 10u64));
    assert(!bit(x, 11u64));
    assert(!bit(x, 12u64));
    assert(!bit(x, 13u64));
    assert(!bit(x, 14u64));
    assert(!bit(x, 15u64));
    assert(!bit(x, 16u64));
    assert(!bit(x, 17u64));
    assert(!bit(x, 18u64));
    assert(!bit(x, 19u64));
    assert(!bit(x, 20u64));
    assert(!bit(x, 21u64));
    assert(!bit(x, 22u64));
    assert(!bit(x, 23u64));
    assert(!bit(x, 24u64));
    assert(!bit(x, 25u64));
    assert(!bit(x, 26u64));
    assert(!bit(x, 27u64));
    assert(!bit(x, 28u64));
    assert(!bit(x, 29u64));
    assert(!bit(x, 30u64));
    assert(!bit(x, 31u64));
    assert(!bit(x, 32u64));
    assert(!bit(x, 33u64));
    assert(!bit(x, 34u64));
    assert(!bit(x, 35u64));
    assert(!bit(x, 36u64));
    assert(!bit(x, 37u64));
    assert(!bit(x, 38u64));
    assert(!bit(x, 39u64));
    assert(!bit(x, 40u64));
    assert(!bit(x, 41u64));
    assert(!bit(x, 42u64));
    assert(!bit(x, 43u64));
    assert(!bit(x, 44u64));
    assert(!bit(x, 45u64));
    assert(!bit(x, 46u64));
    assert(!bit(x, 47u64));
    assert(!bit(x, 48u64));
    assert(!bit(x, 49u64));
    assert(!bit(x, 50u64));
    assert(!bit(x, 51u64));
    assert(!bit(x, 52u64));
    assert(!bit(x, 53u64));
    assert(!bit(x, 54u64));
    assert(!bit(x, 55u64));
    assert(!bit(x, 56u64));
    assert(!bit(x, 57u64));
    assert(!bit(x, 58u64));
    assert(!bit(x, 59u64));
    assert(!bit(x, 60u64));
    assert(!bit(x, 61u64));
    assert(!bit(x, 62u64));
    assert(!bit(x, 63u64));
    assert(x == 0u64) by (bit_vector)
        requires
            (x >> 0u64) & 1u64 != 1u64,
            (x >> 1u64) & 1u64 != 1u64,
            (x >> 2u64) & 1u64 != 1u64,
            (x >> 3u64) & 1u64 != 1u64,
            (x >> 4u64) & 1u64 != 1u64,
            (x >> 5u64) & 1u64 != 1u64,
            (x >> 6u64) & 1u64 != 1u64,
            (x >> 7u64) & 1u64 != 1u64,
            (x >> 8u64) & 1u64 != 1u64,
            (x >> 9u64) & 1u64 != 1u64,
            (x >> 10u64) & 1u64 != 1u64,
            (x >> 11u64) & 1u64 != 1u64,
            (x >> 12u64) & 1u64 != 1u64,
            (x >> 13u64) & 1u64 != 1u64,
            (x >> 14u64) & 1u64 != 1u64,
            (x >> 15u64) & 1u64 != 1u64,
            (x >> 16u64) & 1u64 != 1u64,
            (x >> 17u64) & 1u64 != 1u64,
            (x >> 18u64) & 1u64 != 1u64,
            (x >> 19u64) & 1u64 != 1u64,
            (x >> 20u64) & 1u64 != 1u64,
            (x >> 21u64) & 1u64 != 1u64,
            (x >> 22u64) & 1u64 != 1u64,
            (x >> 23u64) & 1u64 != 1u64,
            (x >> 24u64) & 1u64 != 1u64,
            (x >> 25u64) & 1u64 != 1u64,
            (x >> 26u64) & 1u64 != 1u64,
            (x >> 27u64) & 1u64 != 1u64,
            (x >> 28u64) & 1u64 != 1u64,
            (x >> 29u64) & 1u64 != 1u64,
            (x >> 30u64) & 1u64 != 1u64,
            (x >> 31u64) & 1u64 != 1u64,
            (x >> 32u64) & 1u64 != 1u64,
            (x >> 33u64) & 1u64 != 1u64,
            (x >> 34u64) & 1u64 != 1u64,
            (x >> 35u64) & 1u64 != 1u64,
            (x >> 36u64) & 1u64 != 1u64,
            (x >> 37u64) & 1u64 != 1u64,
            (x >> 38u64) & 1u64 != 1u64,
            (x >> 39u64) & 1u64 != 1u64,
            (x >> 40u64) & 1u64 != 1u64,
            (x >> 41u64) & 1u64 != 1u64,
            (x >> 42u64) & 1u64 != 1u64,
            (x >> 43u64) & 1u64 != 1u64,
            (x >> 44u64) & 1u64 != 1u64,
            (x >> 45u64) & 1u64 != 1u64,
            (x >> 46u64) & 1u64 != 1u64,
            (x >> 47u64) & 1u64 != 1u64,
            (x >> 48u64) & 1u64 != 1u64,
            (x >> 49u64) & 1u64 != 1u64,
            (x >> 50u64) & 1u64 != 1u64,
            (x >> 51u64) & 1u64 != 1u64,
            (x >> 52u64) & 1u64 != 1u64,
            (x >> 53u64) & 1u64 != 1u64,
            (x >> 54u64) & 1u64 != 1u64,
            (x >> 55u64) & 1u64 != 1u64,
            (x >> 56u64) & 1u64 != 1u64,
            (x >> 57u64) & 1u64 != 1u64,
            (x >> 58u64) & 1u64 != 1u64,
            (x >> 59u64) & 1u64 != 1u64,
            (x >> 60u64) & 1u64 != 1u64,
            (x >> 61u64) & 1u64 != 1u64,
            (x >> 62u64) & 1u64 != 1u64,
            (x >> 63u64) & 1u64 != 1u64,
    ;
}

/// `x & y == y` exactly when every bit of `y` is a bit of `x`.
proof fn lemma_and_eq_of_subset(x: u64, y: u64)
    requires bits(y).subset_of(bits(x)),
    ensures x & y == y,
{
    let d = (x & y) ^ y;
    assert forall|b: u64| b < 64 implies !#[trigger] bit(d, b) by {
        lemma_bit_ops(x, y, b);
        assert(bits(y).contains(b) ==> bits(x).contains(b));
        assert((((x & y) ^ y) >> b) & 1u64 == 1u64 <==> ((((x & y) >> b) & 1u64 == 1u64) != ((y >> b) & 1u64 == 1u64))) by (bit_vector)
            requires b < 64;
    }
    lemma_zero_of_no_bits(d);
    assert((x & y) ^ y == 0u64 ==> x & y == y) by (bit_vector);
}

/// No bit of `x` outside `y` means `x`'s slots are a subset of `y`'s.
proof fn lemma_subset_of_and_not_zero(x: u64, y: u64)
    requires x & !y == 0u64,
    ensures bits(x).subset_of(bits(y)),
{
    assert forall|b: u64| bits(x).contains(b) implies bits(y).contains(b) by {
        assert(((x & !y) >> b) & 1u64 != 1u64) by (bit_vector)
            requires x & !y == 0u64, b < 64;
        lemma_bit_ops(x, !y, b);
        lemma_bit_ops(y, 0u64, b);
    }
}

/// `lowest_n_bits` in `src/bitmap.rs`, trusted here: its result is a
/// subset of `m`, which is all `claim_word` relies on.
#[verifier::external_body]
pub fn lowest_n_bits(m: u64, n: usize) -> (r: u64)
    ensures r & !m == 0u64,
{
    if n >= 64 {
        return m;
    }
    let mut m = m;
    let mut have = m.count_ones() as usize;
    while have > n {
        m &= !(1u64 << (63 - m.leading_zeros()));
        have -= 1;
    }
    m
}

struct_with_invariants!{
    /// One word of a slab's slot bitmap: set bit = slot not available.
    pub struct SlotWord {
        pub atomic: AtomicU64<_, GhostSubset<u64>, _>,
        pub mask: u64,
        pub loc: Ghost<Loc>,
    }

    pub open spec fn wf(self) -> bool {
        invariant on atomic with (mask, loc) is (v: u64, g: GhostSubset<u64>) {
            g.id() == loc@ && g@ == bits(!v & mask)
        }
    }
}

impl SlotWord {
    /// A fresh word with every slot of `mask` clear, and the authority the
    /// slot tokens come from.
    pub fn new(mask: u64) -> (r: (SlotWord, Tracked<GhostSetAuth<u64>>))
        ensures r.0.wf(), r.0.mask == mask, r.1@.id() == r.0.loc@,
    {
        let tracked (auth, sub) = GhostSetAuth::<u64>::new(bits(mask));
        let ghost loc = sub.id();
        proof {
            assert(!(!mask) & mask == mask) by (bit_vector);
        }
        let atomic = AtomicU64::new(Ghost((mask, Ghost(loc))), !mask, Tracked(sub));
        (SlotWord { atomic, mask, loc: Ghost(loc) }, Tracked(auth))
    }

    /// `AtomicBitmap::claim_word` (`shift == 0`). Returns the slots this
    /// call won and their tokens.
    pub fn claim_word(&self, n: usize) -> (r: (u64, Tracked<GhostSubset<u64>>))
        requires self.wf(),
        ensures r.1@.id() == self.loc@, r.1@@ == bits(r.0), r.0 & !self.mask == 0u64,
    {
        let mask = self.mask;
        let cur = atomic_with_ghost!(&self.atomic => load(); ghost g => {});
        let want = lowest_n_bits(!cur & mask, n);
        let tracked mut tok = GhostSubset::<u64>::dummy();
        if want == 0 {
            // A zero mask owns no slots: the empty subset of this word's
            // authority.
            atomic_with_ghost!(&self.atomic => load(); ghost g => {
                tok = g.empty();
            });
            proof {
                assert forall|b: u64| b < 64 implies !#[trigger] bit(0u64, b) by {
                    assert((0u64 >> b) & 1u64 != 1u64) by (bit_vector);
                }
                assert(bits(0u64) =~= Set::<u64>::empty());
                assert(0u64 & !mask == 0u64) by (bit_vector);
            }
            return (0, Tracked(tok));
        }
        proof {
            assert(want & !mask == 0u64) by (bit_vector)
                requires want & !(!cur & mask) == 0u64;
        }
        let prev = atomic_with_ghost!(&self.atomic => fetch_or(want);
            update prev -> next;
            ghost g => {
                let won = want & !prev;
                // The slots won were clear before: they are in the word's
                // tokens.
                assert(won & !(!prev & mask) == 0u64) by (bit_vector)
                    requires won == want & !prev, want & !mask == 0u64;
                lemma_subset_of_and_not_zero(won, !prev & mask);
                // What is left is the clear bits after the `fetch_or`.
                assert(!next & mask == (!prev & mask) & !won) by (bit_vector)
                    requires next == prev | want, won == want & !prev;
                lemma_bits_ops(!prev & mask, won);
                tok = g.split(bits(won));
            }
        );
        let won = want & !prev;
        proof {
            assert(won & !mask == 0u64) by (bit_vector)
                requires won == want & !prev, want & !mask == 0u64;
        }
        (won, Tracked(tok))
    }

    /// `AtomicBitmap::release` (`shift == 0`). Ensures that the "released
    /// twice" check the Rust makes (`prev & mask == mask`) always passes.
    pub fn release(&self, m: u64, Tracked(tok): Tracked<GhostSubset<u64>>) -> (prev: u64)
        requires self.wf(), tok.id() == self.loc@, tok@ == bits(m), m & !self.mask == 0u64,
        ensures prev & m == m,
    {
        let mask = self.mask;
        let tracked mut tok = tok;
        let prev = atomic_with_ghost!(&self.atomic => fetch_and(!m);
            update prev -> next;
            ghost g => {
                // Tokens are unique: none of m's slots is clear in the word.
                tok.disjoint(&g);
                lemma_subset_of_and_not_zero(m, mask);
                assert forall|b: u64| bits(m).contains(b) implies bits(prev).contains(b) by {
                    lemma_bit_ops(!prev, mask, b);
                    lemma_bit_ops(prev, 0u64, b);
                    assert(!g@.contains(b));
                }
                lemma_and_eq_of_subset(prev, m);
                // Giving the tokens back leaves the clear bits after the
                // `fetch_and`.
                assert(!next & mask == (!prev & mask) | m) by (bit_vector)
                    requires next == prev & !m, m & !mask == 0u64;
                lemma_bits_ops(!prev & mask, m);
                g.combine(tok);
            }
        );
        prev
    }
}

fn main() {
    let (w, Tracked(_auth)) = SlotWord::new(!0u64);
    let (got, tok) = w.claim_word(3);
    if got != 0 {
        let _ = w.release(got, tok);
    }
}

} // verus!
