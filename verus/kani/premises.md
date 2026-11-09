# The premises Kani statuses name

A Kani harness runs a trusted item's own body on the inputs and statics
the harness sets up. What the C program stores in those statics, and
what the program's own callbacks, and the functions of the C and C++
runtimes it links, do while challoc calls them, and what
glibc's loader and the Linux kernel place at the addresses the item
reads, are outside its reach. A seam whose harness covers its whole
contract except for such facts is `discharged on premises (Kani:
<harness path>; premises: <what>)` (verus/SEAMS.md, "Status"), and
`<what>` names them by the labels below: C labels for the C program and
the runtime functions challoc calls, L labels for the loader and the
kernel.

## Premises

Each premise is a fact about what the C program that calls challoc
stores in, or passes through, challoc's exported C symbols, or about
what the program's own callback, or a function of the C runtime (glibc)
or the C++ runtime it links, does while challoc calls it, as a K
premise of `verus/isla/seams.md` states the kernel's side of a system
call. The
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
  neither unwinds nor `longjmp`s out of the call and does not call
  `pthread_exit` during it), does not re-enable cancellation, and any
  call it makes into challoc passes only pointers it owns and frees only
  live blocks. challoc calls them, when not null, on the thread that
  called the report, with a NUL-terminated buffer on challoc's stack and
  with cancellation disabled (`api::print::call_back_nocancel`,
  `api::jemalloc::call_hook_nocancel`): the `write_cb` with the
  program's `cbopaque` (`api::print::call_back`), and, when `write_cb`
  is null, the hook with a null `cbopaque` (`api::jemalloc::call_hook`).
  A write through `s`,
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
- **C6, the C++ runtime's `std::__throw_bad_alloc`.** A non-null
  address the loader resolves challoc's weak reference
  `_ZSt17__throw_bad_allocv` to (the GOT entry
  `api::cxx::throw_bad_alloc_addr` loads, which P8 of
  `verus/isla/seams.md` says holds the address of the definition symbol
  resolution picks; `api::cxx::is_throw_bad_alloc`) is that of
  libstdc++'s `std::__throw_bad_alloc()`: a function with the C calling
  convention (the Itanium C++ ABI gives a non-member function the
  platform's C convention) that takes no arguments and never returns:
  it throws a `std::bad_alloc`, or, in a libstdc++ built without
  exceptions, aborts. A program that defines that symbol itself,
  interposing on the runtime's, is outside this premise unless its
  definition does the same. challoc only calls the address, as an
  `asm!` output turned into a function pointer
  (`api::cxx::call_throw_bad_alloc`, from `api::cxx::throw_bad_alloc`),
  on the thread whose `operator new` failed, and reads no byte through
  it. The throw unwinds through `throw_bad_alloc` and its callers
  (`oom`, or, for an alignment that is not a power of two, the aligned
  operator's frame) to the caller of `operator new`; as for C4, that
  this unwind through Rust frames works is a fact about challoc's
  compiled code (the `api::cxx` module documentation), not about the
  runtime, so it is outside this premise and a status that rests on it
  stays partly checked. The harnesses run the item with a stand-in, a
  Rust function, that panics, or ends the path, where the runtime's
  function would throw. Source: libstdc++ `src/c++11/functexcept.cc`
  (the exported definition, `__throw_bad_alloc() {
  _GLIBCXX_THROW_OR_ABORT(bad_alloc()); }`) and
  `include/bits/new_throw.h` (its declaration, `void
  __throw_bad_alloc(void) __attribute__((__noreturn__));`, where the
  header does not define it inline for constexpr exceptions), as read
  on gcc's master branch on 2026-11-06.
- **C7, glibc's `pthread_key_create`.** The symbol `pthread_key_create`
  resolves to glibc's `int pthread_key_create(pthread_key_t *key, void
  (*destructor)(void *))`, the signature of challoc's declaration of it
  in `heap/shared.rs`, with `pthread_key_t` an `unsigned int`. A program
  or preloaded library that defines that symbol itself, interposing on
  glibc's, is outside this premise unless its definition does the same.
  Called by `heap::shared::ThreadKey::create` with the address of a
  live, writable `pthread_key_t` on challoc's stack and the destructor
  `heap::heap_destructor`, it returns 0 and stores the new key in
  `*key`, or returns a nonzero error number, creating no key. It
  returns to its caller, writes no memory of challoc's but `*key`, and
  makes no call into challoc while it runs. glibc claims, with a
  compare-and-swap, the first unused, usable slot of its table of
  `PTHREAD_KEYS_MAX` keys it wins, stores the destructor in the slot
  and the slot's index in `*key`, and returns 0; when it claims no slot
  it returns `EAGAIN` without writing `*key`. It allocates nothing.
  When glibc later calls the destructor (at a thread's exit, theorem 10
  of `verus/THEOREMS.md`) is not part of the call, and not of this
  premise. The harness runs the item with `pthread_key_create`
  replaced by a stand-in that records its call and the destructor,
  checks that `key` is writable, and returns any value, storing any
  key when that value is 0. Source: POSIX `pthread_key_create` ("If
  successful, the pthread_key_create() function shall store the newly
  created key value at *key and shall return zero. Otherwise, an error
  number shall be returned to indicate the error."); glibc
  `nptl/pthread_key_create.c` (`___pthread_key_create`), as read on
  glibc's master branch on 2026-11-06 (a mirror).
- **C8, glibc's `pthread_atfork`.** The symbol `pthread_atfork`
  resolves to glibc's `int pthread_atfork(void (*prepare)(void), void
  (*parent)(void), void (*child)(void))`, the signature of challoc's
  declaration of it in `api/mod.rs`: glibc links its copy into
  challoc's own object from `libc_nonshared.a`, and that copy calls
  libc's `__register_atfork` with challoc's `__dso_handle`. A program
  or preloaded library that defines `__register_atfork` itself,
  interposing on glibc's, is outside this premise unless its definition
  does the same. Called by `api::pthread_atfork_handlers` with
  `api::fork_prepare`, `api::fork_parent` and `api::fork_child`, it
  records the three as one registration and returns 0. It returns to
  its caller, reads and writes no memory challoc's Rust code owns, and
  makes no call into challoc while it runs. `__register_atfork`
  appends the handlers to a table under glibc's atfork lock, which a
  `fork` holds from its prepare handlers to its parent and child
  handlers; the return rests on the calling thread not holding that
  lock, as a thread running a `fork`'s handlers does, which is assumed,
  with no source. The table holds its first 48
  entries in place (`DYNARRAY_INITIAL_SIZE`) and grows past them with
  `malloc`, which in a process whose allocator is challoc is a call
  into challoc; a process with 48 or more live registrations before
  challoc's is outside this premise, and so is the `ENOMEM` return,
  which needs a failed `malloc`. So is the process-ending
  `__libc_fatal` on an overflow of glibc's 64-bit registration counter.
  What glibc does with the handlers at a `fork` (the prepare handlers
  in the reverse order of registration, the parent and child handlers
  in order, on the forking thread; theorem 13's premises,
  `verus/THEOREMS.md`) is not part of the call, and not of this
  premise. The harness runs the item with `pthread_atfork` replaced by
  a stand-in that records its call and the three handlers, in argument
  order, and returns any value. Source: POSIX `pthread_atfork` ("Upon
  successful completion, pthread_atfork() shall return a value of zero;
  otherwise, an error number shall be returned to indicate the error");
  glibc `sysdeps/pthread/pthread_atfork.c` (`__pthread_atfork`, "no
  definition but the one locally in the executable or DSO is used"),
  `sysdeps/pthread/Makefile` (`static-only-routines += pthread_atfork`,
  the routines built into `libc_nonshared.a`), `posix/register-atfork.c`
  (`__register_atfork`, and `__run_prefork_handlers` and
  `__run_postfork_handlers` for the lock and the order at a `fork`),
  `malloc/dynarray-skeleton.c` and `malloc/dynarray_emplace_enlarge.c`
  (the in-place table and the `malloc` past it), as read on glibc's
  master branch on 2026-11-06 (a mirror).
- **C9, glibc's `pthread_setcancelstate`.** The symbol
  `pthread_setcancelstate` resolves to glibc's `int
  pthread_setcancelstate(int state, int *oldstate)`, the signature of
  challoc's declarations of it in `api/print.rs` and
  `purge/background.rs` (its `glibc` module). A program or preloaded
  library that defines that symbol itself, interposing on glibc's, is
  outside this premise unless its definition does the same. Called by
  `api::print::cancel_disable` with `PTHREAD_CANCEL_DISABLE` (1; 0 is
  `PTHREAD_CANCEL_ENABLE`), by `api::print::cancel_restore` with any
  `state`, and by `purge::background::pthread_join` with
  `PTHREAD_CANCEL_DISABLE` before its join and with any `state` after
  it, each time with the address of a live, writable `int` on
  challoc's stack as `oldstate`, it returns to its caller, writes no
  memory of challoc's but `*oldstate`, and makes no call into challoc
  while it runs. For a `state` of 0 or 1 it sets the calling thread's
  cancelability state to `state`, stores the previous state, 0 or 1, in
  `*oldstate`, and returns 0; for any other `state` it returns `EINVAL`
  without writing `*oldstate`. It is not a cancellation point, so it
  never acts on a deferred cancel. glibc acts on a pending cancel inside
  it only when the call enables cancellation on a thread whose
  cancelability type is asynchronous; asynchronous cancellation inside
  challoc is outside its contract (`docs/design.md`, 8.1), and so
  outside this premise. It allocates nothing. The harnesses run each
  item with `pthread_setcancelstate` replaced by a stand-in that
  records its call and `state`, checks that `oldstate` is writable, and
  returns any value, storing any `int` in `*oldstate` when that value
  is 0. Source: POSIX `pthread_setcancelstate` ("The
  pthread_setcancelstate() function shall atomically both set the
  calling thread's cancelability state to the indicated state and
  return the previous cancelability state at the location referenced by
  oldstate."; "If successful, the pthread_setcancelstate() and
  pthread_setcanceltype() functions shall return zero; otherwise, an
  error number shall be returned to indicate the error.") and section
  2.9.5.2, "Cancellation Points", of its System Interfaces volume, whose
  lists do not name it ("An implementation shall not introduce
  cancellation points into any other functions specified in this volume
  of POSIX.1-2024."); glibc `nptl/pthread_setcancelstate.c`
  (`__pthread_setcancelstate`: the `EINVAL` return before any write,
  the store to `*oldstate`, and `__do_cancel` only when
  `cancel_enabled_and_canceled_and_async` holds of the new state),
  `nptl/descr.h` (`cancel_enabled_and_canceled_and_async`: enabled,
  asynchronous and cancelled, and neither exiting nor terminated) and
  `sysdeps/nptl/pthread.h` (the two values), as read on glibc's master
  branch on 2026-11-06.
- **C10, glibc's `pthread_setspecific`.** The symbol
  `pthread_setspecific` resolves to glibc's `int
  pthread_setspecific(pthread_key_t key, const void *value)`, the
  signature of challoc's declaration of it in `heap/shared.rs` (its
  `glibc` module), with `pthread_key_t` an `unsigned int`. A program or
  preloaded library that defines that symbol itself, interposing on
  glibc's, is outside this premise unless its definition does the same,
  and so is a thread challoc runs on that glibc did not create (a raw
  `clone`), whose thread pointer names no descriptor of its own.
  Called by `heap::shared::pthread_setspecific` with any key and any
  `value`, on the calling thread, it returns 0 and records `value` as
  that thread's value for the key, or returns a nonzero error number:
  `EINVAL` for a key that is not live (a key of `PTHREAD_KEYS_MAX`
  (1024) or more, or one whose sequence number in glibc's key table is
  even, never created or deleted), recording nothing. It does not read
  or write through `value`, returns to its caller, writes no `errno`
  and no memory of challoc's (its statics, its TLS slot, its stack),
  reads none of that memory, and makes no call into challoc while it
  runs. For a key below 32 (`PTHREAD_KEY_2NDLEVEL_SIZE`) glibc stores
  the value in the first block of 32 entries the thread's descriptor
  holds, allocating nothing; for a key of 32 or above it stores it in
  that key's block of 32, which the thread's first call that records a
  non-null value for a key of the block `calloc`s, and which in a
  process whose allocator is challoc is a call into challoc (it returns
  `ENOMEM` when the `calloc` fails). A call made while challoc's key
  (the one `heap::shared::ThreadKey::create` stored) is 32 or above
  and the calling thread has no block for it is outside this premise,
  and so is any other call that `calloc`s a block. What glibc later
  does with the value (passes it to the key's destructor at the
  thread's exit, theorem 10 of `verus/THEOREMS.md`) is not part of the
  call, and not of this premise. The harness runs the item with
  `pthread_setspecific` replaced by a stand-in that records its call,
  the key and the address of `value`, and returns any value. Source:
  POSIX `pthread_setspecific` ("The pthread_setspecific() function
  shall associate a thread-specific value with a key obtained via a
  previous call to pthread_key_create()"; "If successful, the
  pthread_setspecific() function shall return zero; otherwise, an
  error number shall be returned to indicate the error"; a key not
  obtained from `pthread_key_create()` is undefined there, so the
  `EINVAL` return for any other key is glibc's); glibc
  `nptl/pthread_setspecific.c` (`___pthread_setspecific`: the `EINVAL`
  checks, the first block, and the `calloc` of a second-level block),
  `sysdeps/nptl/internaltypes.h` (`KEY_UNUSED`, an even sequence
  number), `nptl/descr.h` (`PTHREAD_KEY_2NDLEVEL_SIZE`,
  `specific_1stblock`, `specific`) and
  `sysdeps/unix/sysv/linux/bits/local_lim.h` (`PTHREAD_KEYS_MAX`), as
  read on glibc's master branch on 2026-11-06 (a mirror).
- **C11, glibc's `__errno_location`.** The symbol `__errno_location`
  resolves to glibc's `int *__errno_location(void)`, the signature of
  challoc's declaration of it in `vspec/errno.rs`. A program or
  preloaded library that defines that symbol itself, interposing on
  glibc's, is outside this premise unless its definition does the same,
  and so is a thread challoc runs on that glibc did not create (a raw
  `clone`), which may share another thread's `errno`. Called by
  `vspec::errno::set_errno` and `vspec::errno::get_errno` with no
  arguments, it returns the address of the calling thread's `errno`:
  glibc's thread-local `int errno`, in libc's static TLS block of the
  thread, aligned for an `int`, live while the thread runs, the same
  address on every call by that thread, and another object than any
  other thread's. No Rust allocation of challoc's (a stack frame, a
  static, a `Box`) contains those bytes, and no thread but the calling
  one writes them. It returns to its caller, writes no memory, reads
  none of challoc's (only glibc's GOT entry for the TLS offset and the
  thread pointer), and makes no call into challoc while it runs. The
  pointer it returns may read and write that `int`: its provenance,
  which Kani cannot observe, is that of memory outside the Rust
  abstract machine, exposed ("Provenance of the addresses" below). Any
  write to `errno` by other code on the thread (a libc call in trusted
  code, a signal handler that does not restore it) between challoc's
  write and its read is outside this premise (`vspec::errno`'s
  `ThreadErrno`). The harnesses run each item with `__errno_location`
  replaced by a stand-in that records its call and returns the address
  of a Rust static, holding any value. Source: POSIX `<errno.h>`
  (`errno` expands "to a modifiable lvalue of type int and thread local
  storage duration") and XSH section 2.3, Error Numbers ("For each
  thread of a process, the value of errno shall not be affected by
  function calls or assignments to errno by other threads"); glibc
  `csu/errno-loc.c` (`__errno_location`, `return &errno;`),
  `csu/errno.c` (`__thread int errno;`) and `include/errno.h` (`extern
  __thread int errno attribute_tls_model_ie;`, and `__errno_location`
  declared `__attribute_const__`), as read on glibc's master branch on
  2026-11-06 (a mirror).
- **C12, glibc's `pthread_self`.** The symbol `pthread_self` resolves
  to glibc's `pthread_t pthread_self(void)`, the signature of challoc's
  declaration of it in `sys/mod.rs` (`linux`'s `glibc` module), with
  `pthread_t` an `unsigned long`. A program or preloaded library that
  defines that symbol itself, interposing on glibc's, is outside this
  premise unless its definition does the same, and so is a thread
  challoc runs on that glibc did not create (a raw `clone`), for which
  it returns another thread's ID. Called by `sys::pthread_self` with no
  arguments, it returns the calling thread's ID, the address of its
  thread descriptor (`struct pthread`), and does not fail. It returns
  to its caller, writes no memory, reads none of challoc's (the thread
  pointer, and on x86_64 the descriptor's `header.self` word at
  `%fs:16`), and makes no call into challoc while it runs. What callers
  rely on of the ID is not part of the call, and not of this premise:
  that it tells the live threads of a process apart and stays the
  forking thread's in a `fork` child is what `lock::axiom_child_window`
  assumes, and that it is never 0, 1 or 2 is what the init gate's
  values assume (`api::GATE_IDLE`, `GATE_DONE` and `GATE_FORKING`;
  their code claims nothing). The harness runs the item with
  `pthread_self` replaced by a stand-in that records its call and
  returns any value. Source: POSIX `pthread_self` ("The pthread_self()
  function shall return the thread ID of the calling thread"; "No
  errors are defined."); glibc `nptl/pthread_self.c` (`__pthread_self`,
  `return (pthread_t) THREAD_SELF;`), `sysdeps/x86_64/nptl/tls.h` and
  `sysdeps/aarch64/nptl/tls.h` (`THREAD_SELF`: the `header.self` word
  through `%fs`, and the thread pointer less one `struct pthread`), as
  read on glibc's master branch on 2026-11-06 (a mirror).
- **C13, the stream a program passes to `malloc_info`.** The `stream` a
  program passes to `malloc_info` is null or the address of an open
  stream: a `FILE` that one of glibc's functions that open a stream
  (such as `fopen`, `fdopen`, `freopen`, `tmpfile`, `popen`,
  `fmemopen`, `open_memstream` and `fopencookie`) returned, or `stdin`,
  `stdout` or
  `stderr`, that the program has not closed and closes on no other
  thread while the call runs. A stream whose locking the program took
  over (`__fsetlocking` with `FSETLOCKING_BYCALLER`, under which glibc
  takes no lock for it) is used by no other thread while the call runs;
  otherwise the uses are a data race. The functions the program gave
  `fopencookie` for the stream keep the C allocation contract while
  glibc calls them during the call, as C3's callbacks do: each reads the
  buffer it is given only within its length and only until it returns,
  writes no byte through it, returns to its caller (it neither unwinds
  nor `longjmp`s out of the call and does not call `pthread_exit` during
  it), does not re-enable cancellation, and any call it makes into
  challoc passes only pointers it owns and frees only live blocks.
  `api::print::malloc_info` rejects a null stream with `EINVAL` before
  any write, and passes any other only to glibc's `fwrite` (C14), on the
  thread that called `malloc_info`, through its `Target::File` and
  `api::print::write_file`. A stream that is not open, or one closed
  while the call runs, is the program's use of memory it does not own.
  The harness runs the item on any stream address, which the stand-in
  for `fwrite` (C14) records and does not read through. Source: the
  Linux man-pages project's `malloc_info(3)` ("The string is printed on
  the file stream stream.") and `fopencookie(3)` (the program's `write`
  function, "ssize_t write(void *cookie, const char *buf, size_t
  size);"); glibc `malloc/malloc.c` (`__malloc_info`, which writes the
  document to `fp` with `fputs` and `fprintf`),
  `libio/__fsetlocking.c` (`FSETLOCKING_BYCALLER` sets `_IO_USER_LOCK`)
  and `libio/libio.h` (`_IO_USER_LOCK`, under which `_IO_flockfile`
  takes no lock), as read on glibc's master branch on 2026-11-07 (a
  mirror); the C allocation contract
  (`verus/RESULTS.md`, "Trust base") for the calls into challoc; ISO C11
  section 5.1.2.4 (a data race is undefined behaviour). That the
  program's stream functions do not `longjmp` out of the call is
  assumed, with no source.
- **C14, glibc's `fwrite`.** The symbol `fwrite` resolves to glibc's
  `size_t fwrite(const void *ptr, size_t size, size_t n, FILE *stream)`,
  the signature of challoc's declaration of it in `api/print.rs`. A
  program or preloaded library that defines that symbol itself,
  interposing on glibc's, is outside this premise unless its definition
  does the same, and so is a thread challoc runs on that glibc did not
  create (a raw `clone`), whose thread pointer names no descriptor of
  its own. Called by
  `api::print::write_file` with cancellation disabled (C9), with the
  address of `n` readable bytes on challoc's stack as `ptr` (the
  report's buffer), a `size` of 1 and a stream C13 describes, not null,
  it writes at most those `n` bytes to the stream (none to a
  wide-oriented stream) and returns a count no greater than `n`. It
  reads the `n` bytes at `ptr`
  and no other memory of challoc's, writes none (its statics, its TLS
  slot, its stack), and may set the calling thread's `errno`. It holds
  the stream's lock while it writes, unless the program took the
  stream's locking over (C13). Unlike C7 to C12, which leave every call
  into challoc outside them, it may call into challoc while it runs: glibc `malloc`s a stream's buffer on
  its first write, a memory stream grows its buffer with `malloc` and
  `free`, and a `fopencookie` stream's write is a call of the program's
  function (C13). In a process whose allocator is challoc each such
  `malloc` and `free` is a call into challoc, on the calling thread,
  that passes only pointers glibc owns and frees only live blocks. It
  is a cancellation point (its `write` system call); with cancellation
  disabled, as C9's call before it leaves it and as neither glibc nor
  the program's stream functions (C13) change it, it acts on no cancel.
  It returns to its caller. What becomes of bytes it leaves in the
  stream's buffer (a later flush) is not part of the call, and not of
  this premise. The harness runs the item with `fwrite` replaced by a
  stand-in that records its call, its arguments and its place among the
  calls of `pthread_setcancelstate`, checks that the `size * n` bytes at
  `ptr` are readable, and returns any value, and with
  `pthread_setcancelstate` replaced by a stand-in that records each
  call's place and `state`, checks that `oldstate` is writable, and
  returns any value for each call, storing any `int` in `*oldstate` when
  that value is 0. Source: POSIX `fwrite` ("The fwrite() function shall write, from the array
  pointed to by ptr, up to nitems elements whose size is specified by
  size, to the stream pointed to by stream."; "The fwrite() function
  shall return the number of elements successfully written, which shall
  be less than nitems only if a write error is encountered."; "if a
  write error occurs, the error indicator for the stream shall be set,
  and errno shall be set to indicate the error") and section 2.9.5.2,
  "Cancellation Points", of its System Interfaces volume, which names it
  among the functions at which "A cancellation point may also occur";
  glibc `libio/iofwrite.c` (`_IO_fwrite`: the stream lock, the
  orientation check, `_IO_sputn` and the count it returns),
  `libio/libioP.h` (`_IO_acquire_lock`), `libio/fileops.c` (`_IO_new_file_write`:
  `__write`, a cancellation point, unless the stream was opened not to
  be one), `libio/filedoalloc.c` (`_IO_file_doallocate`, the `malloc`
  of the buffer), `libio/strops.c` (a memory stream's `malloc` and
  `free` as it grows) and `libio/iofopncook.c` (`_IO_cookie_write`, the
  call of the program's write function), as read on glibc's master
  branch on 2026-11-07 (a mirror).
- **C15, glibc's `pthread_join`.** The symbol `pthread_join` resolves
  to glibc's `int pthread_join(pthread_t thread, void **retval)`, the
  signature of challoc's declaration of it in `purge/background.rs`
  (its `glibc` module), with `pthread_t` an `unsigned long`. A program
  or preloaded library that defines that symbol itself, interposing on
  glibc's, is outside this premise unless its definition does the same,
  and so is a thread challoc runs on that glibc did not create (a raw
  `clone`), whose thread pointer names no descriptor of its own.
  Called by `purge::background::pthread_join` with cancellation
  disabled (C9), with a null `retval` and the ID of a thread that
  `purge::background::spawn` created joinable, on a stack challoc
  mapped (`pthread_attr_setstack`), and not yet joined (the wrapper's
  `# Safety` clause, which its callers keep), it waits until that thread
  has exited, frees what glibc
  kept for it, and returns 0, or returns a nonzero error number. A call
  on any other ID (one already joined or detached, or the calling
  thread's own) is outside this premise. It stores nothing through `retval`, reads and writes none of challoc's
  memory (its statics, its TLS slot, its stack) but the exited thread's
  descriptor, which glibc placed in the stack challoc gave the thread,
  and returns to its caller. Unlike C7 to C12, which leave every call
  into challoc outside them, it calls into challoc while it runs: for a thread on a stack the program provided,
  as challoc's is, glibc frees the thread's TLS at the join
  (`_dl_deallocate_tls`): its dynamic thread vector, which glibc's
  `pthread_create` allocated with `calloc` (and a later access may have
  grown with `realloc`), and the TLS blocks of the dynamically loaded
  modules the thread used, which glibc allocated with `malloc` at the
  thread's first use of each; and, when the thread had any, its
  priority-protection data. In a
  process whose allocator is challoc each is a `free` into challoc, on
  the calling thread, of a live block challoc handed glibc, or of null
  (`_dl_deallocate_tls` frees every slot of the vector, the empty ones
  and those of modules in static TLS included). glibc makes the TLS
  frees with its stack cache lock held, which `pthread_create` and
  `pthread_join` take again, so the call returns only if no such nested
  entry starts or joins a thread; that challoc's `free` does not is a
  fact about challoc's code, not part of this premise. It is a
  cancellation point: its
  `__pthread_testcancel` and its futex wait act on a deferred cancel
  only while cancellation is enabled, which C9's call before it
  disables, so it acts on none. What the thread did before it exited
  (its key destructors, `heap::heap_destructor` among them, which run
  on that thread, theorem 10 of `verus/THEOREMS.md`) is not part of
  the call, and not of this premise. The harness runs the item with
  `pthread_join` replaced by a stand-in that records its call, the ID,
  the address of `retval` and its place among the calls of
  `pthread_setcancelstate`, and returns any value, and with
  `pthread_setcancelstate` replaced by a stand-in that records each
  call's place and `state`, checks that `oldstate` is writable, and
  returns any value for each call, storing any `int` in `*oldstate` when
  that value is 0. Source: POSIX `pthread_join` ("The pthread_join() function shall suspend
  execution of the calling thread until the target thread terminates,
  unless the target thread has already terminated."; "If successful,
  the pthread_join() function shall return zero; otherwise, an error
  number shall be returned to indicate the error."; "The behavior is
  undefined if the value specified by the thread argument to
  pthread_join() does not refer to a joinable thread.") and section
  2.9.5.2, "Cancellation Points", which names it among the functions at
  which "Cancellation points shall occur"; glibc `nptl/pthread_join.c`
  (`___pthread_join`, a `__pthread_clockjoin_ex` with cancellation),
  `nptl/pthread_join_common.c` (`__pthread_clockjoin_ex`:
  `__pthread_testcancel`, the `EDEADLK` and `EINVAL` returns, the wait
  on `joinstate`, the store through a non-null `thread_return`, and
  `__nptl_free_tcb`), `nptl/nptl_free_tcb.c` (the `free` of `tpp`, and
  `__nptl_deallocate_stack`), `nptl/nptl-stack.c`
  (`__nptl_deallocate_stack`: under the stack cache lock,
  `_dl_deallocate_tls` for a stack the program provided),
  `nptl/allocatestack.c` (`ALLOCATE_GUARD_USER`, the mode of a stack
  from `pthread_attr_setstack`), `elf/dl-tls.c` (`_dl_deallocate_tls`'s
  `free`s, `allocate_dtv`'s `calloc`, `_dl_resize_dtv`'s `realloc` and
  `allocate_dtv_entry`'s `malloc`), `nptl/futex-internal.c` (the wait,
  an `INTERNAL_SYSCALL_CANCEL`), `nptl/pthread_testcancel.c` and
  `nptl/cancellation.c` (`__internal_syscall_cancel`; neither acts on a
  cancel while cancellation is disabled), as read on glibc's master
  branch on 2026-11-07 (a mirror).

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
so a seam that reads through an address L1 or L2 describes, or reads
or writes through the address C11's `__errno_location` returns, rests
on this argument too, which no tool checks. challoc allocates none of
that memory: the three globals are glibc's objects in ld.so, which
challoc reaches only through `.weak` references, and the rseq area and
`errno` lie in glibc's TLS block of the thread, in bytes no Rust
allocation of challoc's (a stack frame, a static, a `Box`) contains.
`core::ptr`'s documentation ("Exposed Provenance") says that memory
outside the control of the Rust abstract machine "is always considered
to be exposed, so long as this memory is disjoint from memory that will
be used by the abstract machine such as the stack, heap, and statics".
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
- `vspec::errno::set_errno` and `vspec::errno::get_errno` write and read
  through the pointer `__errno_location` returns (C11), a foreign
  function's return value into memory outside the abstract machine:
  one whose provenance is that memory's, exposed, on the same step as
  for an `asm!` output above. The pointer may write and read the `int`
  C11 describes.

A thread stack the program provides (`pthread_attr_setstack`) may lie in
memory challoc handed out, and glibc then places the thread's TLS
block, rseq area and `errno` included, there. Those bytes are in
challoc's own mappings, which challoc reaches only through addresses,
never through a Rust allocation of its own, so they are exposed on the
same footing. A
program written in Rust whose own Rust allocation (a `Vec` from its
global allocator) is that stack is outside this argument: the bytes are
then that program's abstract-machine memory, not disjoint from it.
