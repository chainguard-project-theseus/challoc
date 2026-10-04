From radium Require Import lang notation.
From refinedrust Require Import typing shims.
From challoc.challoc.generated Require Import generated_code_challoc generated_specs_challoc generated_template_heap_mark_bound.

Set Default Proof Using "Type".

Section proof.
Context `{RRGS : !refinedrustGS Σ}.

Lemma heap_mark_bound_proof (π : thread_id) :
  heap_mark_bound_lemma π.
Proof.
  heap_mark_bound_prelude.

  (* The field place [bound] of [*h]: the precondition owns the heap's
     head ([HeapHead_sls], [challoc.rr.heads]), not the whole heap, so the
     projection is typed through the head ([challoc.rr.struct_prefix]).
     The head's [bound] is a [bool_t], so the store of [true] is
     RefinedRust's own rule. *)
  repeat first [
    typed_place_head heap_Heap_sls (HeapHead_sls heap_ThreadStats_st) |
    liRStep].

  all: print_remaining_goal.
  Unshelve. all: sidecond_solver.
  Unshelve. all: sidecond_hammer.
  Unshelve. all: print_remaining_sidecond.
Qed.
End proof.
