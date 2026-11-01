// Isla entry for the x86 model (challoc verus/ISLA.md, x86_64): rb with
// instruction fetches served from the opcode isla_footprint was given.
//
// isla-footprint passes the instruction as one 128-bit vector, its bytes
// in memory order from the least significant end, padded to 16 bytes (the
// decoder reads at most 15, and only those the instruction has). The model
// fetches instruction bytes as ordinary byte reads through rb(.., ":X");
// this rb serves them from the opcode, so the trace has no memory or
// page-table events for the fetch itself (as with Isla's Arm models, which
// take the opcode as an argument). A fetch outside the 16 bytes is a
// model error (an abnormal path for the checker, not an assertion Isla
// would assume away). Everything else is the model's rb.

val rb : forall 'addr_width 'n, 'addr_width > 0 & 'n > 0. (int('n), bits('addr_width), string) -> bits(8 * 'n)

function rb (n, addr, access_kind) = {
  result : bits(8 * 'n) = sail_zeros(8 * n);
  foreach (offset from 0 to (n - 1)) {
    if not_bool(canonical_address_p(signed(addr) + offset)) then x86_model_error("rb: non-canonical address");
    let lin_addr : bits(48) = add_bits_int(sail_mask_signed(48, addr), offset);
    if access_kind == ":X" then {
      let k : bits(48) = sub_bits(lin_addr, isla_fetch_base);
      if unsigned(k) >= 16 then x86_model_error("isla: fetch outside the opcode");
      let shifted : bits(128) = isla_opcode >> (8 * unsigned(k[3 .. 0]));
      result[((8 * offset) + 7) .. (8 * offset)] = shifted[7 .. 0]
    } else {
      let phys_addr : bits(52) =
        if app_view then sail_mask_signed(52, lin_addr) else ia32e_la_to_pa(lin_addr, access_kind)
      in
      result[((8 * offset) + 7) .. (8 * offset)] = memi(phys_addr)
    }
  };
  return result
}
