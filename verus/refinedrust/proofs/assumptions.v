(** What each discharged seam's proof rests on: `run.sh` fails unless every
    `Print Assumptions` below names only the axioms RefinedRust itself
    uses (`statements.v` checks what each proves). *)
From challoc.challoc.proofs Require Import
  proof_heap_Heap_arena_volatile proof_heap_others_node_link proof_heap_mark_bound.
Print Assumptions heap_Heap_arena_volatile_proof.
Print Assumptions heap_others_node_link_proof.
Print Assumptions heap_mark_bound_proof.
