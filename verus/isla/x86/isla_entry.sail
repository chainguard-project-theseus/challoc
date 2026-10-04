// Isla entry for the x86 model (challoc verus/ISLA.md, x86_64): one
// fetch-decode-execute step of the instruction in `opcode` (see
// isla_rb.sail) at the current rip. The cycle marker after the setup is
// where `isla-footprint -s` starts the trace. A Sail exception (the
// model's x86_fault, x86_model_error, the app-view Syscall()) ends the
// step: it is caught and recorded as a write of isla_fault, which the
// checker treats as an abnormal path.

val isla_cycle_count = impure {_: "cycle_count"} : unit -> unit

val isla_footprint : bits(128) -> bool

function isla_footprint(opcode) = {
  isla_opcode = opcode;
  isla_fetch_base = truncate(rip, 48);
  isla_cycle_count();
  try {
    x86_fetch_decode_execute()
  } catch {
    _ => isla_fault = true
  };
  true
}
