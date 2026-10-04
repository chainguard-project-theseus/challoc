From radium Require Import lang notation.
From refinedrust Require Import typing shims.
From challoc.challoc.generated Require Import generated_code_challoc generated_specs_challoc generated_template_heap_others_node_link.

Set Default Proof Using "Type".

Section proof.
Context `{RRGS : !refinedrustGS Σ}.

Lemma heap_others_node_link_proof (π : thread_id) :
  heap_others_node_link_lemma π.
Proof.
  heap_others_node_link_prelude.

  (* The field places of [*node]: the precondition owns the node's head
     ([ArenaCachesHead_sls], [challoc.rr.heads]), not the whole node, so
     each projection is typed through the head ([challoc.rr.struct_prefix]). *)
  repeat first [
    typed_place_head heap_others_ArenaCaches_sls ArenaCachesHead_sls |
    liRStep].

  all: print_remaining_goal.
  Unshelve. all: sidecond_solver.
  Unshelve. all: sidecond_hammer.
  Unshelve. all: print_remaining_sidecond.
Qed.
End proof.
