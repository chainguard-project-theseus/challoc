# Seam audit: the C and kernel boundary

An adversarial audit of the seams of verus/SEAMS.md in five categories:
inline asm, FFI/syscall, platform/kernel fact, C-program contract and raw
pointer from C caller (76 seams; the ledger at 8635213, numbers as in its
index, which this audit leaves unchanged). For each: can it prove
`false`, does its body meet its contract for every input its `requires`
admits, is it still needed, and does it claim more than its callers use.
The four of them discharged with Isla are audited with their records.

Verdicts: **sound** (no defect found; a premise the seam names is not a
defect), **fixed** (a false or unsound contract, corrected here, callers
re-proved), **weakened**, **removed**, **unsound-open** (a defect left
for a decision). Totals: 69 sound, 7 fixed, 0 weakened, 0 removed,
0 unsound-open; the 4 Isla records were stale and are revised.

## Verdicts

| seam | item | verdict | reason |
|---|---|---|---|
| S1 | `throw_bad_alloc_addr` | sound | GOT load of a weak symbol (null when undefined); claims nothing |
| S7 | `store_usable` | sound | the `Stored` token states exactly the store the body makes, none for null |
| S8 | `store_out` | sound | claims nothing; the out-pointer is `posix_memalign`'s C contract |
| S12 | `name_byte` | sound | claims nothing; `cstr` reads up to the first NUL and at most `MAX_NAME` bytes |
| S13 | `name_bytes` | sound | `r@.len() == n` is what `from_raw_parts` returns |
| S14 | `old_len` | sound | claims nothing |
| S15 | `copy_prefix` | fixed | admitted any `n`, but copies `n` bytes out of `&T`: now `n <= size_of::<T>()` and `n <= oldlen` |
| S16 | `store` | fixed | the C buffer's size went unstated: now `oldlen == size_of::<T>()`, the caller's check verified |
| S17 | `load` | fixed | as `store`, `newlen == size_of::<T>()`; every `T` used is valid for any bytes |
| S19 | `query_ptr` | sound | the precondition puts the 8-byte read inside `newlen` |
| S20 | `store_triple` | sound | the precondition puts the 24-byte write inside `oldlen` |
| S21 | `mib_cap` | sound | claims nothing |
| S22 | `mib_out` | fixed | the capacity went unstated: now `len <= cap`, the value `mib_cap` read |
| S23 | `mib_in` | sound | `len <= MAX_MIB` keeps the copy in `local`, which Verus havocs (no `ensures`) |
| S31 | `axiom_fork_resumed_state` | sound | mints a `LockHeld`; no lemma makes two of one lock contradict, though two would let `get_unlocked` alias (doc says so) |
| S32 | `axiom_fork_resumed` | fixed | ensured a fact about its input token: any other lock's token proved `false` |
| S38 | `Unlocked` | sound | opaque, private, non-`Copy` |
| S39 | `axiom_unlocked` | sound | a premise about the C caller; three call sites, as its doc says |
| S40 | `emit` | fixed | no bound on `len`: `buf[len] = 0` and `fwrite(buf, len)` go past the buffer |
| S41 | `opt_byte` | sound | claims nothing; `Opts::parse` stops at NUL or 64 bytes |
| S102 | `read_slot` | sound | claims nothing |
| S103 | `malloc_conf_addr` | sound | GOT load; claims nothing |
| S104 | `je_malloc_conf_addr` | sound | as S103, weak |
| S105 | `byte_at` | sound | claims nothing |
| S106 | `bytes_at` | sound | as S13 |
| S107 | `env_var` | sound | the value is a suffix of an entry `cstr` cut at `MAX_TEXT` |
| S112 | `seed` | sound | `requires` only; outside `verus!`, claims nothing |
| S113 | `axiom_c_claim` | sound | the C contract of `free`, once per entry point (and per branch of the verification-only fast-path theorem); a double free makes the proof vacuous, as documented |
| S134 | `pthread_setspecific` | sound | the re-entry premise is named; `requires reentry_benign` is checked |
| S168 | `axiom_process_page_at_most_64k` | sound | Linux pages are at most 64 KiB; the code itself accepts up to 4 MiB (decision 2) |
| S169 | `Reserved` | sound | opaque, non-`Copy`; minted only by `try_reserve` |
| S170 | `Reserved::is_apart_from_static` | sound | `mmap` without `MAP_FIXED` never overlaps a live mapping |
| S171 | `Reserved::is_disjoint` | sound | `&mut self` and `&Self` cannot be one token; two live mappings are apart |
| S172 | `Reserved::into_unbacked` | sound | consumes the token, so a range is handed out once |
| S173 | `try_reserve` | sound | a successful hint-free `mmap` is above 0 and inside the default window |
| S189 | `errno_location` | sound | claims nothing |
| S190 | `errno_read` | sound | claims nothing |
| S191 | `errno_write` | sound | claims nothing; outside the `ThreadErrno` model, as `vspec::errno` says |
| S192 | `pthread_join` | sound | claims nothing |
| S193 | `spawn` | fixed | passes `shared` to the thread, whose `shared_arg` ensures `wf()`, without requiring it |
| S199 | `symbols` | sound | GOT loads of weak symbols; claims nothing |
| S200 | `thread_pointer` | sound | claims nothing |
| S207 | `getauxval` (x86_64) | sound | `AT_PAGESZ` is 4096 on x86_64 Linux (0 if absent) |
| S208 | `getauxval` (aarch64) | sound | pages of 4, 16 or 64 KiB; `HWCAP_PACA` covers the data keys `pacda`/`autda` use |
| S215 | `getrandom` | sound | the kernel returns at most `buflen`; the bytes are havocked |
| S217 | `read` | sound | as S215 |
| S219 | `write` | sound | the kernel returns at most `count` |
| S221 | `trap` | sound | `ud2`/`brk` never fall through |
| S222 | `axiom_user_va_limit` | sound | 47-bit window on x86_64, 48-bit on arm64 without `FORCE_52BIT` |
| S223 | `axiom_auth_sign` | sound | below the VA limit bit 55 is clear and `autda` restores the PAC bits from it |
| S224 | `sign` | sound | a function of `(p, modifier)` under the key premise named in `spec_sign` |
| S225 | `auth` | sound | as S224; a failed check faults or yields a pointer that faults |
| S226 | `strip` | sound | claims nothing |
| S227 | `syscall0` (x86_64) | sound | `rcx`/`r11` clobbered, memory not `nomem`; claims nothing |
| S228 | `syscall1` (x86_64) | sound | as S227 |
| S229 | `syscall2` (x86_64) | sound | as S227 |
| S230 | `syscall3` (x86_64) | sound | as S227 |
| S231 | `syscall4` (x86_64) | sound | as S227 (`r10` for the fourth argument) |
| S232 | `syscall5` (x86_64) | sound | as S231 |
| S233 | `syscall6` (x86_64) | sound | as S231 |
| S234 | `syscall0` (aarch64) | sound | `svc 0`, x0 out; the kernel preserves the rest |
| S235 | `syscall1` (aarch64) | sound | discharged; its Isla record quoted a stale contract, revised |
| S236 | `syscall2` (aarch64) | sound | as S234 |
| S237 | `syscall3` (aarch64) | sound | as S235 |
| S238 | `syscall4` (aarch64) | sound | as S234 |
| S239 | `syscall5` (aarch64) | sound | as S234 |
| S240 | `syscall6` (aarch64) | sound | as S235 |
| S241 | `get` | sound | the `TlsSlot` premise is named |
| S242 | `get_tp` (aarch64) | sound | discharged; its Isla record quoted a stale contract, revised |
| S243 | `set` | sound | the `TlsSlot` premise is named |
| S283 | `lend` | sound | no code; the token's value is unknown |
| S284 | `set_errno` | sound | writes `errno` through `__errno_location` |
| S314 | `libc::FILE` | sound | an opaque type |
| S357 | `axiom_fresh_anon_zero` | sound | `mmap(2)`; premise named, not checked (note 3) |
| S358 | `axiom_discarded_zero` | sound | `madvise(2)` `MADV_DONTNEED`; premise named, not checked (note 3) |
| S359 | `Unbacked::commit` | sound | `mprotect(2)`/`mmap(2)`; premise named, not checked (decision 3) |

## Details

### S32 `api::axiom_fork_resumed`: derives `false` (fixed)

The axiom was

```rust
axiom fn axiom_fork_resumed(s: &Shared, tracked state: &LockHeld) -> (tracked held: ForkHeld)
    ensures
        fork_holds(*s, held, *state),
```

and `fork_holds` includes `state.held() == purge::background::state_lock()`:
a conclusion about a token the caller already holds. Verus accepted, in
`src/api/mod.rs` at 8635213,

```rust
proof fn scratch_fork_resumed_false(s: &Shared, tracked t: &LockHeld)
    requires
        t.held() != purge::background::state_lock(),
    ensures
        false,
{
    let tracked h = axiom_fork_resumed(s, t);
}
```

(`cargo verus focus -- --verify-module api`: 353 verified, 0 errors).
Any `LockHeld` of another lock, such as one `RawLock::lock_held` returns
or one in `fork_prepare`'s bundle, satisfies the `requires`. The fix
moves the fact into a `requires`; the one call site,
`fork_release`, already requires it of its `state`. With the fix the
scratch proof fails, so it was not kept: a regression proof cannot state
that something does not verify.

### S40 `api::print::emit`: missing bound (fixed)

`emit` had `requires may_syscall()` only, but its body stores the NUL
at `buf[len]` (a bounds-check panic for `len >= 1024`), slices
`&buf[..len]` (a panic for `len > 1024`) and calls
`fwrite(buf.as_ptr(), 1, len, f)`, which reads past the 1024-byte buffer
for `len > 1024` with no panic. Its caller `Sink::flush` is verified but
cannot know `len < 1024`: `Sink` has no type invariant (the comment
before `impl Write for Sink` says why) and the reports write through a
generic `W: Write`, which havocs the sink. The fix: `emit` requires
`len < 1024`, and `flush` checks it with `vspec::check::check` (an abort
on failure, as the bounds check was; `write_str` never leaves `len` above
1023, so it never fires). Exec change, off the hot set: `flush` gains a
compare and a branch to the panic, and `emit`'s bounds check folds away.
Under theorem 32 (read-only APIs never abort) the check is an integrity
check: `flush` requires `may_abort_corrupt()`, and
`api::tests::abort_grants_only_at_roots` lists it.

### S15, S16, S17, S22 `api::mallctl`: unstated sizes (fixed)

`copy_prefix(v: &T, oldp, n)` copied `n` bytes out of the Rust value
`v` for any `n` Verus accepted; a verified caller passing more than
`size_of::<T>()` would over-read Rust memory. `store`, `load` and
`mib_out` left the size of the C buffer to a `# Safety` comment, so the
callers' size checks (`have != want`, `newlen != size_of::<T>()`,
`mib_cap(miblenp) < len`) were not tied to the writes. Each now takes the
length the caller read as a ghost argument, as `query_ptr` and
`store_triple` already did, and requires the bound; the callers verify
unchanged apart from passing it (`mallctlnametomib` binds the capacity
it reads to a `let`). That the ghost length is what the C caller's
pointer holds stays the `mallctl` caller's contract. `load<T>` is
generic over `T: Copy`; a `T` with invalid bit patterns (`bool`) would
make it undefined behaviour, and every instantiation (`u8`, `u64`,
`usize`, `isize`, `c_uint`, `*const c_void`) is valid for any bytes:
recorded in its doc, since Verus cannot state it.

### S193 `purge::background::spawn`: missing requirement (fixed)

`spawn(shared, ..)` hands `shared` to the new thread, whose
`shared_arg` (raw pointer (internal), another lane's) ensures
`r.wf()` of it, on the grounds that "`start` was given a well-formed
`Shared`". `spawn` did not require that, so a verified caller with any
`Shared` could make the thread's code assume `wf()`. It now requires
`shared.wf()`; its one caller `create` requires it already. (The thread
also takes the reference as `&'static`, a lifetime `spawn`'s `&Shared`
does not promise; that is Rust's side, recorded in decision 4.)

### The Isla records of S235, S237, S240 and S242 (revised)

All four records quoted, as their contract, the ledger entry at
d865271: no `requires` for the three aarch64 `svc` stubs, and no token
and no `ensures` for `get_tp`. At 8635213 the stubs require
`may_syscall()` and `get_tp` takes `Tracked<&TlsSlot>` and ensures
`r.0 as usize == slot.value()`. The record check of `verus/isla/run.sh`
(part 1, no Isla needed) failed on all four at 8635213 ("its contract
differs from the ledger's"). The records now quote the current
contracts. For the stubs the added `requires` is an opaque capability
that asks nothing of the machine, so the Isla statement still covers
it. For `get_tp` the `ensures` is covered by Isla fact 3 (`out` is the
8 bytes of the calling thread's `challoc_tls`) together with the
`TlsSlot` premise that a token's value is the slot's (named as a
residual; the trusted `get` and `set` rest on it too). The asm templates
are unchanged, so the Isla facts stand; `run.sh` should be rerun where
the Isla toolchain exists.

## Notes (sound, with something to say)

1. **Kernel writes into Rust locals.** The raw syscall stubs claim that
   nothing Verus tracks changes. Verified wrappers pass the addresses of
   their own locals for the kernel to write: `clock_gettime` (`ts`),
   `ncpus` (`mask`), `block_all_signals` (`old`). That is sound only
   because the address is made through a `&mut` borrow whose spec says
   nothing of the referent afterwards (`ptr::from_mut`,
   `<[T]>::as_mut_ptr` in `vspec/sys.rs`), so Verus treats the local as
   overwritten. Checked with a scratch function: after
   `from_mut(&mut x)` and `a.as_mut_ptr()`, `assert(x == 0)` and
   `assert(a[0] == 0)` fail, while after `from_ref(&y)` `assert(y == 0)`
   holds. Adding `ensures *final(r) == *old(r)` to those core specs
   (true of the functions themselves) would make these wrappers prove
   false facts, such as `ncpus() == 1`. The core-library lane owns those
   specs; this is recorded there too (decision 1).
2. **Memory the kernel changes behind a permission.** `munmap`,
   `mprotect` and `madvise` are verified `unsafe fn`s with no
   `requires` on the range. Today no caller applies them to memory that
   a `PointsTo` with a value covers (`Meta` never purges; purges and THP
   advice touch user memory and guard slabs, which hold no typed
   permission), so the stubs' "changes nothing tracked" holds, but it is
   a property of the call sites, not of a contract.
3. **Zero axioms with unchecked premises.** `axiom_fresh_anon_zero` and
   `axiom_discarded_zero` mint a `Zeroed` token for any range, and
   `Unbacked::commit` a `PointsToRaw` and a `Zeroed` for any `Unbacked`,
   with the kernel-state premise left to the call site. None can prove
   `false` (the tokens carry no exclusivity; `Unbacked` tokens are
   disjoint by construction), and each is called only where its doc
   says (`huge::zero_run`; `meta::Inner::commit_bump_to` and
   `reserve_committed`). A misplaced call would make the zero theorem
   lie without any verifier error.
4. **Edge cases tried.** Eight scratch proofs, each `ensures false`,
   instantiated the slice's axioms and trusted proof functions at the
   edges: `axiom_fork_resumed_state` twice, and `axiom_fork_resumed`
   twice on one token (after the fix); `axiom_c_claim` twice on one
   pointer, on address 0 and on `usize::MAX`;
   `axiom_process_page_at_most_64k`, `axiom_user_va_limit` and
   `axiom_auth_sign` at `p = 0` and `p = user_va_limit() - 1` with
   modifiers 0, `usize::MAX` and equal to `p`; the two zero axioms at
   address 0 length 0, length `usize::MAX`, a negative address, and
   address and length `usize::MAX`; `Unbacked::commit` of empty tokens
   at -1, 0 (twice) and `usize::MAX` after a `split(0)` and `join`; and
   `axiom_unlocked` twice. Verus refuted all eight (`postcondition not
   satisfied`; `cargo verus focus -- --verify-module api
   --verify-module api::print`: 352 verified, 8 errors, the eight). Two
   tokens of one lock are consistent because no lemma claims two
   `LockHeld` of one lock are apart, as `axiom_fork_resumed_state`'s doc
   requires. The `Reserved` proof functions need tokens only
   `try_reserve` (exec) mints, so they were read, not instantiated.

## Decisions for the coordinator

1. Whether to state in `vspec/sys.rs` that `ptr::from_mut` and
   `<[T]>::as_mut_ptr` must keep saying nothing of their referent
   (note 1), or to give the syscall wrappers that pass locals their own
   specs.
2. `axiom_process_page_at_most_64k` duplicates the `getauxval` fact
   (`AT_PAGESZ <= 64 KiB`, from which `features::page_size` proves its
   bound) because `Regions::reserve_owning` accepts any page up to
   `MAX_SLAB` (4 MiB) and `process_regions()` is not tied to
   `page_size()`. Narrowing `reserve_owning`'s check to 64 KiB and
   `Regions::separated` to `lg_page <= 16` would make the axiom a lemma;
   it changes an init-time check and the tests that build regions.
3. `Unbacked::commit` could require a ghost token that only a successful
   `mprotect(PROT_READ | PROT_WRITE)` of exactly its range mints (from
   `meta::commit_range`), turning its premise into a checked one at the
   cost of one trusted mint in the `mprotect` wrapper.
4. `spawn` takes `&Shared` and the thread uses it as `&'static Shared`;
   giving `spawn`, `create` and `start` `&'static Shared` would say so in
   the types (no codegen change, signatures other lanes touch).
5. Outside this slice (allocator invariant, S33 and S34 of the ledger),
   found by this audit's review and left to that category's lane:
   `api::count_held` ensures `r as int == held.n()` for a `u32` result,
   a fact about its input `ForkHeld` (that `0 <= n <= u32::MAX`), the
   pattern fixed in S32. `axiom_fork_resumed` leaves `n` unconstrained,
   so the two do not prove `false` today, but proof code inside `api`
   could build a `ForkHeld` with `n == -1` around a real `Arenas` token
   and derive it. Suggested fix: add `0 <= n <= u32::MAX` to
   `ForkHeld::wf` (`fork_prepare` proves it from its `u32` read, and the
   axiom then ensures it), and let `count_held` require it. `arena_held`
   (S33) states facts about its input too, consistent ones.
