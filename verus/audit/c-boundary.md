# Seam audit: the C and kernel boundary, the audited seams still trusted at `26f535a9`

*An adversarial audit of the Verus seams in five categories: inline asm,
FFI/syscall, platform/kernel fact, C-program contract and raw pointer from
C caller, made during [#138](https://github.com/wolfi-dev/challoc/pull/138). There it covered 76 seams: 68 sound,
7 fixed, 0 weakened, 1 removed and 0 unsound-open.*

*This file lists the 51 of those seams that the ledger at `26f535a9`
still lists. The others are verified at `26f535a9` or no longer exist
(among them `api::print::emit` and `mallctl`'s `store` and `load`, now
verified), so their rows are left out; seams of this scope that the
audit did not cover are not listed. Each row names the seam by its item
and its ledger file, `verus/ledger/src/<file>.md`, which gives the
seam's contract, category, tool and status.*

The verdicts are the audit's, from #138; none was re-audited at
`26f535a9`. A reason may describe the contract as it stands at
`26f535a9`, but its verdict was not judged again.

For each seam the audit asks four questions. Can it prove `false`? Does
its body meet its contract for every input its `requires` admits? Is it
still needed? Does it claim more than its callers use? The seams that
Isla checks are read together with their entries in
`verus/isla/seams.md`.

Verdicts: **sound** means no defect was found; a premise that the seam
names is not a defect. **fixed** means the audit found the contract
false or unsound and corrected it; the reason gives the corrected
contract. Remaining seam work is tracked in
[wolfi-dev/challoc#397](https://github.com/wolfi-dev/challoc/issues/397).

Totals for the 52 seams listed: 50 sound, 2 fixed.

## Verdicts

| seam | ledger file | verdict | reason |
|---|---|---|---|
| `throw_bad_alloc_addr` | `api/cxx.rs` | sound | GOT load of a weak symbol (null when undefined); ensures only `is_throw_bad_alloc(r)`, an uninterpreted name for that address. |
| `axiom_fork_resumed_state` | `api/mod.rs` | sound | Mints a `StateHold`, which carries the state lock's cell `PointsTo`, so two calls give two permissions of one cell, which vstd's `PointsTo::is_exclusive` makes contradict (see "Notes"); `get_unlocked` is gone. It asserts `lock_inv` of the held value, which `axiom_held` used to assume at the read, and that the hold is of the lock the handler passes (`purge::background::state`'s `&STATE`, required to be of rank `rank_state`). |
| `axiom_fork_resumed` | `api/mod.rs` | fixed | Requires the state hold to be the state lock's (`state.wf()`; see details). |
| `read_slot` | `config.rs` | sound | Claims nothing. |
| `malloc_conf_addr` | `config.rs` | sound | GOT load; claims nothing. |
| `je_malloc_conf_addr` | `config.rs` | sound | As `malloc_conf_addr`, for a weak symbol. |
| `single_threaded_addr` | `purge/background.rs` | sound | As `je_malloc_conf_addr`, for glibc's weak `__libc_single_threaded`. |
| `byte_at` | `config.rs` | sound | Claims nothing (also the read of `__libc_single_threaded`). |
| `bytes_at` | `config.rs` | sound | `r@.len() == n` is what `from_raw_parts` returns. |
| `env_var` | `config.rs` | sound | The value is a non-empty suffix of an `environ` entry, cut by `cstr` at `MAX_TEXT`; requires and keeps `!secure_exec()`. |
| `axiom_c_claim` | `heap/live.rs` | sound | The C contract of `free`, once per entry point (and per branch of the verification-only fast-path theorem); a double free makes the proof vacuous, as documented. |
| `pthread_setspecific` | `heap/shared.rs` | sound | The re-entry premise is named; `requires reentry_benign` is checked, and the TLS cell is unchanged. |
| `Reserved` | `layout/regions.rs` | sound | Opaque, non-`Copy`; minted only by `try_reserve`. |
| `Reserved::is_apart_from_static` | `layout/regions.rs` | sound | `mmap` without `MAP_FIXED` never overlaps a live mapping; requires a non-empty token. |
| `Reserved::is_disjoint` | `layout/regions.rs` | sound | `&mut self` and `&Self` cannot be one token; two live mappings are apart. |
| `Reserved::into_unbacked` | `layout/regions.rs` | sound | Consumes the token, so a range is handed out once. |
| `try_reserve` | `layout/regions.rs` | sound | A successful hint-free `mmap` is above 0 and inside the default window. |
| `pthread_join` | `purge/background.rs` | sound | Claims nothing about the result; keeps the TLS cell (`kept`). |
| `spawn` | `purge/background.rs` | fixed | Requires `shared.wf()`, which the thread's `shared_arg` ensures of the `Shared` it is passed (see details). |
| `symbols` | `rseq.rs` | sound | GOT loads of weak symbols; claims nothing. |
| `thread_pointer` | `rseq.rs` | sound | Claims nothing. |
| `getauxval#1` | `sys/auxv.rs` | sound | x86_64: `AT_PAGESZ` is 4096 on Linux, a power of two at most 64 KiB, and is the process's page; `AT_SECURE` is `at_secure()`. |
| `getauxval#2` | `sys/auxv.rs` | sound | aarch64: pages of 4, 16 or 64 KiB; `HWCAP_PACA` and `HWCAP_PACG` cover the data keys `pacda` and `autda` use. |
| `trap` | `sys/mod.rs` | sound | `ud2` and `brk` never fall through. |
| `axiom_user_va_limit` | `sys/pac.rs` | sound | 47-bit window on x86_64, 48-bit on arm64 without `FORCE_52BIT`. |
| `axiom_auth_sign` | `sys/pac.rs` | sound | Below the VA limit bit 55 is clear, and `autda` restores the PAC bits from it. |
| `sign` | `sys/pac.rs` | sound | A function of `(p, modifier)` under the key premise named in `spec_sign`. |
| `auth` | `sys/pac.rs` | sound | As `sign`; a failed check faults or yields a pointer that faults. |
| `strip` | `sys/pac.rs` | sound | Claims nothing. |
| `syscall0#1` | `sys/raw.rs` | sound | x86_64: `rcx` and `r11` clobbered, memory not `nomem`; claims nothing. |
| `syscall1#1` | `sys/raw.rs` | sound | As `syscall0#1`. |
| `syscall2#1` | `sys/raw.rs` | sound | As `syscall0#1`. |
| `syscall3#1` | `sys/raw.rs` | sound | As `syscall0#1`. |
| `syscall4#1` | `sys/raw.rs` | sound | As `syscall0#1`, with `r10` for the fourth argument. |
| `syscall5#1` | `sys/raw.rs` | sound | As `syscall4#1`. |
| `syscall6#1` | `sys/raw.rs` | sound | As `syscall4#1`. |
| `syscall0#2` | `sys/raw.rs` | sound | aarch64: `svc 0`, x0 out; the kernel preserves the rest. |
| `syscall1#2` | `sys/raw.rs` | sound | As `syscall0#2`. |
| `syscall2#2` | `sys/raw.rs` | sound | As `syscall0#2`. |
| `syscall3#2` | `sys/raw.rs` | sound | As `syscall0#2`. |
| `syscall4#2` | `sys/raw.rs` | sound | As `syscall0#2`. |
| `syscall5#2` | `sys/raw.rs` | sound | As `syscall0#2`. |
| `syscall6#2` | `sys/raw.rs` | sound | As `syscall0#2`. |
| `get` | `sys/tls.rs` | sound | Returns the TLS cell's value; rests on the premise that a cell token's value is the slot's. |
| `get_tp` | `sys/tls.rs` | sound | As `get`, with the thread pointer (`tp()`). |
| `set` | `sys/tls.rs` | sound | Requires `slot_step` from the old value; as `get`, the cell's value is the slot's. |
| `lend` | `vspec/errno.rs` | sound | No code; the token's value is unknown. |
| `set_errno` | `vspec/errno.rs` | sound | Writes `errno` through `__errno_location`. |
| `libc::FILE` | `vspec/print.rs` | sound | An opaque type. |
| `axiom_fresh_anon_zero` | `huge/mod.rs` | sound | `mmap(2)`; the premise is named, not checked (see "Notes"). |
| `axiom_discarded_zero` | `huge/mod.rs` | sound | `madvise(2)` with `MADV_DONTNEED`; the call's success is required (`sys::madvise_ok`), and that nothing has written the bytes since is named, not checked (see "Notes"). |
| `Unbacked::commit` | `vspec/unbacked.rs` | sound | `mprotect(2)` or `mmap(2)`; the premise is named, not checked (see "Notes"). |

## Details

### `axiom_fork_resumed`: the state hold is required

`fork_holds` includes `state.wf()`: the hold is the token of a lock of
the state lock's rank (`purge::background::StateHold::lock`) with the
permission of its cell and
a value that keeps the lock invariant. An axiom that ensured
`fork_holds(*s, held, *state)` for any `state` would state a conclusion
about a hold the caller already has, and this scratch proof would verify:

```rust
proof fn scratch_fork_resumed_false(s: &Shared, slot: &AtomicU64, tracked h: &StateHold)
    requires
        !h.wf(),
    ensures
        false,
{
    let tracked held = axiom_fork_resumed(s, slot, h);
}
```

The axiom requires `state.wf()` instead, and its one call site,
`fork_release`, requires it of its `state`. At `26f535a9` the hold was a
`LockHeld` and the requirement `state.held() == state_lock()`, for the
same reason.

### `purge::background::spawn`: `shared.wf()`

`spawn(shared, ..)` hands `shared` to the new thread, whose `shared_arg`
ensures `r.wf()` of it. `spawn` requires `shared.wf()`, so a verified
caller cannot make the thread's code assume `wf()` of an arbitrary
`Shared`; its one caller, `create`, requires it as well. The thread also
takes the reference as `&'static`, a lifetime that `spawn`'s `&Shared`
does not promise; that is Rust's side, outside the Verus contract.

## Notes

1. **Kernel writes into Rust locals.** The raw syscall stubs claim that
   nothing Verus tracks changes. Verified wrappers pass the addresses of
   their own locals for the kernel to write: `clock_gettime` (`ts`),
   `ncpus` (`mask`) and `block_all_signals` (`old`). That is sound
   because the address is made through a `&mut` borrow whose spec says
   nothing of the referent afterwards (`ptr::from_mut` and
   `<[T]>::as_mut_ptr` in `vspec/sys.rs`), so Verus treats the local as
   overwritten. After `from_mut(&mut x)` and `a.as_mut_ptr()`,
   `assert(x == 0)` and `assert(a[0] == 0)` fail to verify, while after
   `from_ref(&y)`, `assert(y == 0)` holds. An `ensures *final(r) ==
   *old(r)` on those core specs, though true of the functions themselves,
   would let these wrappers prove false facts, such as `ncpus() == 1`.
2. **Memory the kernel changes behind a permission.** `munmap`,
   `mprotect` and `madvise` are verified `unsafe fn`s with no `requires`
   on the range. No caller applies them to memory that a `PointsTo` with
   a value covers: `Meta` never purges, and purges and THP advice touch
   user memory and guard slabs, which hold no typed permission. The
   stubs' "changes nothing tracked" holds as a property of the call
   sites, not of a contract.
3. **Zero axioms with unchecked premises.** `axiom_fresh_anon_zero`
   mints a `Zeroed` token for any range, `axiom_discarded_zero` for any
   range whose `MADV_DONTNEED` succeeded (`sys::madvise_ok`), and
   `Unbacked::commit` a `PointsToRaw` and a `Zeroed` for any `Unbacked`,
   with the kernel-state premise left to the call site. None can prove
   `false`: the tokens carry no exclusivity, and `Unbacked` tokens are
   disjoint by construction. Each is called only where its doc says
   (`huge::zero_run` for the first two, and `class::table::SlabTable::new`
   for `axiom_fresh_anon_zero`; `meta.rs` for `commit`). A
   misplaced call would make the zero theorem false without any
   verifier error.
4. **Page size.** `getauxval` bounds `AT_PAGESZ` at 64 KiB and ties it
   to `process_regions().lg_page`. `Regions::reserve_owning` accepts any
   page up to `MAX_SLAB` (4 MiB), so the 64 KiB bound is a platform fact
   that the contract states, not one the code enforces.
5. **Edge cases tried.** Scratch proofs, each `ensures false`,
   instantiate the axioms and trusted proof functions at their edges:
   `axiom_fork_resumed` twice on one token; `axiom_c_claim` twice on one
   pointer, on address 0 and on `usize::MAX`; `axiom_user_va_limit` and
   `axiom_auth_sign` at `p = 0` and `p = user_va_limit() - 1`, with
   modifiers 0, `usize::MAX` and equal to `p`; the two zero axioms at
   address 0 with length 0, at length `usize::MAX`, at a negative
   address, and at address and length `usize::MAX`; and
   `Unbacked::commit` of empty tokens at -1, 0 (twice)
   and `usize::MAX` after a `split(0)` and `join`. Verus refutes each
   ("postcondition not satisfied"). `axiom_fork_resumed_state` twice is
   the exception: each call's `StateHold` carries a permission of the
   state lock's cell, and vstd's `PointsTo::is_exclusive` makes two
   contradict. A scratch proof of `false` from two permissions of one
   lock's cell verifies in `lock.rs`, where `Lock::has_perm` unfolds; the
   same proof from two well-formed holds fails in `purge/background.rs`,
   where `has_perm` is closed. The axiom's doc keeps it to one call per
   handler. The `Reserved` proof functions need tokens that only
   `try_reserve` (exec) mints, so they are read, not instantiated.
