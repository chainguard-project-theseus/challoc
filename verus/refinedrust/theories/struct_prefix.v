(** Typing a field of a struct of which only a prefix is owned.
    RefinedRust types the place of a field [f] of [*p] through ownership
    of the whole struct [S] at [p]. A [repr(C)] struct [H] made of the
    first fields of [S] has the same offset for [f], so the field
    projection on [S] is the same Radium expression as on [H], and the
    place can be typed with ownership of an [H] at [p] instead.
    [typed_place_member_prefix] is that step; [struct_prefix_offset]
    proves its side condition from the two structs' field lists. Proved;
    nothing here is assumed. *)
From refinedrust Require Import typing.

Section rule.
  Context `{!typeGS Σ}.

  Lemma typed_place_member_prefix E L f l {rto} (lt : ltype rto) r bmin0 b
      (sls_full sls_head : struct_layout_spec) m P (T : place_cont_t rto bmin0) :
    offset_of (use_struct_layout_alg' sls_full).(sl_members) m =
      offset_of (use_struct_layout_alg' sls_head).(sl_members) m →
    typed_place E L f l lt r bmin0 b (GetMemberPCtx sls_head m :: P) T
    ⊢ typed_place E L f l lt r bmin0 b (GetMemberPCtx sls_full m :: P) T.
  Proof.
    intros Hoff. rewrite /typed_place /place_to_wp /= /GetMember /= Hoff. done.
  Qed.
End rule.

Section offsets.
  Context `{!LayoutAlg}.

  Lemma offset_of_cons_eq (x : option string * layout) l m :
    x.1 = Some m → offset_of (x :: l) m = Some 0%nat.
  Proof.
    intros Hx. rewrite /offset_of /index_of /=.
    rewrite decide_True; done.
  Qed.

  Lemma offset_of_cons_ne (x : option string * layout) l m :
    x.1 ≠ Some m →
    offset_of (x :: l) m = (λ o, ly_size x.2 + o)%nat <$> offset_of l m.
  Proof.
    intros Hx. rewrite /offset_of /index_of /=.
    rewrite decide_False; last done.
    destruct (list_find _ l) as [[i y]|]; done.
  Qed.

  (** The [repr(C)] layout of [h ++ r] places [h]'s fields where the
      layout of [h] alone does. *)
  Lemma C_raw_go_offset_app off a h r m :
    m ∈ h.*1 →
    offset_of (C_struct_layout_alg_raw_go off a (h ++ r)) m =
      offset_of (C_struct_layout_alg_raw_go off a h) m.
  Proof.
    induction h as [|[n ly] h IH] in off, a |- *; intros Hm.
    { set_solver. }
    simpl.
    destruct (decide (n = m)) as [->|Hne].
    - case_decide.
      + rewrite !offset_of_cons_eq //.
      + rewrite !(offset_of_cons_ne (None, _)) //.
        rewrite !offset_of_cons_eq //.
    - assert (m ∈ h.*1) as Hm'.
      { simpl in Hm. set_solver. }
      assert ((Some n, ly).1 ≠ Some m) as Hn by (simpl; congruence).
      case_decide.
      + rewrite !(offset_of_cons_ne (Some n, ly)) // IH //.
      + rewrite !(offset_of_cons_ne (None, _)) // !(offset_of_cons_ne (Some n, ly)) // IH //.
  Qed.

  Lemma list_map_option_app {X Y} (g : X → option Y) a b l :
    list_map_option g (a ++ b) = Some l →
    ∃ la lb, l = la ++ lb ∧ list_map_option g a = Some la ∧ list_map_option g b = Some lb.
  Proof.
    induction a as [|x a IH] in l |- *; simpl.
    { intros Hb. by exists [], l. }
    intros Hl. apply bind_Some in Hl as (l' & Hl' & Hx).
    apply bind_Some in Hx as (y & Hy & [= <-]).
    destruct (IH _ Hl') as (la & lb & -> & Ha & Hb).
    exists (y :: la), lb. split; first done. split; last done.
    rewrite /list_map_option /= -/(list_map_option g a) Ha /= Hy. done.
  Qed.

  Lemma list_map_option_field_names (fields : list (string * syn_type)) fl :
    list_map_option (λ '(field_name, field_spec),
      ly ← use_layout_alg field_spec; Some (field_name, ly)) fields = Some fl →
    fl.*1 = fields.*1.
  Proof.
    induction fields as [|[n st] fields IH] in fl |- *; simpl.
    { by intros [= <-]. }
    intros Hl. apply bind_Some in Hl as (l' & Hl' & Hx).
    apply bind_Some in Hx as (y & Hy & [= <-]).
    apply bind_Some in Hy as (ly & _ & [= <-]).
    rewrite !fmap_cons (IH _ Hl') //.
  Qed.

  Lemma use_struct_layout_alg_C_members sls sl :
    sls.(sls_repr) = StructReprC →
    use_struct_layout_alg sls = Some sl →
    ∃ fl, list_map_option (λ '(field_name, field_spec),
        ly ← use_layout_alg field_spec; Some (field_name, ly)) sls.(sls_fields) = Some fl ∧
      sl.(sl_members) = C_struct_layout_alg_raw fl.
  Proof.
    intros Hrepr Hsl. rewrite /use_struct_layout_alg in Hsl.
    apply bind_Some in Hsl as (fl & Hfl & Halg).
    exists fl. split; first done.
    rewrite Hrepr strruct_layout_alg_repr_C in Halg.
    rewrite /C_struct_layout_alg in Halg.
    repeat case_decide; simplify_eq. done.
  Qed.

  (** The side condition of [typed_place_member_prefix]. *)
  Lemma struct_prefix_offset (sls_full sls_head : struct_layout_spec) rest m :
    sls_full.(sls_repr) = StructReprC →
    sls_head.(sls_repr) = StructReprC →
    sls_full.(sls_fields) = sls_head.(sls_fields) ++ rest →
    m ∈ sls_head.(sls_fields).*1 →
    struct_layout_spec_is_layoutable sls_full →
    struct_layout_spec_is_layoutable sls_head →
    offset_of (use_struct_layout_alg' sls_full).(sl_members) m =
      offset_of (use_struct_layout_alg' sls_head).(sl_members) m.
  Proof.
    intros Hrf Hrh Hfields Hm [slf Hf] [slh Hh].
    rewrite /use_struct_layout_alg' Hf Hh /=.
    destruct (use_struct_layout_alg_C_members _ _ Hrf Hf) as (flf & Hflf & ->).
    destruct (use_struct_layout_alg_C_members _ _ Hrh Hh) as (flh & Hflh & ->).
    rewrite Hfields in Hflf.
    apply list_map_option_app in Hflf as (la & lb & -> & Ha & _).
    rewrite Hflh in Ha. injection Ha as <-.
    rewrite /C_struct_layout_alg_raw C_raw_go_offset_app; first done.
    by rewrite (list_map_option_field_names _ _ Hflh).
  Qed.

  Lemma struct_layout_spec_is_layoutable_of_syn_type (sls : struct_layout_spec) :
    syn_type_is_layoutable sls → struct_layout_spec_is_layoutable sls.
  Proof.
    intros [ly Hly]. apply use_layout_alg_struct_Some_inv in Hly as (sl & Hsl & _).
    by exists sl.
  Qed.
End offsets.

(** In a proof: when the next place step is field [m] of [sls_full], type
    it through [sls_head], the struct the precondition owns. The side
    condition takes both structs' layouts from the proof context (the
    function's [syn_type_is_layoutable] premise, and the ownership of the
    head). *)
Ltac typed_place_head sls_full sls_head :=
  lazymatch goal with
  | |- context [typed_place _ _ _ _ _ _ _ _ (GetMemberPCtx sls_full ?m :: _) _] =>
      let Hoff := fresh "Hoff" in
      assert (Hoff : offset_of (use_struct_layout_alg' sls_full).(sl_members) m =
        offset_of (use_struct_layout_alg' sls_head).(sl_members) m);
      [ unfold CACHED in *;
        eapply struct_prefix_offset;
        [ done | done | simpl; reflexivity | set_solver
        | eexists; eassumption | eexists; eassumption ]
      | notypeclasses refine (tac_fast_apply
          (typed_place_member_prefix _ _ _ _ _ _ _ _ sls_full sls_head m _ _ Hoff) _) ]
  end.
