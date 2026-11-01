// Isla entry for the x86 model (challoc verus/ISLA.md, x86_64): the
// entry's own registers, declared before the model (plain Sail types, as
// the model's prelude is not loaded yet). isla_opcode is the instruction
// (isla_rb.sail), isla_fetch_base the rip it was fetched at, isla_fault
// the abnormal-path marker (isla_entry.sail). verus/isla/check.rs never
// links them between instructions.

register isla_opcode : bitvector(128)
register isla_fetch_base : bitvector(48)
register isla_fault : bool
