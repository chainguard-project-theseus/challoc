# Isla: the inline-assembly seams

Verus sees only the signatures of challoc's inline assembly:
raw syscalls, the TLS slot and the thread pointer, GOT loads of
weak symbols, pointer authentication and the trap instruction.
[Isla](https://github.com/rems-project/isla) symbolically executes
machine code against a Sail ISA model (here the Armv8.5-A model
translated from Arm's ASL, for pointer authentication also the Armv9.4-A
one, and the x86-64 model translated from the ACL2 x86isa model)
and prints, per instruction, every path as a trace of register and
memory events with SMT definitions. `verus/isla/run.sh` finds every
instance of each checked template in the compiled code, traces each
distinct instruction, and checks each job (a composition of traces
and the properties it must have) with `verus/isla/check.rs` and Z3.

[isla/seams.md](isla/seams.md) lists the 28 seams this checks (the
inline-asm seams with an instruction to trace, and `axiom_auth_sign`),
the contract of each, what the runs establish against it and the
residual assumptions. Their overrides lines under `verus/ledger/` give
each tool Islaris/Isla and cite `Isla: verus/isla/seams.md`, and run.sh
fails when the two disagree. The "Platform premises" of isla/seams.md
states, each with its source, what the runs assume about the build, the
loader, Linux and the CPU (P1 to P13, the runs' bounds: verus/SEAMS.md,
"Status") and what the kernel does after a trace ends (K1 to K3). 7 of
the 28 are `discharged (Isla: verus/isla/seams.md)`: the three GOT
functions with no `ensures`, `rseq::symbols` and `thread_pointer`,
`strip` and `axiom_auth_sign`, whose contracts rest on nothing but their
runs and those bounds; for the code, that is the static library's, not
the shipped `cdylib` (P1). 18 are `discharged on premises (Isla:
verus/isla/seams.md; premises: ...)`, each naming the platform premises
it rests on beyond them: the kernel's side of a system call (K1), of the
trap and of an `autda` that faults (K2); the kernel keeping the PAC key
and EnDA fixed (K3); and the loader's fill of a GOT entry (P8), which
`throw_bad_alloc_addr`'s `ensures` states. The TLS accessors `get`,
`get_tp` and `set` are `trusted (partly checked, Isla:
verus/isla/seams.md; not checked: ...)`: besides P8 they rest on the TLS
slot model's premise and `TlsCell::tp`, premises of Verus's own model.

The harness composes the per-instruction traces itself and proves
the resulting conditions with Z3, which is weaker than an
[Islaris](https://github.com/rems-project/islaris) proof in Coq/Iris: no
proof certificate, and the composition is `check.rs`'s own (part of the
trusted base). It drops trace events it cannot express in SMT (struct
and enum values, `assume-reg`, unknown events), which only weakens path
conditions or fails the check, never passes one (The checker, below).

## Running

```sh
make isla                                 # both targets
ISLA_TARGETS=aarch64 make isla            # one target
sh verus/isla/run.sh -a arm/libchalloc.a  # a prebuilt aarch64 library, x86_64 built
```

`make isla` runs `sh verus/isla/run.sh`, which needs the tools below on
`PATH` and builds the release static library for each target with
`cargo rustc --target <target>-unknown-linux-gnu`: natively on each
architecture, or for the other one with its standard library installed
(rustup's `rust-std`), or from a library built elsewhere and passed with
`-a` (aarch64) or `-x` (x86_64). Isla itself runs on either host. For
x86_64 it also fetches rems-project/sail-x86-from-acl2 at
`0c6c023dccc7c20d8d9dbfc481f01d9bb49d62bf` and builds the x86 model
(`verus/isla/x86/build-ir.sh`, about 15 seconds) unless `ISLA_X86_IR`
names one. `ISLA_CARGO_FLAGS` passes cargo flags, such as
`--no-default-features --features quantum8`. The header of `run.sh`
lists its other variables.

## Setup

The Chainguard packages carry the whole toolchain:

| package | what |
|---|---|
| `isla` | rems-project/isla e9b5d945: `isla-footprint`, `isla-preprocess`, the configurations in `/usr/share/isla/configs` |
| `isla-snapshots` | isla-snapshots d8b31014: `armv8p5.ir`, `armv9p4.ir.gz` in `/usr/share/isla-snapshots` |
| `sail`, `isla-sail` | Sail 0.20.3 and Isla's `isla-sail` plugin (`/usr/lib/isla-sail/plugin.cmxs`), for the x86 model |
| `z3` | the solver `isla-check` calls |
| `rust-1.98`, `build-base` | the library build and `isla-check` |
| `llvm-21` | `llvm-objdump` and `llvm-ar` |

Things that cost time on the first install, and that run.sh handles:

- `isla-footprint -x` takes the opcode's bytes in memory order
  (`d4000001`, `svc #0`, is `-x -i 010000d4`).
- The configuration's default toolchain must name tools on `PATH` even
  when only opcodes are given (run.sh points them at `true`).
- `armv8p5.ir`'s footprints are the instruction alone with its PC update.
  `armv9p4.ir`'s default footprint function starts with the model's reset
  sequence and its footprints end before the PC update: run.sh uses its
  `isla_footprint_no_init` function (no reset), and the checker applies
  the PC update (`pc-step`). That function also swallows an UNDEFINED
  abort without a trace event, so under `pc-step` each instruction names
  the register it writes when it completes.
- In `armv9p4.ir` the system registers are structs (`-R
  SCR_EL3.bits=...`, but `VBAR_EL1` plain), and its feature flags take
  effect only from the configuration file: the model copies them into
  `FeatureImpl` before `-R` values apply, and its initialisers turn on
  many Armv8.6-and-later features unless the file pins them. EDSCR must
  be pinned to Non-debug state, or every exception path forks into
  halting-debug ones.
- Parsing the 156 MB text model is most of a trace's time: run.sh runs
  `isla-preprocess` once into the serialized `.irx` form, which loads in
  half the time, and compares four of its traces (one per configuration
  file, and an exception entry) with the text model's.
- The model's QARMA computation, unfolded symbolically, does not finish
  in ten minutes; `--abstract ComputePAC` makes it an `abstract-call`
  event and each trace takes two seconds. A symbolic TCR is rejected
  ("symbolic bit-vector length", from the VA size), so TCR is pinned per
  VA size, and `--reset-constraint` does not make a constrained value
  concrete.

### The x86_64 model

No prebuilt x86 model exists for Isla: isla-snapshots carries Arm,
RISC-V and Morello models, and Isla's `configs/` has no x86 file.
rems-project/sail-x86-from-acl2 (the ACL2 x86isa model machine-translated
to Sail) builds into one with Sail and the `isla-sail` plugin.
`x86/build-ir.sh` follows the upstream `x86.ir` recipe (`model/Makefile`):
the same model files in the same order, with the four
`test-generation-patches/` spliced in (`bang_memi.sail`, `lahf.sail`,
`logcount_alt.sail` and `slices.sail`, none of which defines `rb`,
SYSCALL, a segment accessor or an MSR accessor), plus the entry below.

The model's instruction fetch reads memory byte by byte through its own
`rb`, where Isla's Arm models take the opcode as an argument. The entry
(`verus/isla/x86/`) gives it one: `isla_rb.sail` is spliced over `rb`
and serves fetches (`":X"`) from a 128-bit `isla_opcode` relative to the
`rip` the step started at (a fetch outside it is a model error);
`isla_entry.sail` defines `isla_footprint(opcode)`, which records the
opcode and the fetch base, emits Isla's cycle marker (where
`isla-footprint -s` starts the trace; without it every register event is
hidden as initialisation), runs `x86_fetch_decode_execute` and turns any
Sail exception (the model's `x86_fault`, `x86_model_error`) into a write
of `isla_fault`; `isla_regs.sail` declares the three registers.
`x86.toml` sets the model's mode flags: the system view (`app_view`
false: segmentation, paging and the SDM's SYSCALL rather than the
application view's emulated system calls), not halted or faulted;
everything else is symbolic. `isla-footprint -x` takes at most 8 bytes,
so run.sh passes the 16 bytes with `--partial`, most significant first.
It runs with `--pessimistic`: a Sail assertion that can fail is an
error, not a path condition.

CI runs the x86_64 half only after landing, in the `isla-main` check,
which gates nothing: the presubmit `isla` check runs the aarch64 half
alone until the x86 model is packaged (#397), so a change that breaks
the x86_64 check shows up after it merges, not before.

From a fully symbolic state the model explores compatibility mode and
the other modes for every data access and fault path, and in its system
view it walks symbolic page tables, so the GOT loads, the TLS accesses,
`mov fs:[0]` and `ud2` do not finish in ten minutes. For those jobs
`run-x86_64.sh` (section 13) makes three changes, each a premise in
[isla/seams.md](isla/seams.md):

- **The entry calls `reset_registers`.** Isla applies function
  assumptions and reset constraints at the model's call of its
  `reset_registers` builtin, which Isla's Arm models make at reset and
  the x86 entry does not. run.sh patches the IR: one declaration and one
  call in `isla_footprint`, just after the cycle marker (so its events
  are in the trace), with the function's later jump targets moved past
  the two new instructions; it fails if the entry is not the shape it
  patches. With no assumption to apply the call does nothing: the patched
  model's `syscall` traces are the unpatched model's (`isla-check
  --same`).
- **64-bit mode by function assumption.** `--fun-assumption
  'n64_bit_modep((_ unit)) => true'`: the model's test for 64-bit mode
  (IA32_EFER.LMA and CS.L, a pure read of the two) answers true without
  running. The `syscall` job assumes the same two bits, and `syscall`
  traced with the assumption, in the system view, still meets that job.
  The trace records the assumption and each use, and a job must name it
  (`fun n64_bit_modep true`): the checker fails any other.
- **The application view** (`app_view` true in a copy of `x86.toml`):
  linear addresses are the memory's, with no page walk.

The model reads and writes memory a byte at a time, so the x86 jobs name
each byte (`mem.1.addr` to `mem.8.addr` for an 8-byte load) and assemble
values little-endian. The longest job, `rseq::symbols`'s seven
instructions, is why `isla-check` judges the children of a prefix in
parallel (`ISLA_CHECK_JOBS` threads).

## The harness

`verus/isla/run.sh` takes the libraries (built or given), then:

1. checks each seam in `isla/seams.md` against the ledger: the
   overrides lines citing `Isla: verus/isla/seams.md` name exactly its
   seams, each ledger entry's contract is the one seams.md
   quotes, and the item sits under the `#[cfg(target_arch = ...)]` of
   the architecture its heading names (or under none, for `both`; the
   `cfg` is the one on the top-level item, on the `mod` an indented item
   sits in, or on the indented item itself; any other `cfg` naming
   `target_arch` fails). `sh verus/seams.sh --check` must pass, so the
   ledger shows the statuses the overrides give and each seam's `asm!`
   templates are the ones `templates.txt` lists: every `asm!` call in
   the seam's item (verbatim up to whitespace) is a line of
   `templates.txt` under its file and function, every line is such a
   call, an exec item has at least one, and no string literal in the
   item names a trapping or system-register instruction (`svc`, `hvc`,
   `brk`, `ud2`, `mrs`, ...). That part needs no Isla tool and runs
   wherever `seams.sh --check` does (`make verus`), so a changed
   template fails before anyone reruns Isla. At the end of the run,
   these checks also run on mutated copies of their inputs, each of
   which must fail for its own reason: seams.md with the aarch64
   `syscall0#2` headed `(x86_64)` (`is not the x86_64 item`), seams.md
   with that seam's contract changed (`contract differs`), and a copy of
   the ledger whose `syscall0#2` overrides line names Kani (`a tool other
   than Islaris/Isla`). And the item check must place the x86_64
   `syscall0#1`, an `fn` in a `mod` under the `cfg`, on x86_64, so it
   cannot answer aarch64 for every item.

Then, in `run-aarch64.sh`:

2. disassembles the aarch64 library (`llvm-objdump -dr`);
3. sets up the Armv8.5-A traces: Non-secure EL0 in AArch64 state, the PC
   and vector bases symbolic, the immediates the linker fills as
   symbolic opcode fields (`--partial`);
4. requires every `svc`, `hvc` and `smc` to be `svc #0` and checks
   `svc0.job` and `svc0-vhe.job`;
5. requires every relocation against `challoc_tls` to sit in the TLS
   load or store shape, and checks one load job per register assignment;
6. requires every PAC-family instruction to be `pacda`/`autda` on two
   distinct general registers or `xpacd` on one, and checks the PAC jobs
   in each of the 36 configurations of [isla/seams.md](isla/seams.md),
   in parallel, one process per CPU;
7. requires the GOT functions and `rseq::symbols` to be exactly their
   instructions and relocations, every `mrs` of TPIDR_EL0 to be one the
   jobs cover and every `brk` to be `brk #0x1`, and checks their jobs
   and the TLS store jobs;
8. runs the aarch64 negative controls (below).

And in `run-x86_64.sh`:

9. disassembles the x86_64 library;
10. requires every `syscall`, `sysenter` and `int` to be `syscall` (`0f
    05`) and checks `x86/syscall.job` on the system-view model;
11. requires every relocation against `challoc_tls` to sit in the TLS
    load or store shape (a `R_X86_64_GOTTPOFF` `movq (%rip), %R`, then
    `movq %fs:(%R), %R` or `movq %V, %fs:(%R)`); its three negative
    controls change a copy of the disassembly: the first load without
    `%fs:` or into `%rsp`, and the first store through `%rsp`;
12. runs the `syscall` negative controls;
13. requires the four GOT functions and `rseq::symbols` to be exactly
    their instructions, every `R_X86_64_GOTTPOFF` site to be in the TLS
    load or store shape (one job per register assignment, one encoding
    each), every `%fs:` memory operand to be one of those or a load of
    `%fs:0`, and every `ud2` to be `0f 0b`, and checks their jobs on the
    patched application-view model;
14. runs their negative controls.

Last, back in run.sh, each seam's `Jobs:` line in `isla/seams.md` must
name at least one job (`ARCH/JOB`, the name run.sh records each
`isla-check` of a job under) for each architecture its heading names,
and none for another (a `both` seam names `ARCH/none` for an arm with
no `asm!` call), and every named job of a target in `ISLA_TARGETS` must
have run and passed. The runners pick their jobs by instruction shape,
not from seams.md, so without this a seam could be listed, and its
template held in `templates.txt`, with no job tracing it. The line is a
claim that the job traces the seam's template (and that an `ARCH/none`
arm has no `asm!` call); run.sh checks that the job ran and passed, not
that claim. Its negative control appends a seam heading with no `Jobs:`
line to a copy of seams.md, which must fail with `names no job`.

Each negative control must fail by a judgement of the checker (a `FAIL`
line), not by not running, and for the reason it was built to cause:
`refute` takes the text that `FAIL` line must contain (`abstract call
of Fault not expressible` for the `stop` call that is not the last
event), so a control that a broken checker rejects for some other
reason fails the run. On the real traces they break one fact per job:
a wrong syndrome, return address, EL or VA bound; a register write
left out; a memory read where there is none; flags claimed preserved;
an exception instruction claimed to fall through; a precondition
of the model's SYSCALL dropped; a GOT function's or TLS load's GOT load
one entry off or without its relocation's constraint or the slot's
alignment; a store of the wrong value, or none; the thread pointer read
from the GS base;
determinism with one input left unequal; the PAuth2 XOR claimed an OR
or the v8.5 bit-54 inversion dropped; `autda` claimed never to fault
or to fault with key B's syndrome, or its v8.5 failure claimed to
return another error code; a purity spot check claimed to leave the
pointer unchanged; a job missing `pc-step`, its abstract function or
its function assumption; `udf #0`, `ud2`, `ret`, `mrs` or an `add`
claimed to complete or fall through, or to write nothing; `ud2` claimed to fault
through the model-error path or after a `rip` write. On
synthetic traces (`controls/`) they check the checker itself: a read
after an inexpressible write, a write through a field accessor, a flags
write under `flags-preserved`, a `fallthrough` continuing at PC + 8, a
write before a `stop` call, a `stop` call that is not the last event,
a user register written on a `fault` exception path, an exception
path with no `assert-exc`, a PC write without `__BranchTaken` under
`pc-step`; and `isla-check --same` on two different traces.

The PAC configurations: SCR_EL3 = 0x30401 (NS, RW, API, APK), and
twelve variants: a kernel at EL1 with HCR_EL2 = `HCR_HOST_NVHE_FLAGS`
(RW, API, APK, ATA) or a VHE kernel (E2H, TGE, RW), times Linux's TCR for
a user VA of 36 (16K pages), 39 (4K), 42 (64K), 47 (16K), 48 (4K) or 52
bits (64K), TBI0 on; SCTLR (EnDA), the keys, pointers and modifiers
symbolic. Each in three CPU settings:

- `el1-48` and the like: `armv8p5.ir` with its v8.2-v8.5 features on
  (FEAT_PAuth is v8.3; no FEAT_EPAC, FEAT_PAuth2 or FEAT_FPAC);
- `p2-el1-48` and the like: `armv9p4.ir` as an Armv9.0-A CPU (v8.2-v8.5
  and v9.0 on), with FEAT_PAuth, FEAT_PAuth2, FEAT_FPAC and
  FEAT_FPACCOMBINE on, FEAT_EPAC and FEAT_CONSTPACFIELD off, QARMA5, the
  feature flags of Armv8.6 and later that the model's initialisers would
  turn on pinned off (`pac9_later`), EDSCR in Non-debug state, and
  `autda` traced with `--stop-at AArch64_PACFailException`: Neoverse V2's
  (C4A's) pointer authentication;
- `p2c-el1-48` and the like: the same with FEAT_CONSTPACFIELD on.

run.sh checks that each v9.4 configuration file sets each of these
features exactly once, so a changed Isla configuration fails rather than
running another CPU. The traces are made in every run and written whole
or not at all.

## The checker

`isla-check JOB` (`check.rs`, std-only Rust; its header documents the
job format) links the instructions' traces in order: a register read
(each field of a struct register, such as PSTATE, separately) equals the
value last written to it, or a shared initial value when Isla gave a
symbol; a read Isla gave as a configuration literal is left as it is; a
read of a register whose last write it could not express fails. It
conjoins the path conditions and the job's named assumptions, and for
every combination of one trace per instruction asks Z3 whether it is
feasible, depth first: a prefix that is already infeasible is not
extended. A feasible combination must be a normal path (no exception
unless the job expects one; a `fallthrough` instruction provably
continuing at its own PC + 4), write only the job's registers, read and
write memory as often as the job says, write no NZCV where it says so,
contain no event the checker does not know, and satisfy each `assert`
(its negation unsat). A job with no feasible combination fails.

A job may also run several **copies** of the machine from independent
initial states, for relational properties such as determinism, and name
an **abstract** function, whose `abstract-call` events become
applications of one uninterpreted SMT function shared by all copies.
**`pc-step`** applies the model's PC update after each instruction, for
a model whose footprints end before it; a **`stop`** instruction may end
at a call of a function the job names (Isla's `--stop-at`); and a
**`fault`** instruction falls through or takes an exception that writes
no user register and no system register but the entry's own. The x86
mode (`arch x86_64`) treats `rip` as the PC and each element of a vector
register as its own location, makes a write of `isla_fault` an abnormal
path, and adds the instruction kinds `jump`, `fallthrough LEN` and `trap
FN`. A **`fun`** line names a function assumption the traces may make
and use (the x86 jobs' `n64_bit_modep`); the checker fails a trace that
makes any other. `isla-check --same A B` compares two trace files up to
the order of their paths, struct field order and the type arguments of
constructor names.

## Not covered

- **Other pointer-authentication CPUs**: FEAT_EPAC without FEAT_PAuth2
  and FEAT_PAuth2 without FEAT_FPAC, each one more `pac9_config`
  setting and twelve configurations; 52-bit VA with 4K or 16K pages
  (FEAT_LPA2, TCR.DS), which the v9.4 model has.
- **The linked code.** run.sh reads the static libraries before the
  final link. The shipped `cdylib` is a separate fat-LTO run and may choose
  other registers, and a static link of `libchalloc.a` into an
  executable relaxes the TLS sequences to local-exec (aarch64
  `movz`/`movk`, x86_64 `mov R, imm32`) and may relax the GOT loads to
  address computations, which need their own jobs.
- **An Islaris proof.** `check.rs` composes the traces;
  no Islaris proof in Coq/Iris checks them, and the relational PAC jobs
  have no product construction or hand proof.
- **The empty templates** (`/* {0} */` register hints): no instruction to
  trace; `make emptyasm` checks that they add none ([README.md](README.md)).
