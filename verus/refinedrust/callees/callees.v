(** The RefinedRust stdlib functions the discharged seams call, proved
    against the specifications the seams' proofs assume of them: `run.sh`
    checks what these proofs rest on, as for the seams' own. *)
From rrstd.ptr.ptr.proofs Require Import proof_read proof_read_volatile proof_write.
(* [Heap::arena_volatile]: [ptr::read_volatile], which calls [ptr::read]. *)
Print Assumptions read_proof.
Print Assumptions read_volatile_proof.
(* [ptr::write], kept for the remaining seams' callees. *)
Print Assumptions write_proof.
