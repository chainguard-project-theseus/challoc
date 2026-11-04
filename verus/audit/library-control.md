# Seam audit: library and control, the audited seams still trusted at `26f535a9`

*An adversarial audit of the Verus seams in four categories: core
library, function pointer, panic/abort and termination, made during
[#138](https://github.com/wolfi-dev/challoc/pull/138). There it covered 153 seams: 148 sound and 5 fixed.*

*This file lists the 116 of those seams that the ledger at `26f535a9`
still lists. The others are verified at `26f535a9` or no longer exist
(among them `parse_from` and the `Display` spec of `sys::cgroup::Cgroup`,
now verified), so their rows are left out; seams of this scope that the
audit did not cover are not listed. Each row names the seam by its item
and its ledger file, `verus/ledger/src/<file>.md`, which gives the
seam's contract, category, tool and status.*

The verdicts are the audit's, from #138; none was re-audited at
`26f535a9`. The status column is `26f535a9`'s ledger, checked by script
against `verus/ledger/src/`. A reason may describe the contract as it
stands at `26f535a9`, but its verdict was not judged again.

The audit reads each seam against its body or the `core`
documentation, checks its Kani harness against the contract, and
instantiates the generic and edge-case axioms in scratch proofs to look
for a derivation of `false`. Verdicts: **sound** means no derivation of
`false` was found and the contract holds of the body or of `core`'s
documented behaviour; **fixed** means the audit found the contract
false or unsound and corrected it, and the reason gives the corrected
contract. Remaining seam work is tracked in
[wolfi-dev/challoc#397](https://github.com/wolfi-dev/challoc/issues/397).

Totals for the 116 seams listed: 111 sound and 5 fixed; at `26f535a9`,
84 discharged, 9 partly checked and 23 trusted. The category column is
the ledger's; a few seams of this audit sit in another category there.

| seam | ledger file | category | status | verdict | reason |
|---|---|---|---|---|---|
| `call_throw_bad_alloc` | `api/cxx.rs` | function pointer | trusted | sound | `-> !` with no `ensures`; the body transmutes a non-null address and calls it, and `__throw_bad_alloc` never returns (see "Notes"). |
| `Handler` | `api/cxx.rs` | function pointer | trusted | sound | Opaque type: a function pointer Verus cannot name; nothing claimed. |
| `call_handler` | `api/cxx.rs` | function pointer | trusted | sound | No `ensures`; the handler may throw, which is not a return. |
| `oom` | `api/cxx.rs` | termination | trusted | sound | Termination only (handler loop ends when the program's handler frees memory, is removed or throws); body verified. |
| `root` | `api/mallctl.rs` | allocator invariant | discharged | sound | `ROOT` is an immutable static; Kani checks `r@ == root_nodes()`. |
| `init_slow` | `api/mod.rs` | termination | trusted (partly checked) | sound | Termination only (waits for the initialising thread); body verified. |
| `word` | `api/print.rs` | core library | trusted (partly checked) | sound | No `ensures` (`&CStr` to `&str`); Kani checks it returns. |
| `hist_line` | `api/print.rs` | core library | discharged | sound | No `ensures`; formats through a writer. |
| `axiom_array_layout` | `arena.rs` | core library | trusted (partly checked) | fixed | Requires `N * size_of::<T>() <= isize::MAX`, which every array with a layout meets; without it the generic axiom proves `false` at `[u64; usize::MAX]` with vstd's `layout_for_type_is_valid` (see details). |
| `A_INDEX` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `A_DECAY_NS` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `A_SETS` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `A_SETS_PERM` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `A_CLASSES` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `A_LARGE` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `A_STATS` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `A_LAST_EXIT_PURGE` | `arena.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_arena_layout`, which the build's `const` assertion checks. |
| `axiom_arena_layout` | `arena.rs` | core library | discharged | fixed | Includes `NCLASSES * size_of::<ClassState>() <= isize::MAX`, which `axiom_array_layout`'s `requires` needs at `Arena::split`; the build's `const` assertion checks every clause. |
| `ClassState::claim_with` | `class/mod.rs` | termination | trusted | sound | Termination only (CAS retry loop under contention); body verified. |
| `Config::set_listed` | `config.rs` | function pointer | discharged | sound | Requires and ensures only `wf`; every parser keeps `wf`. |
| `Config::get` | `config.rs` | function pointer | trusted (partly checked) | sound | No `ensures`. |
| `KEYS` | `config.rs` | function pointer | discharged | sound | Every `KEYS` entry is a `TABLE` name. |
| `assert_flushed` | `heap/mod.rs` | panic/abort | discharged | sound | `assert_eq!` (not debug-only): returns only with `*len == 0`. |
| `assert_refill_empty` | `heap/mod.rs` | panic/abort | discharged | sound | As `assert_flushed`. |
| `assert_not_home` | `heap/others.rs` | panic/abort | discharged | sound | Requires only; `assert_ne!`. |
| `debug_scheduled` | `heap/paths.rs` | panic/abort | discharged | sound | No contract; a `debug_assert!`. |
| `ThreadKey::create` | `heap/shared.rs` | function pointer | trusted | sound | No `ensures`; `valid` records the result. |
| `S_PUSH_FLAGS` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_REGIONS` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_TABLES` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_ARENA0` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_META` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_HUGE` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_GC_INTERVAL_NS` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_POLICY` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_CONFIG` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_KEY` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_ARENAS` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_TCACHES` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_STATS` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `S_CGROUP` | `heap/shared.rs` | core library | discharged | sound | `offset_of!` value, opaque; facts only through `axiom_shared_layout`, which the build's `const` assertion checks. |
| `axiom_shared_layout` | `heap/shared.rs` | core library | discharged | fixed | Includes `NCLASSES * size_of::<ClassLine>()` and `NCLASSES * size_of::<SlabTable>()` `<= isize::MAX`, which `axiom_array_layout`'s `requires` needs at `Shared::split`; the build's `const` assertion checks every clause. |
| `word` | `layout/regions.rs` | core library | discharged | sound | Requires `i < NCLASSES + 3`, no `ensures`. |
| `RawLock::lock_slow` | `lock.rs` | termination | trusted | sound | Termination only (waits for the holder's release); body verified. |
| `main` | `purge/background.rs` | termination | trusted (partly checked) | sound | Termination only (the background thread's loop); body verified. |
| `fig_add` | `stats.rs` | core library | discharged | sound | Exact result only when the sum fits (debug panics, release wraps). |
| `fig_mul` | `stats.rs` | core library | discharged | sound | As `fig_add`, for the product. |
| `fig_add64` | `stats.rs` | core library | discharged | sound | As `fig_add`, for `u64`. |
| `scan_lines` | `sys/cgroup.rs` | termination | trusted | sound | Termination only (reads until `read` returns `None`/0 or `on_line` stops); body verified. |
| `getrandom_exact` | `sys/mod.rs` | termination | trusted | sound | Termination only (retries short reads and `EINTR`); body verified. |
| `read_full` | `sys/mod.rs` | termination | trusted | sound | Termination only (retries `EINTR`); body verified. |
| `write_all` | `sys/mod.rs` | termination | trusted | sound | Termination only (retries short writes and `EINTR`); body verified. |
| `check` | `vspec/check.rs` | panic/abort | discharged | sound | `assert!` (not debug-only): returns only if `c` (Kani checks it with `should_panic`). |
| `core::mem::drop` | `vspec/class.rs` | core library | discharged | sound | No `ensures`; runs the destructor. |
| `core::ffi::CStr` | `vspec/config.rs` | core library | trusted | sound | Opaque type; nothing claimed. |
| `core::ffi::CStr::as_ptr` | `vspec/config.rs` | core library | discharged | sound | No `ensures`. |
| `char::to_digit` | `vspec/config.rs` | core library | discharged | sound | `to_digit_spec` matches `core` for every `char` and radix in `2..=36` (Kani checks every input). |
| `i64::wrapping_neg` | `vspec/core_int.rs` | core library | discharged | sound | `i64::MIN` is its own negation (Kani checks every input). |
| `usize::ilog2` | `vspec/core_int.rs` | core library | discharged | sound | `r < 64`, `2^r <= x < 2^(r+1)` for `x > 0` (Kani checks every input). |
| `usize::trailing_zeros` | `vspec/core_int.rs` | core library | discharged | sound | Vstd's recursion, 64 for 0 (Kani checks every input). |
| `usize::leading_zeros` | `vspec/core_int.rs` | core library | discharged | sound | Vstd's `u64_leading_zeros` (Kani checks every input). |
| `u64::count_ones` | `vspec/core_int.rs` | core library | discharged | sound | `popcount` (Kani checks every input). |
| `usize::is_power_of_two` | `vspec/core_int.rs` | core library | discharged | sound | Powers of two below `2^64` (Kani checks every input). |
| `u32::is_power_of_two` | `vspec/core_int.rs` | core library | discharged | sound | Powers of two below `2^32` (Kani checks every input). |
| `usize::next_power_of_two` | `vspec/core_int.rs` | core library | discharged | sound | `x <= 2^63`: `r = 2^63` at the top, `1` for `0` and `1` (Kani checks every input). |
| `usize::div_ceil` | `vspec/core_int.rs` | core library | trusted | sound | `(x + d - 1) / d` in `int` is the ceiling for `d > 0`; core never overflows here. |
| `u32::rotate_right` | `vspec/core_int.rs` | core library | discharged | sound | `rotr32` reduces `n` mod 32 (Kani checks every input). |
| `<[T]>::get_unchecked::<I>` | `vspec/core_int.rs` | core library | discharged | sound | `index`'s own vstd contract (requires in bounds). |
| `core::hint::spin_loop` | `vspec/core_int.rs` | core library | trusted | sound | No contract. |
| `ExWrite` | `vspec/fmt.rs` | core library | trusted | sound | Trait methods without contracts. |
| `core::fmt::Formatter::<'a>::write_str` | `vspec/fmt.rs` | core library | discharged | sound | No `ensures`. |
| `core::fmt::Formatter::<'a>::write_fmt` | `vspec/fmt.rs` | core library | discharged | sound | No `ensures`. |
| `DisplaySpecImpl for sys::Errno` | `vspec/fmt.rs` | core library | discharged | sound | `fmt_req` true; no `ensures` on output. |
| `DebugSpecImpl for slab::State` | `vspec/fmt.rs` | core library | discharged | sound | `fmt_req` true. |
| `core::ffi::c_void` | `vspec/config.rs` | core library | trusted | sound | Opaque type. |
| `Option::<T>::or_else` | `vspec/misc.rs` | core library | discharged | sound | `option` when `Some`, else `f`'s result. |
| `Layout` | `vspec/layout.rs` | core library | trusted | sound | Opaque type; facts through `layout_size`/`layout_align`. |
| `core::alloc::LayoutError` | `vspec/layout.rs` | core library | trusted | sound | Opaque type. |
| `Layout::size` | `vspec/layout.rs` | core library | discharged | sound | `layout_ok` is `core`'s `Layout` invariant (power-of-two align, `size + align - 1 <= isize::MAX`). |
| `Layout::align` | `vspec/layout.rs` | core library | discharged | sound | As `Layout::size`. |
| `Layout::from_size_align` | `vspec/layout.rs` | core library | discharged | sound | `size <= isize::MAX - (align - 1)` is exactly "rounded up to `align` fits in `isize`". |
| `Layout::new::<T>` | `vspec/layout.rs` | core library | discharged | sound | Size and align of `T`; agrees with vstd's `layout_for_type_is_valid`. |
| `PartialEqSpecImpl for config::Thp` | `vspec/misc.rs` | core library | discharged | sound | Derived `PartialEq` on a fieldless enum. |
| `PartialEqSpecImpl for sys::thp_host::ThpSetting` | `vspec/misc.rs` | core library | discharged | sound | As `PartialEqSpecImpl for config::Thp`. |
| `PartialEqSpecImpl for huge::RunState` | `vspec/misc.rs` | core library | discharged | sound | As `PartialEqSpecImpl for config::Thp`. |
| `Option::<T>::map_or` | `vspec/sys.rs` | core library | discharged | sound | `default` for `None`, `f`'s result for `Some`. |
| `str::as_ptr` | `vspec/misc.rs` | core library | discharged | sound | No `ensures`. |
| `Option::<T>::as_deref_mut` | `vspec/misc.rs` | core library | discharged | sound | No `ensures`, and `final(o)` is left unconstrained (weaker than the truth). |
| `<u64>::div_ceil` | `vspec/mod.rs` | core library | trusted | sound | As `usize::div_ceil`, for `u64`. |
| `base_slab_sizes` | `vspec/print.rs` | function pointer | discharged | sound | Requires `hi < NCLASSES`, no `ensures`. |
| `PartialEqSpecImpl for config::PercpuArena` | `vspec/print.rs` | core library | discharged | sound | As `PartialEqSpecImpl for config::Thp`. |
| `axiom_ptr_add_addr` | `vspec/ptr.rs` | core library | trusted (partly checked) | sound | `ptr_add` is uninterpreted, so the axiom has a model; true of `add` in bounds without wrap, ZSTs included. |
| `core::slice::from_ref` | `vspec/slice.rs` | core library | discharged | sound | One-element slice. |
| `<&'a mut [T] as core::iter::IntoIterator>::into_iter` | `vspec/slice.rs` | core library | discharged | sound | Repeats vstd's `iter_mut` prophetic contract; a write through an element flows to `final(s)` (see "Probes"). |
| `<[T]>::swap` | `vspec/slice.rs` | core library | discharged | sound | The two `update`s agree when `a == b`. |
| `<[T]>::is_sorted` | `vspec/slice.rs` | core library | discharged | sound | No `ensures`. |
| `<[T]>::is_sorted_by_key::<'a, F, K>` | `vspec/slice.rs` | core library | discharged | sound | No `ensures`. |
| `<[T]>::select_nth_unstable` | `vspec/slice.rs` | core library | trusted | fixed | States the permutation of the returned parts on return and `final(s)` as the parts' final values, so a write through a part shows in `final(s)` (see details). |
| `<[T]>::select_nth_unstable_by_key::<K, F>` | `vspec/slice.rs` | core library | trusted | fixed | As `<[T]>::select_nth_unstable`. |
| `<[T]>::get_unchecked_mut::<I>` | `vspec/slice.rs` | core library | discharged | sound | `index_mut`'s vstd contract, which ties `final(slice)` to the result's final value (see "Probes"). |
| `<[T]>::swap_with_slice` | `vspec/slice.rs` | core library | trusted (partly checked) | sound | Equal lengths; the two slices trade values. |
| `<[T]>::rotate_left` | `vspec/slice.rs` | core library | trusted (partly checked) | sound | `s[mid..] + s[..mid]`. |
| `core::array::IntoIter<T, N>` | `vspec/slice.rs` | core library | trusted | sound | Opaque type; facts through `array_iter_rest`. |
| `<[T; N] as core::iter::IntoIterator>::into_iter` | `vspec/slice.rs` | core library | discharged | sound | `array_iter_rest` uninterpreted; the iterator starts with the array (Kani checks N = 0 and 3). |
| `<core::array::IntoIter<T, N> as core::iter::Iterator>::next` | `vspec/slice.rs` | core library | discharged | sound | Front element or `None` (Kani checks N = 0 and 3). |
| `array_get_unchecked` | `vspec/slice.rs` | core library | discharged | sound | `i < N`: the element at `i`. |
| `u8::is_ascii_digit` | `vspec/sys.rs` | core library | discharged | sound | `0`..`9` (Kani checks every input). |
| `u8::is_ascii_whitespace` | `vspec/sys.rs` | core library | discharged | sound | Space, tab, LF, FF, CR (Kani checks every input). |
| `isize::wrapping_neg` | `vspec/sys.rs` | core library | discharged | sound | `isize::MIN` is its own negation (Kani checks every input). |
| `<[T]>::as_mut_ptr` | `vspec/sys.rs` | core library | discharged | sound | No `ensures`. |
| `<[T]>::as_ptr` | `vspec/sys.rs` | core library | discharged | sound | No `ensures`. |
| `Result::<T, E>::map_or` | `vspec/sys.rs` | core library | discharged | sound | `default` for `Err`, `f`'s result for `Ok`. |
| `Result::<T, E>::unwrap_or` | `vspec/sys.rs` | core library | discharged | sound | The `Ok` value, else `default`. |
| `u64::is_power_of_two` | `vspec/core_int.rs` | core library | discharged | sound | Powers of two below `2^64` (Kani checks every input). |
| `core::str::Utf8Error` | `vspec/sys.rs` | core library | trusted | sound | Opaque type. |
| `core::str::from_utf8` | `vspec/sys.rs` | core library | trusted (partly checked) | sound | No `ensures`. |

## Details

### `axiom_array_layout`: the size bound

The axiom is generic over every `T` and `N`:

```rust
pub(crate) axiom fn axiom_array_layout<T, const N: usize>()
    requires
        N * size_of::<T>() <= isize::MAX,
    ensures
        size_of::<[T; N]>() == N * size_of::<T>(),
        align_of::<[T; N]>() == align_of::<T>(),
;
```

An array of more than `isize::MAX` bytes has no layout (rustc rejects it
as too big when it lays it out), but Verus still names its `size_of`,
and vstd's `layout_for_type_is_valid::<V>()` ensures
`size_of::<V>() <= isize::MAX` for every `V`. Without the `requires`, the
two together prove `false`:

```rust
fn huge_array() {
    proof { crate::arena::axiom_array_layout::<u64, 0xFFFF_FFFF_FFFF_FFFF>(); }
    vstd::layout::layout_for_type_is_valid::<[u64; 0xFFFF_FFFF_FFFF_FFFF]>();
    assert(false);
}
```

With the `requires`, this derivation fails with "precondition not
satisfied". `split_array` requires the same product bound, and its
callers get it from `axiom_arena_layout` (`ClassState`) and
`axiom_shared_layout` (`ClassLine` and `SlabTable`), whose clauses the
build's `const` assertions check (the product in `u128`). The seam is
partly checked: the `const` assertions cover the five instances the
proofs use, and a Kani harness covers sampled ones; the axiom itself is
generic.

### `select_nth_unstable` and `select_nth_unstable_by_key`: the returned parts

Both functions return three mutable reborrows of `s`: the part before
`index`, the element at it, and the rest. `final(s)` is `s`'s value when
the borrow ends, and that includes any write through those reborrows. A
contract that stated the permutation of `final(s)` alone would hand a
caller that writes through a part a false fact. This scratch function's
postcondition is false at run time (it returns 1), and a contract over
`final(s)` alone accepts it:

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

The contract states the permutation (length, multiset and membership)
of the parts' values on return, `r.0@ + seq![*r.1] + r.2@`, with
`r.0@.len() == index`, and
`final(s)@ == final(r.0)@ + seq![*final(r.1)] + final(r.2)@`. Under it
`bad_select` fails ("postcondition not satisfied"), and a write through
`m` is visible: `final(s)@[0] == 1` after `*m = 1` verifies. A discarded
result still gives the permutation, because Verus resolves dropped
reborrows. The contract leaves out `final(s)@.len() == old(s)@.len()`,
which is true but which no caller needs.

## Probes

These probes found no defect:

- `<&'a mut [T] as core::iter::IntoIterator>::into_iter` (`for e in s`
  over `&mut [T]`) and `<[T]>::get_unchecked_mut::<I>` (at `usize` and
  at `..k`): a write through the element or subslice shows in
  `final(s)`. Scratch functions that claim the old value at the written
  index fail to verify ("postcondition not satisfied").
- `axiom_ptr_add_addr`: `ptr_add` is uninterpreted, so the axiom has a
  model (the address arithmetic itself). At `count = 0` and for
  zero-sized `T` it gives back `p`'s address, which is consistent.
- The `PartialEqSpecImpl`s: every type is a fieldless enum with derived
  `PartialEq`, so `eq_spec` as structural equality is exact. No type
  holds a raw pointer, where Verus equality would also compare
  provenance.
- `Layout::size`, `Layout::align`, `Layout::from_size_align` and
  `Layout::new::<T>`: `layout_ok` is the `Layout` invariant `core` keeps
  (`max_size_for_align`), and it agrees with vstd's
  `layout_for_type_is_valid`.
- The termination seams (`oom`, `init_slow`, `ClassState::claim_with`,
  `RawLock::lock_slow`, the background thread's `main`, `scan_lines`,
  `getrandom_exact`, `read_full` and `write_all`): Verus checks each body
  against its contract. Only termination is assumed, and each loop can
  run on without bound (another thread's progress, a retried `EINTR`,
  the program's `new_handler`, or the background thread's lifetime).
- The abort seams: `assert_flushed`, `assert_refill_empty` and `check`
  use `assert_eq!` and `assert!`, not the debug forms, so their
  `ensures` hold in release builds too.

## Notes

- **Unwinding through `extern "C"`.** `call_throw_bad_alloc` and
  `call_handler` call C++ code that throws through function pointers
  typed `unsafe extern "C" fn`. Unwinding out of a `"C"` call is
  undefined behaviour in Rust, and with `panic = "abort"` a
  `"C-unwind"` call would abort instead of propagating. The design
  relies on the unwinder passing Rust frames that have unwind tables;
  `api::c_programs` tests the `.eh_frame` coverage of `_Znwm` and the
  throw. The Verus contracts claim no return, so this does not touch
  them.
- **Keyed forms.** The permutation claims of
  `<[T]>::select_nth_unstable_by_key::<K, F>` assume the key closure
  leaves every element unchanged. Interior mutability in `T` could break
  that, which Verus does not model. The only instantiation is
  `*mut u8`.
