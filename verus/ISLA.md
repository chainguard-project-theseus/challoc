# Isla: the inline-assembly seams

Twenty-seven seams in [SEAMS.md](SEAMS.md) are inline assembly (category
"inline asm", tool Islaris/Isla): raw syscalls, the TLS slot and thread
pointer, GOT loads of weak symbols, pointer authentication and the trap
instruction. Verus sees only their signatures.
[Isla](https://github.com/rems-project/isla) symbolically executes machine
code against a Sail ISA model (here the Armv8.5-A model translated from
Arm's ASL, for pointer authentication also the Armv9.4-A one, and the
x86-64 model translated from the ACL2 x86isa model) and prints, per
instruction, every path as a trace of register and memory events with
SMT definitions;
[Islaris](https://github.com/rems-project/islaris) proves properties of
such traces in Coq/Iris. This file is the setup, the triage of the 27
seams (and of `axiom_auth_sign`, a platform fact the same runs check),
all twenty-eight discharged (the 27 and `axiom_auth_sign`), and the plan
for what is left: other CPUs, the linked code, Islaris and CI. Seam
numbers and line numbers are from 8635213.

Since the port of upstream #77 two more inline-asm seams are
register-pinning hints with an empty template (`/* {0} */`, one
`inout(reg)` operand) on aarch64: `api::shared_base`
([record](seams/api-shared_base.md), the free paths' pointer to
`SHARED`) and `class::table::word_index`
([record](seams/class-table-word_index.md), the free bit test's word
index). An empty template assembles to no instruction, so there is
nothing for Isla to trace: `run.sh` checks their templates (section 1,
`templates.txt`), and since package EMPTYASM `verus/emptyasm/run.sh`
(which `run.sh` runs after section 1) checks that each such block adds
no instruction to the shipped code: in the assembly LLVM prints beside
the shipped build's object (from the same module; the object stays
byte-identical), every instance of the block, found by its `.loc` line,
holds only the template reprinted as a comment by LLVM's assembler
parser, and the call compiled alone into an object adds no byte. The
operand then comes out of the one register it went in (rustc's
`inout`). Both records are discharged. `sys::opaque`, the hint
upstream #78 added and #77 replaced with `shared_base`, is gone.
Both items are split per architecture (`#[cfg(target_arch)]`): the
x86_64 ones have no `asm!` and verify, so these records are about the
aarch64 items alone.
`layout::regions::class_entry`'s x86_64 hint is the same shape; since
package P7 it is its own item, `layout::regions::entry_barrier`, pinned
and checked the same way ([record](seams/layout-regions-entry_barrier.md),
discharged), and `class_entry` around it is verified.
`heap::ClassLine::guard_word`'s x86_64 hint (upstream #101) is the same
shape too: its template is in `templates.txt`, the empty-block check
covers it, and Kani checks the 8-byte load (on a pinned copy,
`verus/kani/pins.txt`), so its
[record](seams/heap-shared-ClassLine-guard_word.md) is discharged
(tool Kani).

## Setup

What the Isla lanes installed, in a Linux VM without a package manager,
on 2026-10-02, 2026-10-03 and 2026-10-04:

| tool | version | how |
|---|---|---|
| Isla | rems-project/isla e9b5d94 | `cargo build --release` (a minute) |
| Sail models | isla-snapshots d8b3101 | git clone (`armv8p5.ir`; `armv9p4.ir.gz`, last changed in 9591cbb, from sail-arm `wip-rebase-v9.4` 111345b) |
| libz3 for Isla | Z3 4.13.4 x64-glibc-2.35 | GitHub release zip |
| z3 for isla-check | Z3 4.16.0 | the one Verus ships |
| llvm-objdump, llvm-ar | LLVM of rustc 1.95.0 | rustup `llvm-tools` |
| make, m4, opam 2.6.0, gmp, zlib, pkgconf | Wolfi packages | `.apk` files unpacked into `/` |
| OCaml | 5.2.1 | `opam switch create` |
| Sail | rems-project/sail 5f46c04 (0.20.2) | `opam pin --kind=path` |
| isla-sail plugin | Isla e9b5d94 `isla-sail/` | `make` |
| x86 model | sail-x86-from-acl2 0c6c023 | `verus/isla/x86/build-ir.sh` |

The x86 lane kept a prebuilt copy (the stripped `isla-footprint`,
`libz3.so`, `armv8p5.ir`, `x86.ir`) and the scripts that rebuild
everything from nothing outside git (`.tools/isla/`: README, `env.sh`,
`bootstrap/setup.sh`, Wolfi-specific); a later image carries the same
build under `/opt/isla`.

Not installed:

- **Islaris** (Coq 8.x, Iris, std++ and the Islaris Coq development,
  normally through opam). The x86 work below installed opam and OCaml
  (from Wolfi packages, about two minutes), so the stack is within reach,
  but Coq, Iris and Islaris were not attempted. The harness below does
  the Isla-only part: it composes the per-instruction traces itself and
  discharges the resulting conditions with Z3. That is weaker than an
  Islaris proof: no proof certificate, and the composition is
  `check.rs`'s own (part of the trusted base). It drops trace events it
  cannot express in SMT (struct and enum values, `assume-reg`, unknown
  events), which only weakens path conditions or fails the check, never
  passes one (see below).

To reproduce:

```
git clone https://github.com/rems-project/isla
git clone https://github.com/rems-project/isla-snapshots
# libz3.so (Z3 >= 4.12) in the isla checkout root, then:
(cd isla && cargo build --release)
rustup component add llvm-tools --toolchain 1.95.0
export ISLA=$PWD/isla ISLA_SNAPSHOTS=$PWD/isla-snapshots
export LD_LIBRARY_PATH=$ISLA Z3=/path/to/z3
# the x86 model ("The x86_64 model" below), then:
export ISLA_X86_IR=$PWD/x86.ir
verus/isla/run.sh [-a aarch64/libchalloc.a] [-x x86_64/libchalloc.a] [OUT]
```

Things that cost time on the first install: `isla-footprint -x` takes
the opcode's bytes in memory order (`d4000001`, `svc #0`, is
`-x -i 010000d4`); the configuration's default toolchain must name tools
on `PATH` even when only opcodes are given (`run.sh` points them at
`true`); the harness uses `armv8p5.ir`, whose footprints are the
instruction alone with its PC update, and for the PAC instructions also
`armv9p4.ir`, whose default footprint function starts with the model's
reset sequence and whose footprints end before the PC update: `run.sh`
uses its `isla_footprint_no_init` function (no reset), the checker
applies the PC update (`pc-step`), its registers are structs (`-R
SCR_EL3.bits=...`, but `VBAR_EL1` plain), its feature flags take effect
only from the configuration file (the model copies them into
`FeatureImpl` before `-R` values apply, and its initialisers turn on
many Armv8.6-and-later features unless the file pins them), and EDSCR
must be pinned to Non-debug state or every exception path forks into
halting-debug ones. Parsing the 156 MB text model is most of a trace's
time: `run.sh` runs `isla-preprocess` once into the serialized `.irx`
form, which loads in half the time; `run.sh` compares four of its
traces (one per configuration file, and an exception entry) with the
text model's. A further trap: that footprint function swallows an
UNDEFINED abort without a trace event, so under `pc-step` each
instruction names the register it writes when it completes. For the
PAC instructions:
the model's QARMA computation, unfolded symbolically, did not finish in
ten minutes, while `--abstract ComputePAC` makes it an `abstract-call`
event and each trace takes two seconds; a symbolic TCR is rejected
("symbolic bit-vector length", from the VA size), so TCR is pinned per
VA size; and `--reset-constraint` does not make a constrained value
concrete. Building in a tmpfs copy matters: the work tree is a slow
virtiofs mount, where `cp -r` of the Isla checkout took ten minutes and
each `git` command a minute or two.

### The x86_64 model

No prebuilt x86 model exists for Isla: isla-snapshots carries Arm,
RISC-V and Morello models, and Isla's `configs/` has no x86 file. The
pieces do exist elsewhere: rems-project/sail-x86-from-acl2 (the ACL2
x86isa model machine-translated to Sail; last commit 2024-11) has an
`x86.ir` target in `model/Makefile` with patches for test generation,
and rems-project/isla-testgen has an `x86.toml` and a command for a
CHERI x86 variant. Building it needs the Sail compiler with Isla's
`isla-sail` plugin, so OCaml and opam, which the VM lacked. The route
taken, with wall clock on the 16-core VM (all of it scripted in
`.tools/isla/bootstrap/setup.sh`):

1. A downloader (`dl`, Rust, `ureq`/`tar`/`zip`; there is no curl):
   46 s.
2. make, m4, opam, gmp, zlib, pkgconf, patch, diffutils, wget, rsync,
   gawk and xz from Wolfi's package index (`packages.wolfi.dev`), the
   `.apk` files unpacked into `/` by a 40-line resolver
   (`wolfi-get.sh`; there is no `apk`): 10 s, 17 MB.
3. opam with `depext=false` and no sandbox (it otherwise calls `apk`
   and `bwrap`), OCaml 5.2.1: 1 min 45 s.
4. Sail pinned from a checkout, with its dependencies (menhir, ott,
   lem, linksem, zarith, ...): 2 min 30 s; the switch is 1.9 GB.
   Isla e9b5d94's plugin still uses Sail's `Value2` module, which Sail
   removed the day after (b1721d6), so Sail is pinned at its parent
   5f46c04, and as a path (`--kind=path`; a git pin builds the branch
   tip instead).
5. Isla (`cargo build --release`): 1 min 20 s; the plugin: 1 s.
6. The model with the entry below: 15 s, `x86.ir` 10 MB.

About 7 minutes once known. The first time, dead ends included, took
70 minutes from nothing to the first checked `syscall` job.

The model's instruction fetch reads memory byte by byte through its own
`rb`, where Isla's Arm models take the opcode as an argument. The entry
(`verus/isla/x86/`) gives it one: `isla_rb.sail` is spliced over `rb`
and serves fetches (`":X"`) from a 128-bit `isla_opcode` relative to
the `rip` the step started at (a fetch outside it is a model error);
`isla_entry.sail` defines `isla_footprint(opcode)`, which records the
opcode and the fetch base, emits Isla's cycle marker (where
`isla-footprint -s` starts the trace; without it every register event is
hidden as initialisation), runs `x86_fetch_decode_execute` and turns
any Sail exception (the model's `x86_fault`, `x86_model_error`) into a
write of `isla_fault`; `isla_regs.sail` declares the three registers.
`x86.toml` sets the model's mode flags: the system view (`app_view`
false: segmentation, paging and the SDM's SYSCALL rather than the
application view's emulated system calls), not halted or faulted;
everything else is symbolic. `isla-footprint -x` takes at most 8 bytes,
so `run.sh` passes the 16 bytes with `--partial`, most significant
first. It runs with `--pessimistic`: a Sail assertion that can fail is
an error, not a path condition.

## The harness

`verus/isla/run.sh` builds the aarch64 and x86_64 static libraries from
the tree (a few minutes each; `-a` and `-x` take prebuilt ones) and
then:

1. checks each checked `asm!` template against `templates.txt`
   (verbatim up to whitespace), and for each record (every one
   `seams.overrides` marks discharged with tool Islaris/Isla; a record
   must state "Tool: Isla. Status: discharged" and carry no `@...@`
   placeholder) the ledger entry
   whose status names it: same contract, and the item under the
   `#[cfg(target_arch = ...)]` of the architecture in the record's title
   (or under none, for a title with no architecture: `sys::pac`'s items
   and the mixed seams are one item for both; the `cfg` is the one on
   the top-level item, on the `mod` an indented item sits in, or on the
   indented item itself; any other `cfg` naming `target_arch` fails),
   then runs `verus/emptyasm/run.sh` on both targets (the empty
   templates, above);
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
   the PAC instructions run in their own configurations of two models
   (below), in parallel, one process per CPU;
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
   and nothing else is known; any other abstract call fails the job.
   Four for the Armv9.4-A model: **`pc-step`** applies the model's PC
   update after each instruction (PC + 4 unless the trace set
   `__BranchTaken`, which every PC write must come with), as the model's
   `__InstructionExecute` does and its footprint function does not; as
   that function also swallows an UNDEFINED abort without a trace event,
   each instruction then names the register it writes when it completes
   (`insn FILE KIND REG`), and a falling-through trace that does not
   write it fails; an instruction of kind **`stop`** may end at a call
   of a function the job names (`stop AArch64_PACFailException #b10`,
   Isla's `--stop-at`), a path that must write nothing but the model's
   bookkeeping and no memory, call the function with the given
   arguments, and be the last of its copy (`stop.1` in SMT says whether
   it stopped; a job with a `stop` line needs a feasible path that
   stops); an instruction of kind **`fault`** falls through or takes an
   exception, whose path may write no user register, not even one the
   job lists, and no system register but the entry's own (ESR, ELR,
   SPSR, FAR, PFAR, HPFAR, PSTATE), and must satisfy the job's
   **`assert-exc`** lines (a job needs one exactly when such a path is
   feasible); and the model's `sail_take_exception` announcement (a
   primop with no effect on registers or memory) is accepted. `isla-check
   --same A B` compares two trace files up to the order of their paths,
   struct field order and the type arguments of constructor names;
5. runs negative controls that must fail (50), each of which must fail
   by a judgement of the checker (a `FAIL` line), not by not running. On
   the real traces: a
   wrong SVC syndrome, a wrong target EL, a memory read where there is
   none, `svc` expected to fall through, the GOT load one entry off, the
   TLS load without the `ldr` relocation's constraint or without the
   slot's alignment, `add x0, x1, #3` claimed to write nothing; the PAC
   round trip without the VA bound, determinism without the modifier or
   either half of the key or with SCTLR bit 26 equated in place of EnDA,
   a PAC job not naming the abstract function, a purity spot check
   claiming the PAC left the pointer unchanged, the v8.5 `pacda` formula
   without its bit-54 inversion and its failed `autda` with another error
   code; in an Armv9.4-A configuration the round trip without the VA
   bound, determinism without the modifier (of `pacda` and of `autda`) or
   either half of the key or with SCTLR bit 26 for EnDA, the PAuth2 XOR
   claimed an OR, `autda` claimed never to fault (as `fallthrough`, or
   with `not stop.1`), its fault claimed with key B's syndrome, the FPAC
   exception with another syndrome, return address or EL, a job without
   `pc-step`, and `udf #0` (which the model aborts) claimed to complete;
   the GOT function's load
   one entry off and its `ret` claimed to fall through, `rseq::symbols`
   storing the first value twice, the TLS store storing the offset or
   claimed to write nothing, `brk` with the wrong syndrome or claimed to
   fall through, `mrs` claimed to write nothing. On synthetic traces
   (`controls/`): a read after an inexpressible write, a write through a
   field accessor, a PSTATE.N write under `flags-preserved`, a
   `fallthrough` instruction continuing at PC + 8, a register written
   before a `stop` call, a `stop` call that is not the last event, a user
   register written on a `fault` exception path, an exception path with
   no `assert-exc`, and under `pc-step` a PC write without
   `__BranchTaken`; and `isla-check --same` on two different traces;
6. disassembles the x86_64 library and requires every `syscall`,
   `sysenter` and `int` in it to be `syscall` (`0f 05`), runs
   `isla-footprint --pessimistic` on it with the x86 model and checks
   `x86/syscall.job` (`arch x86_64`). The checker's x86 mode (`check.rs`
   header): `rip` is the PC, element I of a vector register (the model's
   `msrs`, segment and `zmms` vectors) is the location `R.I`, a write of
   a whole vector or struct counts per element whose new term is not the
   one the location held (an element written back unchanged is not a
   write), every write not in the job's `writes` fails, a write of
   `isla_fault` makes the path abnormal, the entry's `isla_*` registers
   are never linked between instructions, `syscall` is a `jump` (the
   last of its copy, and a job must assert where it goes, `final.rip`;
   step 8 adds `fallthrough LEN` and `trap FN`), and `pc-step` and
   `stop` are Arm-only;
7. runs ten x86 negative controls on the real trace, each of which must
   fail with a `FAIL` line naming its own reason: the return address one
   byte off, R11 and the SS selector (a vector element) left out of
   `writes`, a memory read where there is none, RFLAGS claimed
   preserved, SS's attributes without B, the job without EFER.SCE (the
   model's `#UD` path) or without the STAR bound (the model's own check
   failing; both are caught only as `isla_fault` paths, reported as a
   model fault), and the job without EFER.LMA or without CS.L (paths
   outside 64-bit mode, on which the asserts name values left
   undefined: a failure, not a Z3 error); and checks that the checker
   refuses (a load error, not a `FAIL`) a job with no `final.rip` assert;
8. runs the x86_64 halves of the mixed seams (section 12; "The x86_64
   mixed seams" below): the exact shape of the four GOT functions and
   `rseq::symbols`, every `R_X86_64_GOTTPOFF` site in the TLS load or
   store shape (one job per register assignment, one encoding each),
   every load of `%fs:0`, every `ud2` as `0f 0b`, and their jobs;
9. runs ten negative controls on those jobs, each failing for its own
   reason: the GOT function's load one entry off and its `ret` claimed
   to fall through, `rseq::symbols` storing the first value twice, a
   TLS load without its relocation's definition, a TLS store storing the
   offset, the thread pointer read at the GS base, `ud2` claimed to fall
   through, to fault through the model-error path or to write rip
   before its fault (a synthetic variant of the real trace), and a job
   that does not name the 64-bit mode assumption its traces use.

The PAC configurations: SCR_EL3 = 0x30401 (NS, RW, API, APK), and
twelve variants: a kernel at EL1 with HCR_EL2 = `HCR_HOST_NVHE_FLAGS`
(RW, API, APK, ATA) or a VHE kernel (E2H, TGE, RW), times Linux's TCR for
a user VA of 36 (16K pages), 39 (4K), 42 (64K), 47 (16K), 48 (4K) or 52
bits (64K), TBI0 on; SCTLR (EnDA), the keys, pointers and modifiers
symbolic. Each in three CPU settings, 36 configurations:

- `el1-48` and the like: `armv8p5.ir` with its v8.2-v8.5 features on
  (FEAT_PAuth is v8.3; no FEAT_EPAC, FEAT_PAuth2 or FEAT_FPAC);
- `p2-el1-48` and the like: `armv9p4.ir` as an Armv9.0-A CPU (v8.2-v8.5
  and v9.0 on), with FEAT_PAuth, FEAT_PAuth2, FEAT_FPAC and
  FEAT_FPACCOMBINE on, FEAT_EPAC and FEAT_CONSTPACFIELD off, QARMA5, the
  103 feature flags of Armv8.6 and later that the model's initialisers
  would turn on pinned off (`pac9_later`), the earlier ones as the
  initialisers and Isla's `armv9p4.toml` leave them, EDSCR in Non-debug
  state, and `autda` traced with `--stop-at AArch64_PACFailException`:
  Neoverse V2's (C4A's) pointer authentication;
- `p2c-el1-48` and the like: the same with FEAT_CONSTPACFIELD on.

`run.sh` checks that each v9.4 configuration file sets each of these
features exactly once. The traces are made in every run and written
whole or not at all.

At 64d801a with the Armv9.4-A work, the x86 `syscall` work and the
x86_64 halves of the mixed seams it passes in about fourteen minutes
after the builds on 16 CPUs (14 min 18 s): two `svc` jobs, twelve TLS
load jobs (70 sites), 2820 PAC jobs (36 instructions, the 33 site forms
and the three canonical ones, with a footprint and a determinism job in
each of the 36 configurations; in each, the round trip and the `pacda`
and `autda` formula jobs with EnDA set and clear; in each v9.4 one, the
FPAC exception job; and 24 ComputePAC purity spot checks), the check
that the serialized v9.4 model gives four of the text model's traces,
the GOT-function job, the `rseq::symbols` job, four `mrs` jobs (76
sites), two `brk` jobs, two TLS store jobs (5 sites), the `syscall` job
(283 sites; 71 traces, one feasible) and its 64-bit-mode cross-check,
the 17 x86 jobs of step 12 (the GOT functions, `rseq::symbols`, ten TLS
load and three TLS store assignments, `fs:[0]`, `ud2`), and every
negative control (71) rejected. The PAC footprints and the x86
`rseq::symbols` job (two to three minutes) dominate.

### The x86_64 mixed seams

Run on the x86 model as in step 6, the GOT loads, the TLS accesses,
`mov fs:[0]` and `ud2` did not finish in ten minutes (the x86 lane's
scouting, 2026-10-04): from a fully symbolic state the model explores
compatibility mode and the other modes for every data access and fault
path, and in its system view it walks symbolic page tables. Section 12
of `run.sh` changes three things for these jobs, and each is named in
the records:

- **The entry calls `reset_registers`.** Isla applies function
  assumptions and reset constraints at the model's call of its
  `reset_registers` builtin, which Isla's Arm models make at reset and
  the x86 entry (`x86/isla_entry.sail`) does not. `run.sh` patches the
  IR it is given: one declaration and one call in `isla_footprint`,
  just after the cycle marker (so its events are in the trace), with
  the function's later jump targets (instruction indices in the IR)
  moved past the two new instructions; it fails if the entry is not the
  shape it patches. With no assumption to apply the call does nothing:
  the patched model's `syscall` traces are the unpatched model's
  (`isla-check --same`). The patch stands in for rebuilding the IR from
  an edited entry, which needs the Sail toolchain.
- **64-bit mode by function assumption.** `--fun-assumption
  'n64_bit_modep((_ unit)) => true'`: the model's test for 64-bit mode
  (IA32_EFER.LMA and CS.L, a pure read of the two) answers true without
  running. Linux runs 64-bit tasks in 64-bit mode; the `syscall` job
  assumes the same two bits, and `syscall` traced with the assumption,
  in the system view, still meets that job. The trace records the
  assumption and each use, and a job must name it (`fun n64_bit_modep
  true`): the checker fails any other.
- **The application view** (`app_view` true in a copy of `x86.toml`):
  linear addresses are the memory's, with no page walk. Linux maps the
  GOT, the stack and the TLS block for user code; a page fault would be
  the kernel's, and the instruction restarts.

Two checker features serve these jobs: an x86 instruction may be
`fallthrough LEN` (rip provably ends LEN bytes on, so a later
instruction is the next one in memory) or `trap FN` (every normal path
is a model fault, with no rip write, whose trace calls FN, traced with
Isla's `--trace-function`; for `ud2`, `x86_fault`, the model's
architectural faults, as opposed to `x86_model_error`). The model reads
and writes memory a byte at a time, so the jobs name each byte
(`mem.1.addr` to `mem.8.addr` for an 8-byte load) and assemble values
little-endian. A job `decl`s the initial registers and memory values
its assumptions name that a fault path leaves undefined (`init.rsp`,
`mem.9.val`), so the assumptions apply in every combination. The
residual facts the jobs assume are the platform's: user addresses below
`2^47`, the GOT entries and the TLS slot 8-byte aligned, the
relocations as the psABI defines them, the return address on the stack
a user address. The `2^47` bound leaves out the 5-level-paging
mappings above it that Linux makes only when asked (`mmap` with a hint
above `2^47`), which challoc never does and the model, with 48-bit
addresses, cannot express.

The longest job, `rseq::symbols`'s seven instructions (5.1e10
combinations, all but a few hundred prefixes cut), is why `isla-check`
judges the children of a prefix in parallel (ISLA_CHECK_JOBS threads)
and asks Z3 all of a combination's properties in one run.

## Triage

The 27 inline-asm seams and `axiom_auth_sign`, by place. "GOT" is a load
of a weak symbol's GOT slot, "TLS" the `challoc_tls` slot through its
initial-exec GOT entry. The status is the ledger's; a seam with an
x86_64 half needs both halves.

| # | item | x86_64 | aarch64 | status |
|---|---|---|---|---|
| S1 | `get_new_handler_addr` | GOT `mov` | GOT `adrp`+`ldr` | discharged |
| S2 | `throw_bad_alloc_addr` | GOT `mov` | GOT `adrp`+`ldr` | discharged |
| S98 | `malloc_conf_addr` | GOT `mov` | GOT `adrp`+`ldr` | discharged |
| S99 | `je_malloc_conf_addr` | GOT `mov` | GOT `adrp`+`ldr` | discharged |
| S191 | `rseq::symbols` | 3 GOT `mov` | 3 GOT pairs | discharged |
| S192 | `rseq::thread_pointer` | `mov fs:[0]` | `mrs tpidr_el0` | discharged |
| S222 | `sys::linux::trap` | `ud2` | `brk #1` | discharged |
| S224 | `axiom_auth_sign` | (no asm) | `pacda`+`autda` | discharged |
| S225 | `pac::sign` | (no asm) | `pacda` | discharged |
| S226 | `pac::auth` | (no asm) | `autda` | discharged |
| S227 | `pac::strip` | (no asm) | `xpacd` | discharged |
| S228-S234 | `raw::syscall0..6` | `syscall` | (other item) | discharged |
| S235-S241 | `raw::syscall0..6` | (other item) | `svc #0` | discharged |
| S242 | `tls::get` | TLS `mov`+`mov fs:` | TLS load | discharged |
| S243 | `tls::get_tp` | (verified) | TLS load | discharged |
| S244 | `tls::set` | TLS `mov`+`mov fs:` | TLS store | discharged |

Per seam: the contract (from SEAMS.md), what an Isla run establishes
against it, and the residual assumption that stays trusted. Every one is
discharged; the details are in "Discharged" below and in the records.

- **S1, S2, S98, S99** (`api/cxx.rs`, `config.rs`; `#[inline(never)]`).
  Contract: `fn ..._addr() -> *const c_void`, no `ensures`. aarch64
  (`got-fn.job.in`): each function is exactly `adrp x0, :got:sym; ldr
  x0, [x0, :got_lo12:sym]; ret`; x86_64 (`x86/got-fn.job`): exactly `mov
  rax, [rip + sym@GOTPCREL]; ret`. Under the relocations' definitions
  each reads exactly the GOT entry, returns it in the result register,
  writes nothing else and returns to the caller. Residual: the ELF
  loader fills the slot with the symbol's address (the program's
  definition interposing), or 0 for an unresolved weak symbol.
- **S191** `rseq::symbols` (`#[inline(never)]`). The three GOT loads,
  then the compiler's stores of the tuple through the return-area
  pointer (x0; rdi) and the return: the three entries read in order,
  their values stored to the return area's three words, nothing else
  written. Residual: as S2, plus the Rust ABI's return area.
- **S192** `rseq::thread_pointer` (`#[inline(always)]`). aarch64: every
  `mrs xN, tpidr_el0` in the code (76 sites, 4 registers) writes only
  xN, with TPIDR_EL0; x86_64: every `mov R, fs:[0]` (one site) reads the
  8 bytes at the FS base into R and writes nothing else. Residual: the
  C library's thread pointer (TPIDR_EL0; the TCB's self pointer at
  `%fs:0`).
- **S222** `sys::linux::trap`. aarch64: every `brk` is `brk #1`; it
  takes the breakpoint exception (EC 0x3C) to EL1, or to EL2 under VHE,
  with the `brk` itself as the return address, and never falls through.
  x86_64: every `ud2` is `0f 0b`; it raises the model's architectural
  fault (`x86_fault`, #UD), writes nothing (rip included) and never
  completes. Residual: Linux sends `SIGTRAP` or `SIGILL`.
- **S224** `axiom_auth_sign`, **S225-S227** `pac::sign`, `auth`, `strip`:
  discharged (below).
- **S228-S234** x86_64 `raw::syscall0..6`: discharged (below).
- **S235-S241** aarch64 `raw::syscall0..6`: discharged (below).
- **S242, S244** `tls::get`, `tls::set`. aarch64: the TLS load (the same
  sites and jobs as `get_tp`) and the TLS store (5 sites, 2 register
  assignments); x86_64: `mov R, [rip + challoc_tls@GOTTPOFF]` then `mov
  R, fs:[R]` (68 sites, 10 registers) or `mov fs:[R], V` (5 sites, 3
  assignments). Each reads the GOT entry and then reads, or writes the
  value to, the 8 bytes at the thread pointer plus the offset, and
  nothing else. Residual: as S243, with the TLS slot model's premise.
- **S243** `tls::get_tp`: discharged (below).

## Discharged

Twenty-eight seams (all 27 inline-asm seams and `axiom_auth_sign`), each
with a five-part record under `verus/seams/` and the status in
`verus/seams.overrides`:

- S1, S2, S98, S99, S191, S192, S222, S242, S244, the mixed seams, one
  record each for both halves
  ([S1](seams/api-cxx-get_new_handler_addr.md),
  [S2](seams/api-cxx-throw_bad_alloc_addr.md),
  [S98](seams/config-linux-malloc_conf_addr.md),
  [S99](seams/config-linux-je_malloc_conf_addr.md),
  [S191](seams/rseq-symbols.md),
  [S192](seams/rseq-thread_pointer.md),
  [S222](seams/sys-linux-trap.md), [S242](seams/sys-tls-get.md),
  [S244](seams/sys-tls-set.md)): the templates, the exact compiled shape
  of every site or function on both architectures, and the jobs of
  run.sh's sections 5 and 7 (aarch64, as in "Triage") and 12 (x86_64,
  "The x86_64 mixed seams"). On x86_64: the GOT functions read exactly
  their GOT entries' 8 bytes, return them in rax and return to the
  caller (`x86/got-fn.job`, `x86/rseq-symbols.job`, the latter storing
  the three values to the return area at rdi); the TLS load and store
  read the GOT entry and then read or write the 8 bytes at the FS base
  plus the offset it holds, and nothing else (`x86/tls-load.job.in`,
  `x86/tls-store.job.in`, one job per register assignment); the
  thread-pointer load reads the 8 bytes at the FS base
  (`x86/fs0.job.in`); `ud2` faults through the model's `x86_fault`,
  writes nothing and never completes (`x86/ud2.job`). Residuals: the
  ELF loader's GOT entries and initial-exec TLS, the thread pointer,
  the Rust ABI's return area, Linux's `SIGTRAP`/`SIGILL`, the TLS slot
  model's premise for `get` and `set` (theorem 15), and on x86_64 the
  processor in 64-bit mode by function assumption and the application
  view.
- S228-S234, the x86_64 `syscall0` to `syscall6`
  ([0](seams/sys-raw-x86_64-syscall0.md),
  [1](seams/sys-raw-x86_64-syscall1.md),
  [2](seams/sys-raw-x86_64-syscall2.md),
  [3](seams/sys-raw-x86_64-syscall3.md),
  [4](seams/sys-raw-x86_64-syscall4.md),
  [5](seams/sys-raw-x86_64-syscall5.md),
  [6](seams/sys-raw-x86_64-syscall6.md)): the template, `0f 05` at every
  site (and no `sysenter` or `int`), and Isla's trace of `syscall` on
  the x86 model from a symbolic state in 64-bit mode with SYSCALL
  enabled: RCX = RIP + 2, R11 = RFLAGS, RIP = IA32_LSTAR, RFLAGS masked
  by IA32_FMASK, CS and SS from IA32_STAR with flat 64-bit code and data
  descriptors, no other register the model has and no memory written,
  no model fault. Residual: Linux's user-mode state (EFER.SCE, 64-bit
  mode, code below 2^47, canonical LSTAR) and the Linux x86_64 syscall
  ABI for the calls challoc makes; the model lacks x87, MXCSR, AVX-512
  mask, PKRU and CET state.
- S235-S241, the aarch64 `syscall0` to `syscall6`
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
  `requires may_syscall()` is the capability theorem 8 rests on (every
  stub that enters the kernel requires it); it puts no condition on the
  instruction.
  Residual: the Linux arm64 syscall ABI for the calls challoc makes.
- S243, the aarch64 `get_tp` ([record](seams/sys-tls-get_tp.md)): the
  template, the shape of every site, and the four-instruction sequence
  for each of the 12 register assignments, with the relocations
  symbolic: it reads exactly the GOT entry and then the slot at the
  thread pointer plus that offset, writes only its three outputs, no
  memory and no flags, and falls through. Its `ensures r.0 ==
  slot.value()` is the ghost slot model's premise (theorem 15) together
  with `set`'s contract (S244, discharged with the same premise): Isla
  shows which memory is read,
  not that the token equals it. The other
  residuals: the ELF TLS initial-exec model and the loader, user
  addresses below `2^48`, the slot's alignment. Not covered: the
  local-exec code a static link produces (plan, item 2).
- S225-S227, `pac::sign`, `auth`, `strip`
  ([sign](seams/sys-pac-sign.md), [auth](seams/sys-pac-auth.md),
  [strip](seams/sys-pac-strip.md)): the templates, every PAC-family
  instruction in one of the three shapes (45 `pacda` sites in 15
  register assignments, 75 `autda` in 17, one `xpacd`), and in each of
  the 36 configurations (12 of the Armv8.5-A model, 24 of the Armv9.4-A
  one with FEAT_PAuth2 and FEAT_FPAC), for every assignment: it falls
  through, writes only its destination, no memory, no flags
  (`pac-footprint.job.in`), except that with FEAT_FPAC an `autda` may
  instead end at the FPAC fault (`AArch64_PACFailException(0b10)`)
  having written nothing; and against the canonical `pacda x0, x1`
  (`autda x0, x1`, `xpacd x0`) run from an independent state, the same
  pointer, modifier, data key A and SCTLR.EnDA give the same result, and
  for `autda` with FEAT_FPAC the same fault or none (`pac-det.job.in`,
  `pac-strip-det.job.in`). So `spec_sign` and `spec_auth` are each one
  function of `(p, modifier)` once the key, EnDA and the configuration
  are fixed for the process, the residual their docs already name (with
  the trap settings: firmware's SCR_EL3.API, Linux's HCR_EL2); where
  `autda` faults it does not return, and the FPAC exception it takes is
  checked on `autda x0, x1` in each v9.4 configuration
  (`pac-fault.job.in`: EC 0x1C, the `autda` as return address, the
  kernel's vector, no user register, other system register or memory
  written). What each model computes, in terms of the abstract
  ComputePAC, is stated and checked for user pointers
  (`pac-spec-*.job.in`): v8.5 `pacda` replaces the PAC field and a
  failed `autda` returns an error code; with FEAT_PAuth2 `pacda` XORs
  the PAC into the field, and with FEAT_FPAC `autda` faults exactly when
  the field XOR the PAC is not all copies of bit 55. The x86_64 bodies
  are unreachable: no x86_64 process satisfies `pac_available()`.
- S224, `axiom_auth_sign` ([record](seams/sys-pac-axiom_auth_sign.md)):
  `pacda x0, x1; autda x0, x1` gives back any `p < 2^VA_BITS` in each
  of the 36 configurations, with EnDA set or clear, and never takes the
  FPAC fault (`pac-rt.job.in`), and `user_va_limit()` is at most the VA
  width the kernel configures (its definition, and
  `axiom_user_va_limit`, S223, for a 52-bit build on a 48-bit CPU).
  ComputePAC stays uninterpreted, so the result holds for QARMA5, QARMA3
  and any implementation-defined algorithm. The x86_64 half needs no
  tool: `spec_sign` and `spec_auth` are unconstrained there, and the
  identity satisfies the axiom. Its ledger tool moved from "none" to
  Isla.

The PAC results rest on two residuals beyond the process's fixed key and
configuration. First, `ComputePAC` is a function of its arguments: read
from each model's source (it and its callees read no architectural
register; its only global, the round-constant array `RC`, it writes with
constants before reading; locals are filled before use), and checked on
eight concrete inputs in the v8.5 model and in the v9.4 one with QARMA5
and with QARMA3, where two copies with their own undefined values and
`RC` contents agree (`pac-pure.job.in`); only the substitution functions
branch on the data, so for every other callee that covers all inputs.
Second, the CPU's pointer authentication is one of the two kinds
checked: Armv8.3-v8.5 FEAT_PAuth, or FEAT_PAuth2 with FEAT_FPAC (either
FEAT_CONSTPACFIELD setting; FEAT_FPACCOMBINE changes only the combined
instructions, which challoc does not emit), which is what the reference
aarch64 machine (C4A, Neoverse V2) has. The residual of the earlier
run, that on C4A the PAC results held by the architecture's text, is
closed for the pointer-authentication features: they are checked on the
v9.4 model. What remains for C4A is narrower: the model's CPU is an
Armv9.0-A one with every later feature off, while Neoverse V2 has a few
Armv8.6 data-processing features (FEAT_BF16, FEAT_I8MM), which the PAC
instructions and the FPAC exception entry do not consult (read, not
checked); and C4A's kernel is a guest at EL1, under the hypervisor's
HCR_EL2, for which the EL1 configurations stand (only HCR_EL2's API,
APK, TGE and E2H bear on trapping and routing, read from the model's
source). Not checked: FEAT_EPAC without FEAT_PAuth2, FEAT_PAuth2 without
FEAT_FPAC (plan, item 1), and 52-bit VA with 4K or 16K pages
(FEAT_LPA2).

The aarch64 `syscall` items share file and name with the x86_64 ones, so
the ledger takes `file:name#N` for the Nth seam of a name in a file
(`verus/seams.sh`, `seams.overrides`): `#1` is x86_64, `#2` aarch64.

## Plan

In order of cost; the estimates are for one person with the toolchain
installed.

1. **The other pointer-authentication CPUs.** Done for the reference
   machine: FEAT_PAuth2 with FEAT_FPAC runs on the v9.4 snapshot
   (Discharged). Left: FEAT_EPAC without FEAT_PAuth2 and FEAT_PAuth2
   without FEAT_FPAC, each one more `pac9_config` setting and twelve
   configurations in `run.sh` (a few minutes of run time each; the
   feature flags cannot be left symbolic, as Isla's `-I` takes only
   concrete values and `-R` applies after the model copies them), and
   52-bit VA with 4K or 16K pages, FEAT_LPA2, which the v9.4 model has
   (TCR.DS set; Linux's TCR values for it to be taken from the kernel).
   Half a day.
2. **Read the linked code.** `run.sh` reads the static libraries
   before the final link. The shipped `cdylib` is a separate fat-LTO run
   and may choose other registers: reading the linked `.so` (concrete
   immediates, the symbol table for the GOT address) needs the aarch64
   cross linker the coordinator has (the x86_64 `.so` links natively).
   A static link of `libchalloc.a` into an
   executable relaxes the TLS sequences to local-exec (aarch64 `movz`/
   `movk` with `R_AARCH64_TLSLE_MOVW_TPREL_G1`/`G0_NC`; x86_64 a `mov R,
   imm32` with `R_X86_64_TPOFF32`), which needs its own jobs and
   an executable to read. A day for both.
3. **Islaris** in place of `check.rs`: opam, Coq, Iris and Islaris, then
   one Islaris specification per job; the traces are the same. Two or
   three days, most of it the first install. The relational PAC jobs
   need Islaris's product construction or a hand proof.
4. **CI**: a job that builds Isla (a minute or ten, network willing) and
   the x86 model (about seven minutes from a bare image with the x86
   lane's `setup.sh`, or a cached copy), fetches the snapshots and runs
   `run.sh` (about fourteen minutes after the builds on 16 CPUs; the PAC footprints
   dominate and scale with CPUs). Without it the records go stale when a
   contract changes, as the four Arm records did between d865271 and
   8635213 (the `may_syscall` precondition and `get_tp`'s `TlsSlot`
   token, brought up to date since).
