# Miri: undefined behaviour in the trusted unsafe code

The Verus proofs trust the `external_body` functions and every raw access
behind a `PointsTo` permission (`ptr_ref`, `ptr_mut_ref`, `ptr_mut_write`):
Verus checks that the permission is held. It does not check that the Rust
code is defined behaviour under Rust's aliasing rules, or that a layout cast
is aligned and in bounds. [Miri](https://github.com/rust-lang/miri)
interprets the unit tests and checks those things on every operation the
tests reach: out-of-bounds and use-after-free accesses, uninitialised reads,
invalid values and misaligned references, data races, and aliasing under
Stacked Borrows (the default) or Tree Borrows (`-Zmiri-tree-borrows`).
Read at `ffc75fe`; line numbers are from that tree.

## Running

```
rustup +nightly component add miri      # cargo-miri must be on PATH
verus/miri/run.sh OUT [FILTER]          # the unmodified tree
verus/miri/run.sh -e OUT [FILTER]       # with the syscall emulator
MIRIFLAGS=-Zmiri-tree-borrows verus/miri/run.sh -e OUT
```

`run.sh` copies the tracked files to `OUT/src` (Miri reads the sources again
for every test, and the copy keeps that off a slow or shared mount), builds
the tests once and runs each test in its own Miri process (`-j`, default
`nproc`; `-t`, default 1200 s each), because a Miri error ends the process
and a single `cargo miri test` stops at the first test that issues a
syscall. Each test's log is in `OUT/logs/`; `OUT/results.txt` sorts them
into `pass`, `ub`, `unsupported`, `abort`, `killed` (a signal, usually the
host out of memory), `fail` and `timeout`. `-Zmiri-disable-isolation` is
always on (the tests read `/proc` and the clock); `-e` also adds
`-Zmiri-ignore-leaks`, because the fixtures leak their `Shared` and their
reservations on purpose.

### The emulator (`-e`)

challoc makes its syscalls with inline assembly (`sys/raw.rs`), reads its
TLS slot with inline assembly (`sys/tls.rs`) and asks glibc's `getauxval`
for the page size; Miri runs none of these. `verus/miri/emulator.patch`,
applied to the copy only, replaces them under `cfg(miri)`:

- `sys::raw::syscall0`..`syscall6` dispatch the syscall numbers to libc
  calls Miri implements (`read`, `write`, `open`, `close`, `clock_gettime`,
  `sched_getaffinity`, `getrandom`, `futex`, `gettid`). `mmap` maps the
  whole reservation read-write at once; `MAP_FIXED` over it and
  `MADV_DONTNEED` zero the range; `mprotect`, `munmap` and `MADV_FREE`
  succeed and do nothing; the guard advice fails with `EINVAL`, so the
  allocator takes its no-guard path. A reservation over 2 GiB fails with
  `ENOMEM`: Miri materialises every mapping, so the real layout (terabytes
  of `PROT_NONE`) cannot be built, and the tests that need the process-wide
  allocator (`api::shared()`) abort in `Regions::reserve`.
- `tls::get` and `set` use a `thread_local!` cell; `getauxval` answers
  4096 for `AT_PAGESZ` and 0 otherwise; the weak-symbol GOT loads
  (`rseq::symbols`, `config::malloc_conf_addr`, `je_malloc_conf_addr`,
  the libstdc++ handlers) return null or the library's own `malloc_conf`;
  `layout::regions::class_entry` drops its empty `asm!` barrier.
- `#![deny(warnings)]` is off under Miri (the replaced asm leaves unused
  imports).

The allocator code above the syscalls is the real code. Protections are
not modelled, so a test that expects a fault, a `PROT_NONE` line in
`/proc/self/maps` or an `mmap` error code fails for the emulator's sake,
not challoc's.

## Results

x86_64, nightly 2026-09-30 (`miri 0.1.0 21b707e3f9`), quantum16. 551 unit
tests; the 94 `api::c_programs` rows run C programs against the release
`.so`, which Miri cannot do, and are left out of the emulated runs.

| Run | Pass | UB | Unsupported | Abort | Fail | Timeout |
|---|---|---|---|---|---|---|
| Unmodified tree, Stacked Borrows | 148 | 0 | 301 | 0 | 93 | 9 |
| Emulator, Stacked Borrows | 312 | 58 | 14 | 44 | 11 | 18 |
| Emulator, Tree Borrows | 302 | 61 | 14 | 44 | 12 | 24 |
| Emulator, aliasing model off (`-Zmiri-disable-stacked-borrows`) | 368 | 0 | 14 | 44 | 13 | 18 |

The table is for the tree before the fixes of findings 2 and 3. After them,
the changed tests reach finding 1 in both aliasing models and pass with the
model off, apart from the timing failure of `periodic_work` described
below. The runs used a draft of `emulator.patch` that differed in three
ways: comments; `exit` instead of `_exit` for `exit_group`; and `EINVAL`
for an `mmap` address hint without `MAP_FIXED`. The one test that passes
such a hint, `sys::tests::mmap_fixed_noreplace_refuses_overlap`, failed
with the draft. With this patch it passes, through the branch the test
keeps for a pre-4.17 kernel that ignores `MAP_FIXED_NOREPLACE`.

The unmodified tree runs the pure logic only: layout and class geometry,
bitmaps, config and cgroup parsing, policy, stats, slab descriptors, the
slab-protocol model. Every other test stops at its first syscall (176 at
inline assembly, 116 at `getauxval`, 9 at `posix_spawn` or `sigemptyset`).
93 fail because the release `.so` is missing: 91 `api::c_programs` rows and
2 symbol-table tests. The other 3 rows stop at `getauxval` first. Tree
Borrows on the 148 pure tests: 147 pass and 1 times out.

The UB column is finding 1 below (57 tests in both models), plus
`periodic_work_runs_gc_on_the_interval` (finding 2, every model), plus,
under Tree Borrows only, the 3 tests of the `TestShared` fixture (finding 2).
With the aliasing model off nothing was found: no out-of-bounds access, no
uninitialised read, no misaligned or invalid value, no use after free and
no data race in the 368 tests that complete. Those include the heap,
class, huge, meta, bitmap, purge and arena tests and the threaded ones that
finish in time. `-Zmiri-strict-provenance` and
`-Zmiri-symbolic-alignment-check` on the 148 pure tests stop only where the
design calls for them (below).

### Undefined behaviour

**1. The heap's cache handles (Stacked and Tree Borrows; 57 tests).**
`Heap::cache_ptr(&mut self, class)` (`heap/cache.rs:308`) returns
`(&raw mut self.caches).cast::<CacheHeader>().add(class)`, a raw pointer
derived from that call's `&mut self`. The callers then pass the pointer to
other `&mut self` methods (`hdr_mut`, `pop`, `refill`, `flush`,
`rebuild_run`, and the `&self` ones that write through it), which
dereference it. Each such call reborrows the whole `Heap`, including
`caches`, as a fresh protected `&mut`: under Stacked Borrows that
invalidates the earlier pointer, under Tree Borrows it freezes it. The
first hit is `ensure_cache` (`heap/cache.rs:879`, `self.hdr_mut(cache)`
after `ptr_mut_write(cache, ..)` at `:834`); every heap test that allocates
stops there, so later sites (the fast pop in `alloc_fast` among them) are
not reached. Reduced:

```rust
pub struct Hdr { pub len: u32 }
pub struct Heap { pub caches: [Hdr; 4] }
impl Heap {
    pub fn cache_ptr(&mut self, class: usize) -> *mut Hdr {
        unsafe { (&raw mut self.caches).cast::<Hdr>().add(class) }
    }
    pub fn hdr_mut(&mut self, cache: *mut Hdr) -> &mut Hdr {
        unsafe { &mut *cache }
    }
}
#[test]
fn raw_pointer_into_self_across_a_mut_self_call() {
    let mut h = Heap { caches: [const { Hdr { len: 0 } }; 4] };
    let c = h.cache_ptr(1);
    unsafe { (*c).len = 1 };
    let r = h.hdr_mut(c); // SB: retag invalidates c; TB: freezes it
    r.len += 1;           // TB: write through a frozen tag
}
```

Both models reject it. These rules are what justify emitting `noalias` for
`&mut` arguments to LLVM. No miscompile is known: the machine code
is what the tests and benchmarks run, and a miscompile needs LLVM to
reorder an access through `self` against one through the handle to the
same header. The headers of explicit caches (`others`) live in metadata
blocks outside `Heap` and are not affected; only the inline home headers
are. The fix is not local, so it is left to a design change. The
handle-taking methods must stop holding a `&mut Heap` over `caches` while
they use the handle, by one of: `caches` in an `UnsafeCell` with `&self`
receivers (Cell-like fields for what those methods mutate); raw
`*mut Heap` receivers; or the header passed as the only `&mut` (the
remaining fields split into their own struct). Every option changes the
verified API of `src/heap/` and must keep the hot set's machine code
identical.

**2. Test code (fixed).**

- `heap/tests.rs:1322` wrote `gc_interval_ns` through a `&'static Shared`
  cast to `*mut`, which is UB in every model. `TestShared` now has a
  `gc_interval_ns` builder option.
- `TestShared::build` (`testutil.rs:134`) passed `b.as_mut_ptr()` of a
  `&mut MaybeUninit<Shared>` to `Shared::init` and then reborrowed `b`.
  `init` stores pointers derived from its argument in the arena registries.
  The reborrow froze them under Tree Borrows, and a later atomic store
  through them was UB (3 tests). A reduction of the same shape is UB under
  Stacked Borrows too. The fixture now keeps the one raw pointer from
  `Box::into_raw`.
- Found by reading, because finding 1 stops these tests first; reductions
  are UB in both models. `heap/tests.rs:584` wrote a `CacheHeader` through
  the `&` that `Heap::cache` returns, and now writes through `&mut` from
  `caches`. `TestHeap::heap` returns a fresh `&mut Heap` on each call, and
  six tests (`foreign_frees_share_one_flush_only_cache_per_class`,
  `dropping_the_home_caches_keeps_the_thread_counters`,
  `set_arena_swaps_cache_arrays`, `a_free_routed_to_an_unbuilt_arena_aborts`,
  `pop_fast_pops_the_top_with_the_zero_flag`,
  `checked_slot_any_is_checked_slot_without_the_arena_test`) kept one
  across `t.alloc` and the other `TestHeap` calls, which take another.
  `check_run` did the same with the `&CacheHeader` of `t.cache`. These
  tests now make the `TestHeap` calls before taking `h`, or go through
  `h`, or call `t.heap()` at each use.

**3. The production `Shared` (fixed).** `shared_storage_ptr`
(`api/mod.rs:360`), the pointer `api::init` hands to `Shared::init`, was
`(*SHARED.0.get()).as_mut_ptr()`: the same `as_mut_ptr` through a temporary
`&mut MaybeUninit<Shared>`. Every reader then takes `&Shared` from the cell
(`Global::get`, `:155`), which invalidates (Stacked Borrows) or freezes
(Tree Borrows) the pointers `init` stored, such as the registry's
`arena0`. A reduction (an `UnsafeCell<MaybeUninit<S>>` static initialised
that way, then an atomic store through the stored pointer after a
`get`) is UB in both models. It is now `SHARED.0.get().cast::<Shared>()`,
which passes both. The release library's disassembly is identical to the
base's. The test suite cannot show this: Miri cannot build the real layout.

### Not undefined behaviour

- **Unsupported** (Miri cannot run it): inline assembly, `getauxval`,
  `fork`, `posix_spawn` (the abort tests and the C header cross-check run a
  child), `sigemptyset`, `mincore`; the release `.so` and C programs.
- **Miri limitations**:
  - Timeouts: the exhaustive and randomised model checks
    (`bitmap::tests::*_matches_model`,
    `layout::tests::slot_of_offset_is_exactly_the_reciprocal_check`, two
    `model::tests` scenarios, `meta::tests::matches_model`) and the long
    threaded tests run far past 20 minutes under the interpreter.
  - `heap::tests::periodic_work_runs_gc_on_the_interval` expects no GC
    within the default 10 s interval, but the interpreted test takes
    longer than that. This is its failure with the aliasing model off;
    the aliasing models stop it at UB first.
  - Miri's `write` is allowed to be short, and is, so
    `sys::tests::write_to_devnull_and_bad_fd`'s `Ok(5)` sees `Ok(2)`
    (`write_all` loops, as it should).
  - One Tree Borrows run
    (`huge::tests::table_grows_and_lookups_survive_backward_shift`) was
    killed with `SIGKILL` after 3 minutes, most likely out of memory; it
    passes under Stacked Borrows.
- **Emulator artefacts**: `/proc/self/maps` permissions (`meta`, `class`
  commit tests), `mmap` error codes, `getrandom` flags, `prctl`, guard
  installation, glibc's rseq symbols.
- **By design**: `-Zmiri-strict-provenance` stops at the first
  integer-to-pointer cast (`slab::SlabDesc::at`, and in test code):
  challoc computes addresses as integers by design and Verus reasons about
  them that way. `-Zmiri-symbolic-alignment-check` rejects
  `slab::tests::Descs`, whose buffer is aligned by rounding the address up,
  which that flag does not credit (4 tests).

## Gaps

- x86_64 only: the image has no aarch64 Miri sysroot, so the aarch64 code
  (PAC, `tpidr_el0`) is not interpreted.
- Finding 1 hides the heap paths behind `ensure_cache` from both aliasing
  models; only the model-off run covers them.
- Anything that needs the real layout (`api::shared()`: the entry points,
  `mallctl`, the reports) reaches Miri only as far as `Regions::reserve`.
