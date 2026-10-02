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
such traces in Coq/Iris. This file is the setup, the
triage of the 26 seams, the four discharged so far and the plan for the
rest. Read at d865271; line numbers are from that tree.

## Setup

What this lane installed, in a Linux VM without a package manager, on
2026-10-02:

| tool | version | how |
|---|---|---|
| Isla | rems-project/isla e9b5d94 | `cargo build --release` |
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

Three things cost time on the first install: `isla-footprint -x` takes
the opcode's bytes in memory order (`d4000001`, `svc #0`, is
`-x -i 010000d4`); the configuration's default toolchain must name tools
on `PATH` even when only opcodes are given (`run.sh` points them at
`true`); and the harness uses `armv8p5.ir`, whose footprints are the
instruction alone with its PC update (the v9.4 snapshot's start with the
model's reset sequence and end without one).

## The harness

`verus/isla/run.sh` builds the aarch64 static library from the tree
(about four minutes; `-a` takes a prebuilt one) and then:

1. checks each discharged `asm!` template against `templates.txt`
   (verbatim up to whitespace), and for each record the ledger entry
   whose status names it: same contract, and the item under the
   `#[cfg(target_arch = ...)]` of the architecture in the record's title;
2. disassembles the library (`llvm-objdump -dr`) and finds every site of
   the discharged templates: every `svc`/`hvc`/`smc` must be `svc #0`,
   and every relocation against `challoc_tls` must sit in the TLS load
   or store shape;
3. runs `isla-footprint` on each distinct instruction at Non-secure EL0
   in AArch64 state, with the PC and vector bases symbolic and the
   immediates the linker fills as symbolic opcode fields (`--partial`);
4. runs `isla-check` (`check.rs`, std-only Rust built with `rustc`) on
   each job. It links the instructions' traces in order: a register read
   (each field of a struct register, such as PSTATE, separately) equals
   the value last written to it, or a shared initial value when Isla gave
   a symbol; a read Isla gave as a configuration literal is left as it
   is; a read of a register whose last write it could not express fails.
   It conjoins the path conditions and the job's named assumptions, and
   for every combination of one trace per instruction asks Z3 whether it
   is feasible. A feasible combination must be a normal path (no
   exception unless the job expects the SVC one), write only the job's
   registers (field writes included), no memory where the job says so
   and no NZCV where it says so, contain no event the checker does not
   know, and satisfy each `assert` (its negation unsat). A job with no
   feasible combination fails;
5. runs eleven negative controls that must fail. On the real traces: a
   wrong SVC syndrome, a wrong target EL, a memory read where there is
   none, `svc` expected to fall through, the GOT load one entry off, the
   TLS load without the `ldr` relocation's constraint or without the
   slot's alignment, `add x0, x1, #3` claimed to write nothing. On
   synthetic traces (`controls/`): a read after an inexpressible write, a
   write through a field accessor, a PSTATE.N write under
   `flags-preserved`.

At d865271 it passes in about three minutes after the build: two `svc`
jobs (kernel at EL1; VHE kernel at EL2), twelve TLS load jobs (one per
register assignment the compiler chose; 70 sites), and every negative
control rejected.

## Triage

The 26 seams, by place. "GOT" is a load of a weak symbol's GOT slot,
"TLS" the `challoc_tls` slot through its initial-exec GOT entry. The
status is the ledger's; a seam with an x86_64 half needs both halves.

| # | item | x86_64 | aarch64 | status |
|---|---|---|---|---|
| S1 | `throw_bad_alloc_addr` | GOT `mov` | GOT `adrp`+`ldr` | trusted |
| S101 | `malloc_conf_addr` | GOT `mov` | GOT `adrp`+`ldr` | trusted |
| S102 | `je_malloc_conf_addr` | GOT `mov` | GOT `adrp`+`ldr` | trusted |
| S196 | `rseq::symbols` | 3 GOT `mov` | 3 GOT pairs | trusted |
| S197 | `rseq::thread_pointer` | `mov fs:[0]` | `mrs tpidr_el0` | trusted |
| S217 | `sys::trap` | `ud2` | `brk #1` | trusted |
| S220 | `pac::sign` | (no asm) | `pacda` | trusted |
| S221 | `pac::auth` | (no asm) | `autda` | trusted |
| S222 | `pac::strip` | (no asm) | `xpacd` | trusted |
| S223-S229 | `raw::syscall0..6` | `syscall` | (other item) | trusted |
| S230 | `raw::syscall0` | (other item) | `svc #0` | trusted |
| S231 | `raw::syscall1` | (other item) | `svc #0` | discharged |
| S232 | `raw::syscall2` | (other item) | `svc #0` | trusted |
| S233 | `raw::syscall3` | (other item) | `svc #0` | discharged |
| S234-S235 | `raw::syscall4..5` | (other item) | `svc #0` | trusted |
| S236 | `raw::syscall6` | (other item) | `svc #0` | discharged |
| S237 | `tls::get` | TLS `mov`+`mov fs:` | TLS load | trusted |
| S238 | `tls::get_tp` | (verified) | TLS load | discharged |
| S239 | `tls::set` | TLS `mov`+`mov fs:` | TLS store | trusted |

Per seam: the contract (from SEAMS.md), what an Isla run establishes
against it, and the residual assumption that stays trusted.

- **S1, S101, S102** (`api/cxx.rs`, `config.rs`; `#[inline(never)]`).
  Contract: `fn ..._addr() -> *const c_void`, no `ensures` ("nothing is
  claimed about the address"). Compiled: x86_64 `mov rax, [rip+slot];
  ret`, aarch64 `adrp x0, :got:sym; ldr x0, [x0, :got_lo12:sym]; ret`.
  Isla would show the function reads exactly the symbol's GOT slot,
  returns it in the result register, writes nothing else and returns to
  the link register; the aarch64 half is the TLS job with the
  `R_AARCH64_ADR_GOT_PAGE`/`LD64_GOT_LO12_NC` pair and a final `ret`.
  Residual: the ELF loader fills the slot with the symbol's address, the
  program's definition interposing, or 0 for an unresolved weak symbol
  (the doc's "or null"; System V gABI).
- **S196** `rseq::symbols` (`#[inline(never)]`). Contract: returns three
  pointers, no `ensures`. Three GOT loads, then the compiler's stores of
  the tuple through the return pointer (outside the asm). Isla: the
  three slots read, the three stores to the return area and nothing else.
  Residual: as S1, plus the Rust ABI's indirect return.
- **S197** `rseq::thread_pointer` (`#[inline(always)]`). Contract:
  returns a `usize`, no `ensures`. aarch64 `mrs xN, tpidr_el0` with
  `nomem`: Isla (the `mrs` trace is already in the TLS jobs) shows it
  writes only xN, with TPIDR_EL0. x86_64 `mov reg, fs:[0]`. Residual: the
  C library keeps the TCB's self pointer at `%fs:0` (x86_64 TLS ABI) and
  TPIDR_EL0 holds the thread pointer.
- **S217** `sys::trap`. Contract: `pub fn trap() -> !`. aarch64
  `brk #1`, x86_64 `ud2`. Isla: the instruction takes the breakpoint
  (EC 0x3C) or undefined-instruction exception and never falls through;
  for `brk` the return address is the instruction itself, so a handler
  that returns re-executes it. Residual: Linux turns the exception into
  `SIGTRAP`/`SIGILL`, whose default action ends the process; with a
  handler installed `trap` still never returns.
- **S220-S222** `pac::sign`, `auth`, `strip`. Contracts: each `requires
  pac_available()`; `sign` ensures `r == spec_sign(p, modifier)`, `auth`
  `r == spec_auth(p, modifier)`, `strip` nothing. The x86_64 bodies are
  plain Rust (the identity), not asm. aarch64 `pacda`/`autda`/`xpacd` on
  one register: Isla (the model with FEAT_PAuth on) would show each
  writes only its operand register, reads only it, the modifier and the
  data key A registers, and computes a function of exactly those (the
  uninterpreted `spec_sign`/`spec_auth` are functions of `(p, modifier)`
  given a fixed key). The same run can check the PAuth part of S219
  (`axiom_auth_sign`, now tool "none"): `autda(pacda(p, m), m) = p` for
  `p` below the VA limit. Residual: the key never changes during the
  process (the `prctl` caveats `spec_sign` names); FEAT_FPAC's faulting
  `autda` needs the v9 model.
- **S223-S229** x86_64 `raw::syscall0..6`. Contract: `unsafe fn
  syscallN(nr, a0..) -> usize`, no `ensures`. `syscall` with rax, rdi,
  rsi, rdx, r10, r8, r9 bound and rcx, r11 clobbered. Isla would show the
  instruction writes only rcx (the return RIP), r11 (RFLAGS) and RIP
  (IA32_LSTAR) and no memory. Residual: the Linux x86_64 syscall ABI
  (result in rax, every register but rax, rcx, r11 preserved).
- **S230-S236** aarch64 `raw::syscall0..6`. As the discharged three
  ([sys-raw-aarch64-syscall1.md](seams/sys-raw-aarch64-syscall1.md)):
  `svc #0` takes the SVC exception with no user register or memory
  written. Residual: the Linux arm64 syscall ABI.
- **S237, S239** `tls::get`, `tls::set`. Contracts: `get() -> *mut
  c_void`, `set(value)`, no `ensures`. aarch64: the TLS load (checked
  already, the same sites as `get_tp`) and the TLS store (`str` in place
  of the last `ldr`: 5 sites, 2 register assignments). x86_64:
  `mov off, [rip+challoc_tls@GOTTPOFF]; mov out, fs:[off]` (or the store).
  Residual: as S238 (initial-exec TLS, the loader's GOT entry, the
  thread pointer).
- **S238** `tls::get_tp`: discharged
  ([sys-tls-get_tp.md](seams/sys-tls-get_tp.md)).

## Discharged

Four seams, each with a five-part record under `verus/seams/` and the
status in `verus/seams.overrides`:

- S231, S233, S236, the aarch64 `syscall1`, `syscall3`, `syscall6`
  ([1](seams/sys-raw-aarch64-syscall1.md),
  [3](seams/sys-raw-aarch64-syscall3.md),
  [6](seams/sys-raw-aarch64-syscall6.md)): the template, `svc #0` at
  every site, and Isla's trace of `svc #0` at EL0 for a kernel at EL1
  and at EL2 (VHE): the SVC exception with its syndrome, return address,
  vector and target EL, no user register or memory written. Residual: the
  Linux arm64 syscall ABI for the calls challoc makes.
- S238, the aarch64 `get_tp` ([record](seams/sys-tls-get_tp.md)): the
  template, the shape of every site, and the four-instruction sequence
  for each of the 12 register assignments, with the relocations
  symbolic: it reads exactly the GOT entry and then the slot at the
  thread pointer plus that offset, writes only its three outputs, no
  memory and no flags, and falls through. Residual: the ELF TLS
  initial-exec model and the loader, user addresses below `2^48`, the
  slot's alignment. Not covered: the local-exec code a static link
  produces (plan, item 5).

The aarch64 `syscall` items share file and name with the x86_64 ones, so
the ledger now takes `file:name#N` for the Nth seam of a name in a file
(`verus/seams.sh`, `seams.overrides`).

## Plan

In order of cost; the estimates are for one person with the toolchain
installed.

1. **The other aarch64-only seams** (S230, S232, S234, S235): one line
   each in `templates.txt` and a record each; the jobs exist. An hour.
2. **Pointer authentication** (S220-S222, and S219's PAuth half): a
   configuration with `__v83_implemented` on, footprints of the three
   instructions, a determinism job (the result depends only on the
   operands and the key: two runs from the same inputs agree) and the
   round-trip job for S219. Half a day, more if the QARMA computation is
   slow in Z3 (the round trip should not need to unfold it: both sides
   compute the same PAC term).
3. **The aarch64 halves of the mixed seams** (S1, S101, S102, S196,
   S197, S217, S237, S239): `check.rs` support for memory writes (S239),
   a `ret` and a `brk` kind, the GOT relocation pair, and function-level
   jobs for the `#[inline(never)]` ones. A day. They stay trusted until
   their x86_64 halves are done.
4. **The x86_64 halves** (S223-S229 and the halves of the mixed seams):
   an x86 model in Isla's IR format: the Sail compiler with the
   `isla-sail` plugin (OCaml, opam), `sail-x86-from-acl2`, and an Isla
   configuration for it. Two to four days, with a risk: Isla's x86
   support is older than its Arm support and may need work.
5. **Read the linked code.** `run.sh` reads the aarch64 static library
   before the final link. The shipped `cdylib` is a separate fat-LTO run
   and may choose other registers: reading the linked `.so` (concrete
   immediates, the symbol table for the GOT address) needs the cross
   linker the coordinator has. A static link of `libchalloc.a` into an
   executable relaxes the TLS sequence to local-exec (`movz`/`movk` with
   `R_AARCH64_TLSLE_MOVW_TPREL_G1`/`G0_NC`), which needs its own job and
   an executable to read. A day for both.
6. **Islaris** in place of `check.rs`: opam, Coq, Iris and Islaris, then
   one Islaris specification per job; the traces are the same. Two or
   three days, most of it the first install.
7. **CI**: a job that builds Isla (about ten minutes), fetches the
   snapshot and runs `run.sh` (about three minutes after the build).
