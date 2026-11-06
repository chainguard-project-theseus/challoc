# The premises Kani statuses name

A Kani harness runs a trusted item's own body on the inputs and statics
the harness sets up. What the C program stores in those statics, and
what the program's own callbacks, and the functions of the C++ runtime
it links, do while challoc calls them, and what
glibc's loader and the Linux kernel place at the addresses the item
reads, are outside its reach. A seam whose harness covers its whole
contract except for such facts is `discharged on premises (Kani:
<harness path>; premises: <what>)` (verus/SEAMS.md, "Status"), and
`<what>` names them by the labels below: C labels for the C program, L
labels for the loader and the kernel.

## Premises

Each premise is a fact about what the C program that calls challoc
stores in, or passes through, challoc's exported C symbols, or about
what the program's own callback, or a function of the C++ runtime it
links, does while challoc calls it, as a K premise of
`verus/isla/seams.md` states the kernel's side of a system call. The
harness assumes it in the values it stores before it runs the item, in
running the item on one thread, and in running a recording stand-in in
place of the program's callback or the runtime's function; its comment
states those values.
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
- **C3, the program's callbacks.** The `write_cb` a program passes to
  `malloc_stats_print` or `je_malloc_stats_print` is null or the address
  of a function of type `void (*)(void *cbopaque, const char *s)`. That
  function, and the hook the program stores in `malloc_message` or
  `je_malloc_message` (C1), each keep the C allocation contract while
  challoc calls them: each reads `s` only up to its NUL and only until
  it returns, writes no byte through `s`, returns to its caller (it
  neither unwinds nor `longjmp`s out of the call, is not cancelled and
  does not call `pthread_exit` during it), and any call it makes into
  challoc passes only pointers it owns and frees only live blocks.
  challoc calls them, when not null, on the thread that called the
  report, with a NUL-terminated buffer on challoc's stack and without
  disabling cancellation: the `write_cb` with the program's `cbopaque`
  (`api::print::call_back`), and, when `write_cb` is null, the hook with
  a null `cbopaque` (`api::jemalloc::call_hook`). A write through `s`,
  or a read of it after the call returns, is the program's use of memory
  it does not own. Each harness runs its item with a recording callback
  in the program's place. Source: jemalloc(3), `malloc_stats_print` (it
  writes the statistics through `write_cb`, passing it `cbopaque`, or
  through `malloc_message` when `write_cb` is null) and `malloc_message`
  (the function jemalloc's messages go to, which a program may
  override); the C allocation contract (`verus/RESULTS.md`, "Trust
  base") for `s` and the calls into challoc; the Rust Reference,
  "Behavior considered undefined" (unwinding past a frame that does not
  allow it, as from a function called through a `"C"` function
  pointer). That neither callback `longjmp`s out of the call is assumed,
  with no source.
- **C4, the program's `new_handler`.** The handler the C++ runtime's
  `std::get_new_handler` returns (C5), when not null, is the address of
  a function of type `void (*)()` that the program installed with
  `std::set_new_handler`. While challoc calls it, it keeps the C++
  contract of a `new_handler` and the C allocation contract: it makes
  more storage available and returns, throws `std::bad_alloc` or an
  exception of a type derived from it, or ends the program without
  returning; it does not `longjmp` out of the call, is not cancelled and
  does not call `pthread_exit` during it; and any call it makes into
  challoc passes only pointers it owns and frees only live blocks.
  challoc calls it from `operator new`'s failure loop (`api::cxx::oom`),
  on the thread whose `operator new` failed, with no arguments, after
  the failed attempt has returned and without disabling cancellation:
  the throwing forms directly (`api::cxx::call_handler`), the
  `nothrow_t` forms through `challoc_call_new_handler` in
  `src/api/cxx_handler.cpp` (`api::cxx::call_handler_caught`), which
  catches what it throws. A throw from a handler the throwing forms call
  unwinds through `oom` (and, for the aligned forms, their operator's
  frame) to the caller of `operator new`. The Rust Reference makes that
  unwind undefined behaviour; it works because the frames rustc emits
  have unwind tables and no personality routine (the `api::cxx` module
  documentation), which is a fact about challoc's compiled code, not
  about the program, so it is outside this premise and a status that
  rests on it stays partly checked. Each harness runs its item with a
  recording handler in the program's place. Source: ISO C++ [new.handler] (a
  `new_handler` "shall perform one of the following": make more storage
  available and return, throw `bad_alloc` or a class derived from it, or
  terminate without returning) and [new.delete.single] (the throwing
  `operator new` loops: it tries to allocate, throws `bad_alloc` when
  no `new_handler` is installed, and otherwise calls the current one and
  repeats when it returns); the C allocation contract
  (`verus/RESULTS.md`, "Trust base") for the calls into challoc. That
  the handler does not `longjmp` out of the call, is not cancelled and
  does not call `pthread_exit` during it is assumed, with no source.
- **C5, the C++ runtime's `std::get_new_handler`.** A non-null address
  the loader resolves challoc's weak reference `_ZSt15get_new_handlerv`
  to (the GOT entry `api::cxx::get_new_handler_addr` loads, which P8 of
  `verus/isla/seams.md` says holds the address of the definition symbol
  resolution picks; `api::cxx::is_get_new_handler`) is that of the C++
  runtime's `std::get_new_handler()` (libstdc++'s, or libc++abi's, which
  is assumed to do the same, with no source): a
  function with the C calling convention (the Itanium C++ ABI gives a
  non-member function the platform's C convention) that takes no
  arguments, does not throw, and returns null or the handler the program
  last installed with `std::set_new_handler`, read with a load that
  races with no `std::set_new_handler` on another thread. A program that
  defines that symbol itself, interposing on the runtime's, is outside
  this premise unless its definition does the same. challoc only calls
  the address, as an `asm!` output turned into a function pointer, and
  reads no byte through it. The harness runs the item with a stand-in,
  a Rust function, that records its call and returns none or one of two
  handlers. Source: ISO C++ [get.new.handler] (it returns the current
  `new_handler`, which may be a null pointer value) and
  [handler.functions] ("Calling the set_* and get_* functions shall not
  incur a data race", and a `set_*` call synchronizes with later calls
  of the same `set_*` function and of the corresponding `get_*`
  function); libstdc++ `libsupc++/new_handler.cc`
  (`std::get_new_handler`, `noexcept`: an `__atomic_load` with
  `__ATOMIC_ACQUIRE`, or a load under a mutex where pointers are not
  lock-free) and `libsupc++/new` (`new_handler get_new_handler()
  noexcept;`), as read on gcc's master branch on 2026-11-06.

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
