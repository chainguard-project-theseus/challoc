# Isla: the inline-assembly seams

Twenty-six seams in [SEAMS.md](SEAMS.md) are inline assembly (category
"inline asm", tool Islaris/Isla): raw syscalls, the TLS slot and thread
pointer, GOT loads of weak symbols, pointer authentication and the trap
instruction. Verus sees only their signatures.
[Isla](https://github.com/rems-project/isla) symbolically executes machine
code against a Sail ISA model (here the Armv8.5-A model translated from
Arm's ASL) and prints, per instruction, every path as a trace of register
and memory events with SMT definitions;
[Islaris](https://github.com/rems-project/islaris) proves properties of
such traces in Coq/Iris. This file is the setup, the triage of the 26
seams (and of `axiom_auth_sign`, a platform fact the same runs check),
the fifteen discharged so far, the evidence for the aarch64 halves of the
mixed seams, and the plan for the rest. Seam numbers and line numbers are
from 8635213.

## Setup

What the Isla lanes installed, in a Linux VM without a package manager,
on 2026-10-02 and 2026-10-03:

| tool | version | how |
|---|---|---|
| Isla | rems-project/isla e9b5d94 | `cargo build --release` (a minute) |
| Sail models | isla-snapshots d8b3101 | git clone (`armv8p5.ir`) |
| libz3 for Isla | Z3 4.13.4 x64-glibc-2.35 | GitHub release zip |
| z3 for isla-check | Z3 4.16.0 | the one Verus ships |
| llvm-objdump, llvm-ar | LLVM of rustc 1.95.0 | rustup `llvm-tools` |

Not installed:

- **Islaris** (Coq 8.x, Iris, std++ and the Islaris Coq development,
  normally through opam). The VM has no opam, OCaml, GNU make or m4, a
  slow network and slow disk writes to the work tree; building that stack
  from source was not attempted. The harness below does the Isla-only
  part: it composes the per-instruction traces itself and discharges the
  resulting conditions with Z3. That is weaker than an Islaris proof: no
  proof certificate, and the composition is `check.rs`'s own (part of the
  trusted base). It drops trace events it cannot express in SMT (struct
  and enum values, `assume-reg`, unknown events), which only weakens path
  conditions or fails the check, never passes one (see below).
- **An x86_64 model.** isla-snapshots carries Arm, RISC-V and Morello
  models only, and Isla's `configs/` has no x86 configuration. Building
  one means the Sail compiler and the `isla-sail` plugin (OCaml, opam)
  and the Sail x86 model (`sail-x86-from-acl2`). Every x86_64 half below
  stays trusted.

To reproduce:

```
git clone https://github.com/rems-project/isla
git clone https://github.com/rems-project/isla-snapshots
# libz3.so (Z3 >= 4.12) in the isla checkout root, then:
(cd isla && cargo build --release)
rustup component add llvm-tools --toolchain 1.95.0
export ISLA=$PWD/isla ISLA_SNAPSHOTS=$PWD/isla-snapshots
export LD_LIBRARY_PATH=$ISLA Z3=/path/to/z3
verus/isla/run.sh [-a libchalloc.a] [OUT]
```

Things that cost time on the first install: `isla-footprint -x` takes
the opcode's bytes in memory order (`d4000001`, `svc #0`, is
`-x -i 010000d4`); the configuration's default toolchain must name tools
on `PATH` even when only opcodes are given (`run.sh` points them at
`true`); the harness uses `armv8p5.ir`, whose footprints are the
instruction alone with its PC update (the v9.4 snapshot's start with the
model's reset sequence and end without one). For the PAC instructions:
the model's QARMA computation, unfolded symbolically, did not finish in
ten minutes, while `--abstract ComputePAC` makes it an `abstract-call`
event and each trace takes two seconds; a symbolic TCR is rejected
("symbolic bit-vector length", from the VA size), so TCR is pinned per
VA size; and `--reset-constraint` does not make a constrained value
concrete. Building in a tmpfs copy matters: the work tree is a slow
virtiofs mount, where `cp -r` of the Isla checkout took ten minutes and
each `git` command a minute or two.

## The harness

`verus/isla/run.sh` builds the aarch64 static library from the tree
(about four minutes; `-a` takes a prebuilt one) and then:

1. checks each checked `asm!` template against `templates.txt`
   (verbatim up to whitespace), and for each record the ledger entry
   whose status names it: same contract, and the item under the
   `#[cfg(target_arch = ...)]` of the architecture in the record's title
   (or under none, for a title with no architecture: `sys::pac`'s items
   are one item for both);
2. disassembles the library (`llvm-objdump -dr`) and finds every site of
   the checked templates: every `svc`/`hvc`/`smc` must be `svc #0`,
   every relocation against `challoc_tls` must sit in the TLS load or
   store shape, every PAC-family instruction must be `pacda`/`autda` on
   two distinct general registers or `xpacd` on one, every `brk` must be
   `brk #0x1`, and the GOT functions must be exactly their three or nine
   instructions with their relocations;
3. runs `isla-footprint` on each distinct instruction at Non-secure EL0
   in AArch64 state, with the PC and vector bases symbolic and the
   immediates the linker fills as symbolic opcode fields (`--partial`);
   the PAC instructions run in a second configuration (below), in
   parallel, one process per CPU;
4. runs `isla-check` (`check.rs`, std-only Rust built with `rustc`) on
   each job. It links the instructions' traces in order: a register read
   (each field of a struct register, such as PSTATE, separately) equals
   the value last written to it, or a shared initial value when Isla gave
   a symbol; a read Isla gave as a configuration literal is left as it
   is; a read of a register whose last write it could not express fails.
   It conjoins the path conditions and the job's named assumptions, and
   for every combination of one trace per instruction asks Z3 whether it
   is feasible, depth first: a prefix that is already infeasible (under
   its traces and the assumptions over names it declares, all of them
   constraints of every completion) is not extended, which takes the
   nine-instruction `rseq::symbols` job (856830 combinations) to three
   seconds. A feasible combination must be a normal path (no
   exception unless the job expects the SVC or BRK one; a `fallthrough`
   instruction provably continuing at its own PC + 4), write only the
   job's registers (field writes included), read and write memory as
   often as the job says (each write's address and value are named for
   the asserts), write no NZCV where it says so, contain no event the
   checker does not know, and satisfy each `assert` (its negation
   unsat). A job with no feasible combination fails. Two additions for
   the PAC jobs: a job may run several **copies** of the machine from
   independent initial states (`copy a`, `copy b`; names `a.init.R0`,
   `b.final.R12`), for relational properties such as determinism; and a
   job may name an **abstract** function (`abstract ComputePAC`), whose
   `abstract-call` events become applications of one uninterpreted SMT
   function shared by all copies, so equal arguments give equal results
   and nothing else is known; any other abstract call fails the job;
5. runs negative controls that must fail (27). On the real traces: a
   wrong SVC syndrome, a wrong target EL, a memory read where there is
   none, `svc` expected to fall through, the GOT load one entry off, the
   TLS load without the `ldr` relocation's constraint or without the
   slot's alignment, `add x0, x1, #3` claimed to write nothing; the PAC
   round trip without the VA bound, determinism without the modifier or
   either half of the key or with SCTLR bit 26 equated in place of EnDA,
   a PAC job not naming the abstract function, a purity spot check
   claiming the PAC left the pointer unchanged; the GOT function's load
   one entry off and its `ret` claimed to fall through, `rseq::symbols`
   storing the first value twice, the TLS store storing the offset or
   claimed to write nothing, `brk` with the wrong syndrome or claimed to
   fall through, `mrs` claimed to write nothing. On synthetic traces
   (`controls/`): a read after an inexpressible write, a write through a
   field accessor, a PSTATE.N write under `flags-preserved`, a
   `fallthrough` instruction continuing at PC + 8.

The PAC configuration: the model's v8.2-v8.5 features on (FEAT_PAuth
is v8.3), SCR_EL3 = 0x30401 (NS, RW, API, APK), and twelve variants:
a kernel at EL1 with HCR_EL2 = `HCR_HOST_NVHE_FLAGS` (RW, API, APK) or a
VHE kernel (E2H, TGE, RW), times Linux's TCR for a user VA of 36 (16K
pages), 39 (4K), 42 (64K), 47 (16K), 48 (4K) or 52 bits (64K), TBI0 on.
SCTLR (EnDA), the keys, pointers and modifiers are symbolic. The traces
are made in every run and written whole or not at all.

At 8635213 it passes in about four minutes after the build on 16 CPUs:
two `svc` jobs, twelve TLS load jobs (70 sites), 884 PAC jobs (36
instructions, the 33 site forms and the three canonical ones, with a
footprint and a determinism job in each of the 12 configurations, the
round trip in each, and eight ComputePAC purity spot checks), the
GOT-function job, the `rseq::symbols` job, four `mrs` jobs (76 sites),
two `brk` jobs, two TLS store jobs (5 sites), and every negative control
rejected.

## Triage

The 26 inline-asm seams and `axiom_auth_sign`, by place. "GOT" is a load
of a weak symbol's GOT slot, "TLS" the `challoc_tls` slot through its
initial-exec GOT entry. The status is the ledger's; a seam with an
x86_64 half needs both halves.

| # | item | x86_64 | aarch64 | status |
|---|---|---|---|---|
| S1 | `throw_bad_alloc_addr` | GOT `mov` | GOT `adrp`+`ldr`: evidence | trusted |
| S103 | `malloc_conf_addr` | GOT `mov` | GOT `adrp`+`ldr`: evidence | trusted |
| S104 | `je_malloc_conf_addr` | GOT `mov` | GOT `adrp`+`ldr`: evidence | trusted |
| S199 | `rseq::symbols` | 3 GOT `mov` | 3 GOT pairs: evidence | trusted |
| S200 | `rseq::thread_pointer` | `mov fs:[0]` | `mrs tpidr_el0`: evidence | trusted |
| S221 | `sys::linux::trap` | `ud2` | `brk #1`: evidence | trusted |
| S223 | `axiom_auth_sign` | (no asm) | `pacda`+`autda` | discharged |
| S224 | `pac::sign` | (no asm) | `pacda` | discharged |
| S225 | `pac::auth` | (no asm) | `autda` | discharged |
| S226 | `pac::strip` | (no asm) | `xpacd` | discharged |
| S227-S233 | `raw::syscall0..6` | `syscall` | (other item) | trusted |
| S234-S240 | `raw::syscall0..6` | (other item) | `svc #0` | discharged |
| S241 | `tls::get` | TLS `mov`+`mov fs:` | TLS load: evidence | trusted |
| S242 | `tls::get_tp` | (verified) | TLS load | discharged |
| S243 | `tls::set` | TLS `mov`+`mov fs:` | TLS store: evidence | trusted |

Per seam: the contract (from SEAMS.md), what an Isla run establishes
against it, and the residual assumption that stays trusted.

- **S1, S103, S104** (`api/cxx.rs`, `config.rs`; `#[inline(never)]`).
  Contract: `fn ..._addr() -> *const c_void`, no `ensures`. aarch64
  (`got-fn.job.in`; notes [S1](seams/api-cxx-throw_bad_alloc_addr.md),
  [S103](seams/config-linux-malloc_conf_addr.md),
  [S104](seams/config-linux-je_malloc_conf_addr.md)): each function is
  exactly `adrp x0, :got:sym; ldr x0, [x0, :got_lo12:sym]; ret`, and
  under the relocation pair's definitions it reads exactly the GOT
  entry, returns it in x0, writes nothing else and returns to x30.
  Residual: the ELF loader fills the slot with the symbol's address (the
  program's definition interposing), or 0 for an unresolved weak symbol.
  x86_64: `mov rax, [rip+slot]; ret`, open.
- **S199** `rseq::symbols` (`#[inline(never)]`; [note](seams/rseq-symbols.md)).
  aarch64: the three GOT pairs, then `stp x8, x9, [x0]; str x10,
  [x0, #16]; ret`: the three entries read in order, their values stored
  to the return area's three words at x0, nothing else written.
  Residual: as S1, plus the Rust ABI's return area in x0.
- **S200** `rseq::thread_pointer` (`#[inline(always)]`;
  [note](seams/rseq-thread_pointer.md)). aarch64: every `mrs xN,
  tpidr_el0` in the code (76 sites, 4 registers) writes only xN, with
  TPIDR_EL0. Residual: TPIDR_EL0 holds the C library's thread pointer.
  x86_64 `mov reg, fs:[0]`, open.
- **S221** `sys::linux::trap` ([note](seams/sys-linux-trap.md)). aarch64:
  every `brk` is `brk #1`; it takes the breakpoint exception (EC 0x3C) to
  EL1, or to EL2 under VHE, with the `brk` itself as the return address,
  and never falls through. Residual: Linux sends `SIGTRAP`. x86_64 `ud2`,
  open.
- **S223** `axiom_auth_sign`, **S224-S226** `pac::sign`, `auth`, `strip`:
  discharged (below).
- **S227-S233** x86_64 `raw::syscall0..6`. `syscall` with rax, rdi, rsi,
  rdx, r10, r8, r9 bound and rcx, r11 clobbered. Isla would show the
  instruction writes only rcx (the return RIP), r11 (RFLAGS) and RIP
  (IA32_LSTAR) and no memory. Residual: the Linux x86_64 syscall ABI.
- **S234-S240** aarch64 `raw::syscall0..6`: discharged (below).
- **S241, S243** `tls::get`, `tls::set` (notes [get](seams/sys-tls-get.md),
  [set](seams/sys-tls-set.md)). aarch64: the TLS load (the same sites
  and jobs as `get_tp`) and the TLS store (5 sites, 2 register
  assignments): it reads the GOT entry and writes the value, 8 bytes, to
  the thread pointer plus the offset, and nothing else. x86_64:
  `mov off, [rip+challoc_tls@GOTTPOFF]; mov out, fs:[off]` (or the
  store), open.
- **S242** `tls::get_tp`: discharged (below).

## Discharged

Fifteen seams, each with a five-part record under `verus/seams/` and the
status in `verus/seams.overrides`:

- S234-S240, the aarch64 `syscall0` to `syscall6`
  ([0](seams/sys-raw-aarch64-syscall0.md),
  [1](seams/sys-raw-aarch64-syscall1.md),
  [2](seams/sys-raw-aarch64-syscall2.md),
  [3](seams/sys-raw-aarch64-syscall3.md),
  [4](seams/sys-raw-aarch64-syscall4.md),
  [5](seams/sys-raw-aarch64-syscall5.md),
  [6](seams/sys-raw-aarch64-syscall6.md)): the template, `svc #0` at
  every site, and Isla's trace of `svc #0` at EL0 for a kernel at EL1
  and at EL2 (VHE): the SVC exception with its syndrome, return address,
  vector and target EL, no user register or memory written. Their
  `requires may_syscall()` is a capability nothing here relies on.
  Residual: the Linux arm64 syscall ABI for the calls challoc makes.
- S242, the aarch64 `get_tp` ([record](seams/sys-tls-get_tp.md)): the
  template, the shape of every site, and the four-instruction sequence
  for each of the 12 register assignments, with the relocations
  symbolic: it reads exactly the GOT entry and then the slot at the
  thread pointer plus that offset, writes only its three outputs, no
  memory and no flags, and falls through. Its `ensures r.0 ==
  slot.value()` is the ghost slot model's premise (theorem 15) together
  with `set`'s trusted contract (S243): Isla shows which memory is read,
  not that the token equals it. The other
  residuals: the ELF TLS initial-exec model and the loader, user
  addresses below `2^48`, the slot's alignment. Not covered: the
  local-exec code a static link produces (plan, item 3).
- S224-S226, `pac::sign`, `auth`, `strip`
  ([sign](seams/sys-pac-sign.md), [auth](seams/sys-pac-auth.md),
  [strip](seams/sys-pac-strip.md)): the templates, every PAC-family
  instruction in one of the three shapes (45 `pacda` sites in 15
  register assignments, 75 `autda` in 17, one `xpacd`), and in each of
  the 12 configurations, for every assignment: it falls through, writes
  only its destination, no memory, no flags (`pac-footprint.job.in`);
  and against the canonical `pacda x0, x1` (`autda x0, x1`, `xpacd x0`)
  run from an independent state, the same pointer, modifier, data key A
  and SCTLR.EnDA give the same result (`pac-det.job.in`,
  `pac-strip-det.job.in`). So `spec_sign` and `spec_auth` are each one
  function of `(p, modifier)` once the key, EnDA and the configuration
  are fixed for the process, the residual their docs already name (with
  the trap settings: firmware's SCR_EL3.API, Linux's HCR_EL2). The
  x86_64 bodies are unreachable: no x86_64 process satisfies
  `pac_available()`.
- S223, `axiom_auth_sign` ([record](seams/sys-pac-axiom_auth_sign.md)):
  `pacda x0, x1; autda x0, x1` gives back any `p < 2^VA_BITS` in each
  configuration, with EnDA set or clear (`pac-rt.job.in`), and
  `user_va_limit()` is at most the VA width the kernel configures (its
  definition, and `axiom_user_va_limit`, S222, for a 52-bit build on a
  48-bit CPU). ComputePAC stays uninterpreted, so the
  result holds for QARMA and for any implementation-defined algorithm.
  The x86_64 half needs no tool: `spec_sign` and `spec_auth` are
  unconstrained there, and the identity satisfies the axiom. Its ledger
  tool moved from "none" to Isla.

The PAC results rest on two residuals beyond the process's fixed key and
configuration. First, `ComputePAC` is a function of its four arguments:
read from the model's source (it and its callees read no architectural
register; its only global, the round-constant array `RC`, it writes with
constants before reading; locals are filled before use), and checked on
eight concrete inputs, where two copies with their own undefined values
and `RC` contents agree (`pac-pure.job.in`); only the two substitution
functions branch on the data, so for every other callee that covers all
inputs. Second, FEAT_PAuth2, FEAT_FPAC and FEAT_CONSTPACFIELD (Armv8.6
and later, in no snapshot `run.sh` uses) are not modelled, and the
reference aarch64 machine (C4A, Neoverse V2) has FEAT_FPAC: there the
results hold by the architecture's definition, not by a check (plan,
item 2). Not covered at all: 52-bit VA with 4K or 16K pages (FEAT_LPA2).

The aarch64 `syscall` items share file and name with the x86_64 ones, so
the ledger takes `file:name#N` for the Nth seam of a name in a file
(`verus/seams.sh`, `seams.overrides`).

## Plan

In order of cost; the estimates are for one person with the toolchain
installed.

1. **The x86_64 halves** (S227-S233 and the halves of the mixed seams
   S1, S103, S104, S199, S200, S221, S241, S243): an x86 model in Isla's
   IR format: the Sail compiler with the `isla-sail` plugin (OCaml,
   opam), `sail-x86-from-acl2`, and an Isla configuration for it. Two to
   four days, with a risk: Isla's x86 support is older than its Arm
   support and may need work. With it, the eight mixed seams' notes
   become records: their aarch64 halves are already checked.
2. **FEAT_PAuth2 and FEAT_FPAC.** The v9.4 snapshot (`armv9p4.ir.gz`)
   has both; its footprints start with the model's reset sequence, so
   `run.sh` needs its own configuration for it (the PAC jobs carry over
   with `--abstract ComputePAC`). Half a day to a day.
3. **Read the linked code.** `run.sh` reads the aarch64 static library
   before the final link. The shipped `cdylib` is a separate fat-LTO run
   and may choose other registers: reading the linked `.so` (concrete
   immediates, the symbol table for the GOT address) needs the cross
   linker the coordinator has. A static link of `libchalloc.a` into an
   executable relaxes the TLS sequence to local-exec (`movz`/`movk` with
   `R_AARCH64_TLSLE_MOVW_TPREL_G1`/`G0_NC`), which needs its own job and
   an executable to read. A day for both.
4. **Islaris** in place of `check.rs`: opam, Coq, Iris and Islaris, then
   one Islaris specification per job; the traces are the same. Two or
   three days, most of it the first install. The relational PAC jobs
   need Islaris's product construction or a hand proof.
5. **CI**: a job that builds Isla (a minute or ten, network willing),
   fetches the snapshot and runs `run.sh` (about four minutes after the
   build on 16 CPUs; the PAC footprints dominate and scale with CPUs).
