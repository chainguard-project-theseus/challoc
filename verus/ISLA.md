# Isla: the inline-assembly seams

Verus sees only the signatures of challoc's inline assembly: raw
syscalls, the TLS slot and the thread pointer, GOT loads of weak
symbols, pointer authentication and the trap instruction.
[Isla](https://github.com/rems-project/isla) symbolically executes
machine code against a Sail ISA model (here the Armv8.5-A model
translated from Arm's ASL) and prints, per instruction, every path as a
trace of register and memory events with SMT definitions.
`verus/isla/run.sh` finds every instance of each checked template in the
compiled code, traces each distinct instruction, and checks each job (a
composition of traces and the properties it must have) with
`verus/isla/check.rs` and Z3.

[isla/seams.md](isla/seams.md) lists the 8 seams this checks (the
aarch64 `raw::syscall0` to `syscall6` and `tls::get_tp`), the contract
of each, what the runs establish against it and the residual
assumptions; the other inline-assembly seams stay trusted. Their
overrides lines under `verus/ledger/` give each tool Islaris/Isla and
cite `Isla: verus/isla/seams.md`, and run.sh fails when the two
disagree. None of the 8 is discharged: the runs end at the system
call's exception, before the kernel's side of the call, and show which
memory `get_tp` reads, not that the slot holds the cell's value, so
each is `trusted (partly checked, Isla: verus/isla/seams.md; not
checked: ...)` (verus/SEAMS.md, "Status").

The harness composes the per-instruction traces itself and proves
the resulting conditions with Z3, which is weaker than an
[Islaris](https://github.com/rems-project/islaris) proof in Coq/Iris: no
proof certificate, and the composition is `check.rs`'s own (part of the
trusted base). It drops trace events it cannot express in SMT (struct
and enum values, `assume-reg`, unknown events), which only weakens path
conditions or fails the check, never passes one (The checker, below).

## Running

```sh
make isla                                 # the aarch64 seams
sh verus/isla/run.sh -a arm/libchalloc.a  # a prebuilt aarch64 library
```

`make isla` runs `sh verus/isla/run.sh`, which needs the tools below on
`PATH` and builds the release static library for aarch64 with `cargo
rustc --target aarch64-unknown-linux-gnu`: natively on aarch64, on
another host with the aarch64 standard library installed (rustup's
`rust-std`), or from a library built elsewhere and passed with `-a`.
Isla itself runs on either host. `ISLA_CARGO_FLAGS` passes cargo flags,
such as `--no-default-features --features quantum8`. The header of
`run.sh` lists its other variables.

## Setup

The Chainguard packages carry the whole toolchain:

| package | what |
|---|---|
| `isla` | rems-project/isla e9b5d945: `isla-footprint`, the configurations in `/usr/share/isla/configs` |
| `isla-snapshots` | isla-snapshots d8b31014: `armv8p5.ir` in `/usr/share/isla-snapshots` |
| `z3` | the solver `isla-check` calls |
| `rust-1.98`, `build-base` | the library build and `isla-check` |
| `llvm-21` | `llvm-objdump` and `llvm-ar` |

Things that cost time on the first install, and that run.sh handles:

- `isla-footprint -x` takes the opcode's bytes in memory order
  (`d4000001`, `svc #0`, is `-x -i 010000d4`).
- The configuration's default toolchain must name tools on `PATH` even
  when only opcodes are given (run.sh points them at `true`).
- `armv8p5.ir`'s footprints are the instruction alone with its PC update.

## The harness

`verus/isla/run.sh` takes the library (built or given), then:

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
6. runs the negative controls (below).

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
reason fails the run. On the real traces they break one fact per
job: a wrong syndrome or EL; a memory read where there is none; an
exception instruction claimed to fall through; a TLS load's GOT load one
entry off or without its relocation's constraint or the slot's
alignment; an `add` claimed to write nothing. On synthetic traces
(`controls/`) they check the checker itself: a read after an
inexpressible write, a write through a field accessor, a flags write
under `flags-preserved`, a `fallthrough` continuing at PC + 8, a write
before a `stop` call, a `stop` call that is not the last event, a user
register written on a `fault` exception path, an exception path with no
`assert-exc`, a PC write without `__BranchTaken` under `pc-step`.

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
FN`. `isla-check --same A B` compares two trace files up to the order of
their paths, struct field order and the type arguments of constructor
names.

## Not covered

- **The linked code.** run.sh reads the static library before the final
  link. The shipped `cdylib` is a separate fat-LTO run and may choose
  other registers, and a static link of `libchalloc.a` into an
  executable relaxes the TLS sequences to local-exec (aarch64
  `movz`/`movk`), which need their own jobs.
- **Islaris** in place of `check.rs`: one Islaris specification per job
  on the same traces.
- **The other inline-assembly seams** ([isla/seams.md](isla/seams.md)):
  the x86_64 code, the TLS store, the GOT functions, `rseq`, the trap
  and pointer authentication stay trusted, and the empty templates
  (`/* {0} */` register hints) have no instruction to trace.
