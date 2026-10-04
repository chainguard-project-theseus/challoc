(** Each discharged seam's proof has exactly its template's statement:
    a proof with an extra hypothesis (say [False]) would not fit these
    definitions, and `run.sh` builds this file. *)
From refinedrust Require Import typing.
From challoc.challoc.generated Require Import
  generated_template_heap_Heap_arena_volatile generated_template_heap_others_node_link
  generated_template_heap_mark_bound.
From challoc.challoc.proofs Require Import
  proof_heap_Heap_arena_volatile proof_heap_others_node_link proof_heap_mark_bound.

Section statements.
  Context `{RRGS : !refinedrustGS Σ}.
  Definition heap_Heap_arena_volatile_statement : ∀ π, heap_Heap_arena_volatile_lemma π :=
    heap_Heap_arena_volatile_proof.
  Definition heap_others_node_link_statement : ∀ π, heap_others_node_link_lemma π :=
    heap_others_node_link_proof.
  Definition heap_mark_bound_statement : ∀ π, heap_mark_bound_lemma π :=
    heap_mark_bound_proof.
End statements.
