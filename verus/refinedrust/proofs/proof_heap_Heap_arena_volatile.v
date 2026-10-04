From radium Require Import lang notation.
From refinedrust Require Import typing shims.
From challoc.challoc.generated Require Import generated_code_challoc generated_specs_challoc generated_template_heap_Heap_arena_volatile.

Set Default Proof Using "Type".

Section proof.
Context `{RRGS : !refinedrustGS Σ}.

Lemma heap_Heap_arena_volatile_proof (π : thread_id) :
  heap_Heap_arena_volatile_lemma π.
Proof.
  heap_Heap_arena_volatile_prelude.

  (* [&raw const self.arena]: RefinedRust has no rule for a raw borrow of a
     shared place; it is typed as a shared borrow at the lifetime of [self]
     ([challoc.rr.raw_shr]). The field place: the precondition shares the
     heap's first fields ([HeapArenaHead_sls], [challoc.rr.heads]), not the
     whole heap, so the projection is typed through them
     ([challoc.rr.struct_prefix]). *)
  repeat first [
    typed_raw_shr ulft_1 |
    typed_place_head heap_Heap_sls HeapArenaHead_sls |
    liRStep].

  all: print_remaining_goal.
  Unshelve. all: sidecond_solver.
  Unshelve. all: sidecond_hammer.
  Unshelve. all: print_remaining_sidecond.
Qed.
End proof.
