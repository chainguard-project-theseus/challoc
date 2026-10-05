# Seam audit: library and control

Adversarial audit of the seams of `verus/SEAMS.md` in four categories:
core library, function pointer, panic/abort and termination (153 seams,
108 of them discharged by Kani), on base 8635213. Each seam was read against
its body or the `core` documentation, its Kani record and harness were
checked against the contract, and the generic and edge-case axioms were
instantiated in scratch proofs (`#[cfg(verus_only)]`, since removed) to
look for a derivation of `false`.

Verdicts: 148 sound, 5 fixed (3 unsound contracts, S46, S329 and S330,
and the 2 contracts the S46 fix touches, S56 and S152), 0 unsound-open,
0 removed, 0 weakened.

| seam | item | category | verdict | reason |
|---|---|---|---|---|
| S2 | `call_throw_bad_alloc` | function pointer | sound | `-> !` with no ensures; the body transmutes a non-null address and calls it, and `__throw_bad_alloc` never returns (see note on unwinding) |
| S3 | `Handler` | function pointer | sound | opaque type: a function pointer Verus cannot name; nothing claimed |
| S4 | `installed_handler` | function pointer | sound | no ensures; reads the handler (an `Option` either way) |
| S5 | `call_handler` | function pointer | sound | no ensures; the handler may throw, which is not a return |
| S6 | `oom` | termination | sound | termination only (handler loop ends when the program's handler frees memory, is removed or throws); body verified |
| S9 | `Reader` | function pointer | sound | opaque type for a function pointer; nothing claimed |
| S10 | `call` | function pointer | sound | no ensures on the reading; calls the reader |
| S11 | `root` | function pointer | sound | `ROOT` is an immutable static; Kani checks `r@ == root_nodes()` (record OK) |
| S27 | `init_slow` | termination | sound | termination only (waits for the initialising thread); body verified |
| S30 | `register_fork_handlers` | function pointer | sound | no ensures; aborts if `pthread_atfork` fails |
| S37 | `Target` | function pointer | sound | opaque type for a function pointer; nothing claimed |
| S42 | `word` | core library | sound | no ensures (`&CStr` to `&str`); Kani checks it returns |
| S43 | `hist_line` | function pointer | sound | no ensures; formats through a writer |
| S44 | `file_target` | function pointer | sound | no ensures; builds the enum |
| S45 | `stderr_target` | function pointer | sound | no ensures; builds the enum |
| S46 | `Arena::axiom_array_layout` | core library | fixed | generic array-layout axiom proved `false` at `[u64; usize::MAX]` with vstd's `layout_for_type_is_valid`; now `requires N * size_of::<T>() <= isize::MAX` |
| S48 | `A_INDEX` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S49 | `A_DECAY_NS` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S50 | `A_SETS` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S51 | `A_SETS_PERM` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S52 | `A_CLASSES` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S53 | `A_LARGE` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S54 | `A_STATS` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S55 | `A_LAST_EXIT_PURGE` | core library | sound | `offset_of!` value, opaque; facts only through S56 (Kani and the build's `const` assertion) |
| S56 | `axiom_arena_layout` | core library | fixed | changed with S46 (a true clause added, ghost-only): adds `NCLASSES * size_of::<ClassState>() <= isize::MAX`, which S46's new `requires` needs at `Arena::split`; checked by the build's `const` assertion and the Kani harness |
| S65 | `PartialEqSpecImpl for State` | core library | sound | derived `PartialEq` on a fieldless enum: equality is structural |
| S72 | `ClassState::take_checks` | panic/abort | sound | requires only, no ensures; two `debug_assert_eq!`s (an abort if they fire); since verified (`debug_assert!`s over the descriptor permission), no longer a seam |
| S74 | `ClassState::claim_with` | termination | sound | termination only (CAS retry loop under contention); body verified |
| S93 | `DebugSpecImpl for Says` | core library | sound | `fmt_req` true; no ensures on output |
| S96 | `Config::set_listed` | function pointer | sound | `listed` is uninterpreted and every parser keeps `wf` under `wf` (Kani; record OK) |
| S97 | `Config::get` | function pointer | sound | `r is Some <==> listed(key@)`, the same `Key::find` as S96 (Kani; record OK) |
| S98 | `KEYS` | function pointer | sound | every `KEYS` entry is a `TABLE` name (Kani; record OK) |
| S99 | `DisplaySpecImpl for Issue<'_>` | core library | sound | `fmt_req` true; no ensures on output |
| S100 | `DisplaySpecImpl for Bytes<'_>` | core library | sound | `fmt_req` true; no ensures on output |
| S101 | `parse_from` | function pointer | sound | exactly `Config::parse`'s verified contract; the body is one call to it (the closure capture is what Verus cannot model) |
| S110 | `group_by_slab` | core library | sound | (one case unverified) only `len` and multiset kept: every write in `flag_sort`/`sort_unstable` is a swap or a write-back of the carried element; the 65536-entry case runs `flag_sort` outside its verified bound (documented at the seam) |
| S116 | `assert_flushed` | panic/abort | sound | `assert_eq!` (not debug-only): returns only with `*len == 0` (Kani; record OK) |
| S117 | `assert_refill_empty` | panic/abort | sound | as S116 |
| S127 | `assert_not_home` | panic/abort | sound | requires only; `assert_ne!` (Kani; record OK) |
| S131 | `debug_scheduled` | panic/abort | sound | no contract; a `debug_assert!` |
| S133 | `ThreadKey::create` | function pointer | sound | no ensures; `valid` records the result |
| S136 | `S_MULTI_ARENA` | core library | sound | `offset_of!` value, opaque; facts only through S152 (Kani and the build's `const` assertion) |
| S137 | `S_PUSH_FLAGS` | core library | sound | as S136 |
| S138 | `S_REGIONS` | core library | sound | as S136 |
| S139 | `S_GUARD_TEST` | core library | sound | as S136 |
| S140 | `S_TABLES` | core library | sound | as S136 |
| S141 | `S_ARENA0` | core library | sound | as S136 |
| S142 | `S_META` | core library | sound | as S136 |
| S143 | `S_HUGE` | core library | sound | as S136 |
| S144 | `S_GC_INTERVAL_NS` | core library | sound | as S136 |
| S145 | `S_POLICY` | core library | sound | as S136 |
| S146 | `S_CONFIG` | core library | sound | as S136 |
| S147 | `S_KEY` | core library | sound | as S136 |
| S148 | `S_ARENAS` | core library | sound | as S136 |
| S149 | `S_TCACHES` | core library | sound | as S136 |
| S150 | `S_STATS` | core library | sound | as S136 |
| S151 | `S_CGROUP` | core library | sound | as S136 |
| S152 | `axiom_shared_layout` | core library | fixed | changed with S46 (true clauses added, ghost-only): adds `NCLASSES * size_of::<GuardTest>()` and `<SlabTable>` `<= isize::MAX` for S46's new `requires` at `Shared::split`; checked by the build's `const` assertion and the Kani harness |
| S163 | `not_a_run_page` | panic/abort | sound | `-> !` only; the build's `not_a_run(self.addr(page))` at the same site never returns either |
| S174 | `word` | core library | sound | requires `i < NCLASSES + 3`, no ensures (Kani; record OK) |
| S176 | `RawLock::lock_slow` | termination | sound | termination only (waits for the holder's release); body verified |
| S185 | `empty_bytes` | panic/abort | sound | no contract ("callers rely on nothing but the type"); a debug overflow aborts |
| S187 | `total_bytes` | core library | sound | requires the sum fits, no ensures (Kani; record OK) |
| S195 | `main` | termination | sound | termination only (the background thread's loop); body verified |
| S203 | `fig_add` | core library | sound | exact result only when the sum fits (debug panics, release wraps) (Kani; record OK) |
| S204 | `fig_mul` | core library | sound | as S203, product |
| S205 | `fig_add64` | core library | sound | as S203, `u64` |
| S209 | `scan_lines` | termination | sound | termination only (reads until `read` returns `None`/0 or `on_line` stops); body verified |
| S210 | `DisplaySpecImpl for SizeSet` | core library | sound | `fmt_req` true; no ensures on output |
| S211 | `DisplaySpecImpl for LgSize` | core library | sound | `fmt_req` is `self.0 < 64`, which keeps every shift in range (Kani; record OK) |
| S212 | `DisplaySpecImpl for Value` | core library | sound | `fmt_req` true; no ensures on output |
| S213 | `DisplaySpecImpl for Features` | core library | sound | `fmt_req` true; no ensures on output |
| S214 | `Features::fmt` | function pointer | sound | no ensures; formats the words |
| S216 | `getrandom_exact` | termination | sound | termination only (retries short reads and `EINTR`); body verified |
| S218 | `read_full` | termination | sound | termination only (retries `EINTR`); body verified |
| S220 | `write_all` | termination | sound | termination only (retries short writes and `EINTR`); body verified |
| S262 | `check` | panic/abort | sound | `assert!` (not debug-only): returns only if `c` (Kani with `should_panic`; record OK) |
| S264 | `core::mem::drop` | core library | sound | no ensures; runs the destructor |
| S266 | `core::ffi::CStr` | core library | sound | opaque type; nothing claimed |
| S268 | `core::ffi::CStr::as_ptr` | core library | sound | no ensures |
| S269 | `char::to_digit` | core library | sound | `to_digit_spec` matches `core` for every `char` and radix in `2..=36` (Kani exhaustive; record OK) |
| S270 | `i64::wrapping_neg` | core library | sound | `i64::MIN` is its own negation (Kani exhaustive) |
| S271 | `usize::ilog2` | core library | sound | `r < 64`, `2^r <= x < 2^(r+1)` for `x > 0` (Kani exhaustive) |
| S272 | `usize::trailing_zeros` | core library | sound | vstd's recursion, 64 for 0 (Kani exhaustive) |
| S273 | `usize::leading_zeros` | core library | sound | vstd's `u64_leading_zeros` (Kani exhaustive) |
| S274 | `u64::count_ones` | core library | sound | `popcount` (Kani exhaustive) |
| S275 | `usize::count_ones` | core library | sound | as S274 |
| S276 | `usize::is_power_of_two` | core library | sound | powers of two below `2^64` (Kani exhaustive) |
| S277 | `u32::is_power_of_two` | core library | sound | powers of two below `2^32` (Kani exhaustive) |
| S278 | `usize::next_power_of_two` | core library | sound | `x <= 2^63`: `r = 2^63` at the top, `1` for `0` and `1` (Kani exhaustive) |
| S279 | `usize::div_ceil` | core library | sound | `(x + d - 1) / d` in `int` is the ceiling for `d > 0`; core never overflows here (trusted, no record) |
| S280 | `u32::rotate_right` | core library | sound | `rotr32` reduces `n` mod 32 (Kani exhaustive) |
| S281 | `<[T]>::get_unchecked::<I>` | core library | sound | `index`'s own vstd contract (requires in bounds) (Kani; record OK) |
| S282 | `core::hint::spin_loop` | core library | sound | no contract |
| S285 | `ExWrite` | core library | sound | trait methods without contracts |
| S286 | `core::fmt::Formatter::<'a>::write_str` | core library | sound | no ensures |
| S287 | `core::fmt::Formatter::<'a>::write_fmt` | function pointer | sound | no ensures |
| S288 | `DisplaySpecImpl for sys::Errno` | core library | sound | `fmt_req` true; no ensures on output |
| S289 | `DebugSpecImpl for slab::State` | core library | sound | `fmt_req` true |
| S290 | `DebugSpecImpl for huge::RunState` | core library | sound | `fmt_req` true |
| S291 | `core::ffi::c_void` | core library | sound | opaque type |
| S292 | `Option::<T>::or_else` | core library | sound | `option` when `Some`, else `f`'s result (Kani) |
| S295 | `Layout` | core library | sound | opaque type; facts through `layout_size`/`layout_align` |
| S296 | `core::alloc::LayoutError` | core library | sound | opaque type |
| S297 | `Layout::size` | core library | sound | `layout_ok` is `core`'s `Layout` invariant (power-of-two align, `size + align - 1 <= isize::MAX`) (Kani) |
| S298 | `Layout::align` | core library | sound | as S297 |
| S299 | `Layout::from_size_align` | core library | sound | `size <= isize::MAX - (align - 1)` is exactly "rounded up to `align` fits in `isize`" (Kani) |
| S300 | `Layout::new::<T>` | core library | sound | size and align of `T`; agrees with vstd's `layout_for_type_is_valid` (Kani) |
| S305 | `PartialEqSpecImpl for config::Thp` | core library | sound | derived `PartialEq` on a fieldless enum |
| S306 | `PartialEqSpecImpl for sys::thp_host::ThpSetting` | core library | sound | as S305 |
| S307 | `PartialEqSpecImpl for huge::RunState` | core library | sound | as S305 |
| S308 | `Option::<T>::map_or` | core library | sound | `default` for `None`, `f`'s result for `Some` (Kani) |
| S309 | `str::as_ptr` | core library | sound | no ensures |
| S311 | `Option::<T>::as_deref_mut` | core library | sound | no ensures, and `final(o)` is left unconstrained (weaker than the truth) |
| S313 | `<u64>::div_ceil` | core library | sound | as S279, `u64` (trusted, no record) |
| S315 | `thp_name` | core library | sound | no ensures |
| S316 | `base_slab_sizes` | function pointer | sound | requires `hi < NCLASSES`, no ensures (Kani; record OK) |
| S317 | `DisplaySpecImpl for sys::cgroup::Cgroup` | core library | sound | `fmt_req` true; no ensures on output |
| S318 | `PartialEqSpecImpl for config::PercpuArena` | core library | sound | as S305 |
| S319 | `stats_allocated` | core library | sound | as S203 (Kani; record OK) |
| S320 | `axiom_ptr_add_addr` | core library | sound | `ptr_add` is uninterpreted, so the axiom has a model; true of `add` in bounds without wrap, ZSTs included (Kani; record OK) |
| S323 | `core::slice::from_ref` | core library | sound | one-element slice (Kani) |
| S324 | `<&'a mut [T] as core::iter::IntoIterator>::into_iter` | core library | sound | repeats vstd's `iter_mut` prophetic contract; a write through an element flows to `final(s)` (scratch probe below) |
| S325 | `<[T]>::swap` | core library | sound | the two `update`s agree when `a == b` (Kani) |
| S326 | `<[T]>::reverse` | core library | sound | (Kani) |
| S327 | `<[T]>::is_sorted` | core library | sound | no ensures |
| S328 | `<[T]>::is_sorted_by_key::<'a, F, K>` | core library | sound | no ensures |
| S329 | `<[T]>::select_nth_unstable` | core library | fixed | ensures named only `final(s)` but the returned parts reborrow `s`: a write through them let a verified caller prove a false postcondition; now states the permutation of the parts on return and `final(s)` as the parts' final values |
| S330 | `<[T]>::select_nth_unstable_by_key::<K, F>` | core library | fixed | as S329 |
| S331 | `<[T]>::sort_unstable` | core library | sound | returns `()`: `final(s)` is the sorted value, a permutation (Rust 1.81+ may panic on an inconsistent `Ord`, an abort) |
| S332 | `<[T]>::sort_unstable_by_key::<K, F>` | core library | sound | as S331 |
| S333 | `<[T]>::get_unchecked_mut::<I>` | core library | sound | `index_mut`'s vstd contract, which ties `final(slice)` to the result's final value (scratch probe below) |
| S334 | `<[T]>::swap_with_slice` | core library | sound | (Kani) |
| S335 | `<[T]>::rotate_left` | core library | sound | `s[mid..] + s[..mid]` (trusted, no record) |
| S336 | `core::array::IntoIter<T, N>` | core library | sound | opaque type; facts through `array_iter_rest` |
| S337 | `<[T; N] as core::iter::IntoIterator>::into_iter` | core library | sound | `array_iter_rest` uninterpreted; the iterator starts with the array (Kani at N = 0, 3) |
| S338 | `<core::array::IntoIter<T, N> as core::iter::Iterator>::next` | core library | sound | front element or `None` (Kani at N = 0, 3) |
| S339 | `array_get_unchecked` | core library | sound | `i < N`: the element at `i` |
| S340 | `u8::is_ascii_digit` | core library | sound | `0`..`9` (Kani exhaustive) |
| S341 | `u8::is_ascii_whitespace` | core library | sound | space, tab, LF, FF, CR (Kani exhaustive) |
| S342 | `isize::wrapping_neg` | core library | sound | `isize::MIN` is its own negation (Kani exhaustive) |
| S343 | `<[T]>::as_mut_ptr` | core library | sound | no ensures |
| S344 | `<[T]>::as_ptr` | core library | sound | no ensures |
| S345 | `Result::<T, E>::map_or` | core library | sound | (Kani) |
| S346 | `Option::<T>::is_some_and` | core library | sound | (Kani) |
| S347 | `Result::<T, E>::unwrap_or` | core library | sound | (Kani) |
| S348 | `u64::is_power_of_two` | core library | sound | (Kani exhaustive) |
| S349 | `core::str::Utf8Error` | core library | sound | opaque type |
| S350 | `core::str::from_utf8` | core library | sound | no ensures |
| S351 | `<core::fmt::Error as core::default::Default>::default` | core library | sound | no ensures |
| S354 | `Option::<&T>::copied` | core library | sound | (Kani) |
| S355 | `PartialEqSpecImpl for sys::cgroup::State` | core library | sound | as S305 |
| S356 | `PartialEqSpecImpl for sys::Errno` | core library | sound | derived `PartialEq` on `Errno(i32)`: structural |

## Details

### S46 `Arena::axiom_array_layout`: fixed (was unsound)

The axiom was generic over every `T` and `N` with no `requires`:

```rust
pub(crate) axiom fn axiom_array_layout<T, const N: usize>()
    ensures
        size_of::<[T; N]>() == N * size_of::<T>(),
        align_of::<[T; N]>() == align_of::<T>(),
;
```

An array of more than `isize::MAX` bytes has no layout (rustc rejects it
as too big when it lays it out), but Verus still names its `size_of`,
and vstd's `layout_for_type_is_valid::<V>()` ensures
`size_of::<V>() <= isize::MAX` for every `V`. The two together prove
`false`. Verus accepted this scratch derivation (verified, 0 errors):

```rust
fn huge_array() {
    proof { crate::arena::axiom_array_layout::<u64, 0xFFFF_FFFF_FFFF_FFFF>(); }
    vstd::layout::layout_for_type_is_valid::<[u64; 0xFFFF_FFFF_FFFF_FFFF]>();
    assert(false);
}
```

Fix: `requires N * size_of::<T>() <= isize::MAX`, which every array
with a layout meets. The same derivation now fails with "precondition
not satisfied". `split_array` now requires that product bound instead of
`size_of::<[T; N]>() <= isize::MAX`, which follows from it through the
axiom. Its three callers get the bound from S56 and S152. The Kani
record now quotes the new contract. Its harness checks the equalities
at concrete instances, all of which meet the `requires`, so it is
unchanged; it needs a rerun only because the record changed.

### S56 `axiom_arena_layout`, S152 `axiom_shared_layout`: fixed (changed with S46)

Each gains true clauses, `NCLASSES * size_of::<E>() <= isize::MAX`, for
the element type `E` of each array the build splits (`ClassState`;
`GuardTest` and `SlabTable`). These supply S46's new `requires`. They
are checked on every build by the `const` assertions next to the axioms
(the product in `u128`), and by new `assert!`s in the Kani harnesses
`arena::kani_layout::arena_layout` and
`heap::shared::kani_layout::shared_layout`. Both records now quote the
new contracts, and their Statement and Translation map the new clauses.
Kani rerun pending.

### S329 `select_nth_unstable`, S330 `select_nth_unstable_by_key`: fixed (were unsound)

The contracts constrained only `final(s)`:

```rust
    ensures
        final(s)@.len() == old(s)@.len(),
        final(s)@.to_multiset() == old(s)@.to_multiset(),
        ...
```

Both functions return three mutable reborrows of `s`: the part before
`index`, the element at it, and the rest. `final(s)` is `s`'s value when
the borrow ends, and that includes any write through those reborrows.
So a caller that writes through them gets a false fact: the permutation
claim no longer holds. Verus accepted this scratch function, whose
postcondition is false at run time (it returns 1):

```rust
fn bad_select(s: &mut [u64]) -> (r: u64)
    requires old(s)@.len() == 1, old(s)@[0] == 0,
    ensures r == 0,
{
    let (_, m, _) = s.select_nth_unstable(0);
    *m = 1;
    s[0]
}
```

The one caller in the tree (`heap/flush.rs`) discards the returned parts,
so no shipped proof used the false fact. Fix: name the result. The
permutation (length, multiset and membership) is now stated of the
parts' values on return, `r.0@ + seq![*r.1] + r.2@`, with
`r.0@.len() == index`, and
`final(s)@ == final(r.0)@ + seq![*final(r.1)] + final(r.2)@`. (A clause
`final(s)@.len() == old(s)@.len()`, true, was tried and dropped: it
pushed `Heap::flush` past its rlimit under quantum8 and aarch64; no
caller needs it.) With the
new contract `bad_select` fails ("postcondition not satisfied"). A write
through `m` is now visible: `final(s)@[0] == 1` after `*m = 1`
verifies. A discarded result still gives the permutation, because Verus
resolves dropped reborrows; the `flush.rs` caller re-verifies, as does a
probe ensuring `final(s)@[0] == 7` after discarding the parts. Neither
seam has a record. The tool (Kani) is unchanged: a harness would check
the parts against the slice's final contents.

## Probes that found nothing

- S324 (`for e in s` over `&mut [T]`) and S333 (`get_unchecked_mut`,
  at `usize` and at `..k`): a write through the element or subslice
  shows in `final(s)`. Scratch functions claiming the old value at the
  written index were rejected ("postcondition not satisfied").
- S320 `axiom_ptr_add_addr`: `ptr_add` is uninterpreted, so the axiom
  has a model (the address arithmetic itself). At `count = 0` and for
  zero-sized `T` it gives back `p`'s address, which is consistent.
- The `PartialEqSpecImpl`s (S65, S305-S307, S318, S355, S356): every
  type is a fieldless enum or `Errno(i32)` with derived `PartialEq`, so
  `eq_spec` as structural equality is exact. No type holds a raw
  pointer, where Verus equality would also compare provenance.
- S297-S300 (`Layout`): `layout_ok` is the `Layout` invariant `core`
  keeps (`max_size_for_align`), and it agrees with vstd's
  `layout_for_type_is_valid`.
- The termination seams (S6, S27, S74, S176, S195, S209, S216, S218,
  S220): Verus checks each body against its contract. Only termination
  is assumed, and each loop really can run on without bound (another
  thread's progress, a retried `EINTR`, the program's `new_handler`,
  or the background thread's lifetime).
- The abort seams: `assert_flushed`, `assert_refill_empty` and `check`
  use `assert_eq!`/`assert!`, not the debug forms, so their `ensures`
  hold in release builds too. The stand-in `not_a_run_page` replaces a
  build call that also never returns.

## Notes (not contract defects)

- S2/S5 (unwinding through `extern "C"`): `call_throw_bad_alloc` and
  `call_handler` call C++ code that throws through function pointers
  typed `unsafe extern "C" fn`. Unwinding out of a `"C"` call is
  undefined behaviour in Rust, and with `panic = "abort"` a
  `"C-unwind"` call would abort instead of propagating. The design
  relies on the unwinder passing Rust frames that have unwind tables
  (`api::c_programs` tests the `.eh_frame` coverage of `_Znwm` and the
  throw). This does not touch the Verus contracts (no return is
  claimed), but it is a decision for the coordinator.
- S110 `group_by_slab`: for a flush of a whole 65536-slot cache, its
  body runs `flag_sort` past the `u16::MAX` length that `flag_sort` is
  verified for. The seam documents this. The contract (length and
  multiset kept) still holds, because every write is a swap or the
  write-back of the carried element. A wrapped cursor could break that
  only by indexing the hole itself, and the documented analysis rules
  that out. A guard `&& high.len() <= u16::MAX as usize` on the
  `flag_sort` branch would bring the case inside verified code, but it
  changes a flush-path function, which this audit leaves to the
  coordinator.
- Record wording fixed (no contract change): the loop and unwind
  sentence copied into the records of S277, S280, S340, S341 and S342
  (no loop in four of their harnesses, 33 iterations in the fifth),
  "sum" for "product" in S204's gap, and the source lines cited by the
  headers of the records of S48-S55 (shifted by this change).
- Record wording still imprecise (left as is):
  - S292's shared closure paragraph does not describe its harness.
  - S333's write test covers only the `usize` index; the `..k` and
    `k..` instantiations the proofs use are checked by address only.
  - S174's Statement claims the byte order, but the harness checks it
    only against a copy of the body.
- S332/S330 (keyed forms): the permutation claim assumes the key
  closure leaves every element unchanged. Interior mutability in `T`
  could break that, which Verus does not model. The only instantiation
  is `*mut u8`; a `T: Copy` bound would make the assumption explicit.
- Kani: the records of S46, S56 and S152 quote the new contracts and
  keep the status "discharged". The harnesses need a rerun
  (`sh verus/kani/run.sh --harness arena::kani_layout::arena_layout
  --harness arena::kani_layout::array_layout_instances --harness
  heap::shared::kani_layout::shared_layout --harness
  heap::shared::kani_layout::shared_array_layout`), which this VM cannot
  do (no Kani).
