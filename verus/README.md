# Verifying challoc with Verus

challoc's own source is verified in place: a function inside a `verus! { ... }`
block is checked by [Verus](https://github.com/verus-lang/verus) for every call
it makes and every operation it performs (no arithmetic overflow, no division
by zero, no out-of-bounds index, no reachable `panic!`, termination of every
loop and recursion), plus the `requires`/`ensures` contract it states. Ghost
code (`spec`, `proof`, `requires`, `ensures`, invariants) is erased from
ordinary `cargo build`s, so the shipped library is unchanged.

## Running

```
# Verus 0.2026.10.18.7d82994 matches vstd =0.0.0-2026-10-18-0147 in Cargo.toml.
cargo verus verify                       # quantum16
cargo verus verify --no-default-features --features quantum8
cargo verus verify --target aarch64-unknown-linux-gnu
```

Ordinary builds and tests are unchanged:

```
cargo build --release
cargo test
```

## What counts as verified

A function counts when it sits inside `verus!`, is not
`#[verifier::external_body]` or `#[verifier::external]`, and its body has no
`assume(...)` or `admit()`. Everything a verified function relies on without
proof is listed in `src/vspec/` (specifications of `core` functions vstd does
not cover, and trusted stand-ins for modules not yet verified) and in the
`external_body` functions, which are the trusted base.

## Conventions

- Keep the executable code as it is. A rewrite needed only to get through
  Verus (an iterator chain turned into a `while` loop, say) must be
  behaviour-preserving, and must leave the release library's machine code
  for the hot set (`malloc`, `free`, `calloc`, `realloc`, `sdallocx`,
  `free_sized` and everything inlined into them) unchanged:
  `scripts/codegen-diff.sh` must report `changed 0` there
  (`CONTRIBUTING.md`, "Performance rules").
- `const` items computed by `const fn`s inside `verus!` are `exec const`;
  `static`s are `exec static`.
- Every loop needs `invariant` and `decreases`.
- `std` atomics keep their orderings: vstd specifies them (any value can be
  returned), which is enough for safety. The Verus memory model is
  sequentially consistent; nothing here proves the `Relaxed`, `Acquire` or
  `Release` orderings right.
- Specifications of `core` functions vstd lacks go in `src/vspec/`, each
  one checked against the `core` documentation.
- `slot_word.rs` is a standalone spike (ghost slot tokens over one bitmap
  word); `verus slot_word.rs` checks it.
