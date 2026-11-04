# The premises Kani statuses name

A Kani harness runs a trusted item's own body on the inputs and statics
the harness sets up. What the C program stores in those statics is
outside its reach. A seam whose harness covers its whole contract except
for such facts is `discharged on premises (Kani: <harness path>;
premises: <what>)` (verus/SEAMS.md, "Status"), and `<what>` names them
by the labels below.

## Premises

Each premise is a fact about what the C program that calls challoc
stores in, or passes through, challoc's exported C symbols. The harness
assumes it in the values it stores before it runs the item, and in
running the item on one thread; its comment states those values.
`verus/count` reads the labels of this section's bullets and fails on a
Kani status that names any other.

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
