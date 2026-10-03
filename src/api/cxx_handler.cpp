// The one piece of challoc written in C++ (design.md section 13.1): the
// `nothrow_t` forms of `operator new` call the program's `new_handler`
// through this function, which returns 0 when the handler throws. The
// standard has a nothrow form return null wherever the throwing form would
// exit through an exception, and Rust cannot catch a C++ exception, so the
// catch lives here. `src/api/cxx.rs` calls it; build.rs compiles it.
//
// The C++ runtime entry points the catch needs are weak references, as
// `std::get_new_handler` is in cxx.rs: a C process that never maps
// libstdc++ still loads libchalloc.so under BIND_NOW, and it cannot have
// installed a handler, so this function is never reached there. They are
// made weak with `.weak` directives because clang ignores a weak attribute
// on its builtin declarations of `__cxa_begin_catch` and `__cxa_end_catch`.
// The function is not `noexcept`: that would make clang guard the catch
// with a call to `std::terminate`, a strong reference into libstdc++.
// It assumes one C++ runtime (libstdc++ or libc++abi) provides all three
// symbols together with `std::get_new_handler`, which is how they ship.
// `catch (...)` also takes glibc's forced unwind, so a thread cancelled
// inside a handler aborts; libstdc++'s own nothrow `operator new` is the
// same `try { ... } catch (...) { return nullptr; }`. The file
// includes no headers, so it needs a C++ compiler but no C++ library.

asm(".weak __gxx_personality_v0");
asm(".weak __cxa_begin_catch");
asm(".weak __cxa_end_catch");

extern "C" __attribute__((visibility("hidden"))) int challoc_call_new_handler(void (*handler)())
{
    try {
        handler();
        return 1;
    } catch (...) {
        return 0;
    }
}
