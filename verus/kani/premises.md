# The premises Kani statuses name

A Kani harness runs a trusted item's own body on the inputs and statics
the harness sets up. What the C program stores in those statics, and
what glibc's loader and the Linux kernel place at the addresses the item
reads, is outside its reach. A seam whose harness covers its whole
contract except for such facts is `discharged on premises (Kani:
<harness path>; premises: <what>)` (verus/SEAMS.md, "Status"), and
`<what>` names them by the labels below: C labels for the C program, L
labels for the loader and the kernel.

## Premises

Each premise is a fact about what the C program that calls challoc
stores in, or passes through, challoc's exported C symbols. The harness
assumes it in the values it stores before it runs the item, and in
running the item on one thread; its comment states those values.
`verus/count` reads the labels of this section's bullets and of the next
section's, and fails on a Kani status that names any other.

- **C1, the hook slots.** The program stores into `malloc_message` and
  `je_malloc_message` (the same hook under jemalloc's prefixed name,
  which challoc exports as a second object) only null or the address of
  a function of type `void (*)(void *cbopaque, const char *s)`. Each
  store into either happens before every read of it by another thread;
  otherwise the store and the read are a data race. challoc reads both
  slots with volatile, non-atomic loads (`api::jemalloc::message_hook`),
  as jemalloc reads `malloc_message` with a plain load. Source:
  jemalloc(3), `malloc_message` (the function jemalloc's messages go to,
  which a program may override); ISO C11 section 5.1.2.4 (a data race is
  undefined behaviour).
- **C2, the name, `newp` and MIB.** The bytes a program passes to
  `mallctl`, `mallctlnametomib` and `mallctlbymib` as the name string
  (up to its NUL), the `newlen` bytes at `newp` and the `miblen` words at
  `mib` are initialised, and no other thread changes them while the call
  runs. A C program may pass indeterminate bytes there without undefined
  behaviour in C, but challoc's Rust reads of them require initialised
  bytes, so this is assumed of the program; a store by another thread
  during the call is a data race. The Kani-checked seams that rest on it
  are `core::slice::from_raw_parts`, in `api::mallctl::name_bytes` (the
  name's bytes before its NUL, borrowed for as long as the call's token),
  and `core::ptr::read_unaligned` and `<*const T>::read_unaligned`, in
  `api::mallctl::load`, `query_ptr` and `mib_in` (each reading inside a
  buffer its `requires` makes the caller's, `c_readable`): verified code
  uses those functions nowhere else, the helpers are used only in the
  functions that read these arguments, and they keep those contracts
  (`api::mallctl::tests::c_buffer_reads_are_listed`). Source: jemalloc(3),
  `mallctl` (`name` names the value; to write it, `newp` points to the
  new value and `newlen` is its length) and `mallctlnametomib` (it
  translates a name to a MIB, which is passed to `mallctlbymib` with its
  length `miblen`); ISO C11 section 5.1.2.4 (a data race is undefined
  behaviour).

## Loader and kernel premises

Each premise is a fact about memory that glibc or the Linux kernel sets
up at an address challoc reads but does not allocate: an address the
loader resolves, or one the kernel's ABI defines. The harness runs the
item on a stand-in for that memory, a local no larger and no more
aligned than the premise states, filled with any value (`kani::any()`),
on one thread; the premise is that the real memory is at least such
memory, as live and as free of other writers while the item runs.
An item's `# Safety` premise reduces to these facts only for the
addresses its callers pass, so a status that cites them also names, in
its `callers:` clause, the guard test that pins every call of the item,
with its arguments, to callers that pass such an address; and it rests
on the argument of "Provenance of the addresses" below (verus/SEAMS.md,
"Status"). The
Linux source paths are those of Linux v7.4-rc1, as in
`verus/isla/seams.md`; the glibc ones are those of glibc's master
branch as read on 2026-11-05 (a mirror at 2.42.9000), not a release tag.

- **L1, glibc's rseq globals.** A non-null address the loader resolves
  challoc's weak references `__rseq_offset`, `__rseq_size` and
  `__rseq_flags` to (the GOT entries `rseq::symbols` loads, which P8 of
  `verus/isla/seams.md` says hold the address of the definition symbol
  resolution picks) is that of glibc's object of that name: a
  `ptrdiff_t` (`isize`), an `unsigned int` (`u32`) and an
  `unsigned int`, each aligned for its type and live for the rest of the
  process. The loader stores the values before relocation, so before
  any of challoc's code runs, and nothing stores to them after: glibc
  places `__rseq_offset` and `__rseq_size` in `.data.rel.ro`, which the
  loader write-protects once it has relocated, and defines
  `__rseq_flags` as a `const` zero. So challoc's plain loads of them race
  with no store. This is the dynamic link, where the objects are
  ld.so's. A program that defines one of the three names itself, and so
  interposes on glibc's, is outside this premise, and so is a static
  link (`libchalloc.a` into an executable with `libc.a`), where the
  objects are the executable's and the startup code that fills them is
  not the one cited here. Source: glibc
  `sysdeps/unix/sysv/linux/dl-rseq-symbols.S` (the objects' sections,
  sizes and `.balign`, and "the dynamic linker ... will initialize the
  value the symbols point to before copy relocations take place"),
  `sysdeps/unix/sysv/linux/rseq-internal.h` ("In .data.relro but not yet
  write-protected"), `sysdeps/nptl/dl-tls_init_tp.c` (`__rseq_flags`),
  `elf/rtld.c` (`dl_main`: the relocations, then `_dl_protect_relro` of
  ld.so); the glibc manual, "Restartable Sequences".
- **L2, the rseq area.** In every thread glibc creates (the initial
  thread, and those of `pthread_create`), the thread pointer (TPIDR_EL0
  on aarch64, the word at `%fs:0` on x86_64; P10 of
  `verus/isla/seams.md`) plus `__rseq_offset` (L1) is the address of
  that thread's rseq area, which glibc allocates in the thread's static
  TLS block: at least 32 bytes, aligned to at least 32 bytes, live while
  the thread runs, whether or not the kernel took the registration. Its
  `cpu_id` field (bytes 4 to 7) is written only by glibc, in the thread
  itself before the thread runs any of challoc's code, and by the kernel,
  in the thread's own context: in its `rseq` calls and on its returns to
  user space. So a load of the field by the thread itself, between two
  of its own instructions, races with no store. A thread challoc runs on
  that glibc did not create (a raw `clone`) is outside this premise.
  Source: glibc `sysdeps/nptl/dl-tls_init_tp.c` ("An rseq area of at
  least 32 bytes is always allocated since application code is allowed
  to check the status of the rseq registration by reading the content
  of the 'cpu_id' field"), `sysdeps/unix/sysv/linux/rseq-internal.h`
  (`RSEQ_SELF`, the thread pointer plus `__rseq_offset`;
  `RSEQ_MIN_ALIGN`, 32; `rseq_register_current_thread`, glibc's stores
  to `cpu_id`); Linux `include/uapi/linux/rseq.h` (`struct rseq`,
  `aligned(32)`; `cpu_id`, "Updated by the kernel ... This field should
  only be read by the thread which registered this data structure") and
  `kernel/rseq.c` (`rseq_length_valid`, the alignment a registration
  needs); rseq(2).

## Provenance of the addresses

Kani cannot observe a pointer's provenance (verus/SEAMS.md, "Status"),
so a seam that reads through an address L1 or L2 describes rests on
this argument too, which no tool checks. challoc allocates none of that
memory: the three globals are glibc's objects in ld.so, which challoc
reaches only through `.weak` references, and the rseq area lies in
glibc's TLS block of the thread, in bytes no Rust allocation of
challoc's (a stack frame, a static, a `Box`) contains. `core::ptr`'s
documentation ("Exposed Provenance") says that memory outside the
control of the Rust abstract machine "is always considered to be
exposed, so long as this memory is disjoint from memory that will be
used by the abstract machine such as the stack, heap, and statics".
So:

- `rseq::area` makes the area's address into a pointer with
  `addr as *mut u8` (`vspec::addr::addr_to_ptr`), the cast
  `core::ptr::with_exposed_provenance_mut` stands for. The area's bytes
  are exposed, so the pointer may read `cpu_id` within them.
- `rseq::symbols` returns each global's address as an `asm!` output, the
  value of a GOT load. The Rust Reference gives an `asm!` block's memory
  accesses the rules of an FFI call (`asm.rules.mem-same-as-ffi`) and
  states no provenance for its outputs. This argument takes such an
  output as it takes a pointer an FFI function returns into memory
  outside the abstract machine: one whose provenance is that memory's,
  exposed. That step rests on how Rust treats pointers from outside
  Rust, not on L1, and a seam that cites L1 for an `asm!` output names
  it in its gloss.

A thread stack the program provides (`pthread_attr_setstack`) may lie in
memory challoc handed out, and glibc then places the thread's TLS block,
rseq area included, there. Those bytes are in challoc's own mappings,
which challoc reaches only through addresses, never through a Rust
allocation of its own, so they are exposed on the same footing. A
program written in Rust whose own Rust allocation (a `Vec` from its
global allocator) is that stack is outside this argument: the bytes are
then that program's abstract-machine memory, not disjoint from it.
