(** A typing rule RefinedRust lacks: [&raw const place] ([&raw{pk, Shr} e])
    below a shared reference. The expression is the same as a shared borrow
    ([Raw] and [Ref] are both [SkipE (SkipE e)]), so the rule borrows the
    place shared at a lifetime [κ] chosen by the proof and hands the client
    the location as an [alias_ptr_kind_t] value together with the place's
    (persistent) shared ownership. Proved from RefinedRust's own rules;
    nothing here is assumed. *)
From refinedrust Require Import typing.

Section rule.
  Context `{!typeGS Σ}.

  Lemma type_raw_shr pk E L f (T : typed_val_expr_cont_t) e κ :
    typed_borrow_shr E L f e κ None (λ L' v rt ty r,
      ∀ l, ⌜v = l2v l⌝ -∗ l ◁ₗ[f.1, Shared κ] r @ (◁ ty) -∗
        T L' v MetaNone locRT (alias_ptr_kind_t pk) l)
    ⊢ typed_val_expr E L f (&raw{pk, Shr} e)%E T.
  Proof.
    iIntros "HT". rewrite /typed_val_expr /typed_borrow_shr.
    iIntros (Φ) "#(LFT & LLCTX) #HE HL Hf HΦ".
    wpe_bind.
    iApply ("HT" $! _ ⊤ with "[//] [//] [//] [//] [$LFT $LLCTX] HE HL Hf").
    iIntros (l) "HT".
    unfold Raw. wpe_bind. iApply wpe_fupd.
    iApply (wpe_logical_step with "HT"); [solve_ndisj.. | ].
    iApply wp_skip. iApply physical_step_intro; iNext. iIntros "HT !>".
    iApply wpe_fupd.
    iApply (wpe_logical_step with "HT"); [solve_ndisj.. | ].
    iApply wp_skip.
    iApply physical_step_intro_lc. iIntros "(Hcred & Hcred1) !> !> HT".
    iMod ("HT" with "Hcred1") as (L' rt ty r r' ly) "(#Hrfn & #Hshr & %Halg & %Hly & #Hlb & #Hsc & HL & Hf & HT)".
    iPoseProof (loc_in_bounds_in_range_usize with "Hlb") as "%Husize".
    iPoseProof (loc_in_bounds_nonnull with "Hlb") as "%Hnonnull".
    iModIntro. iApply ("HΦ" $! L' (l2v l) locRT (alias_ptr_kind_t pk) l MetaNone with "HL Hf [] [HT]").
    - rewrite /ty_own_val/=. iR. iApply l2v_alias_ptr_kind_own_val.
      iPureIntro. destruct pk; simpl; done.
    - iApply ("HT" with "[//]").
      rewrite ltype_own_ofty_unfold /lty_of_ty_own.
      iExists ly. iR. iR. iFrame "Hsc Hlb". iExists r'. iFrame "Hrfn".
      iModIntro. done.
  Qed.
End rule.

(** In a proof: type the next [&raw const] as a shared borrow at [κ]. *)
Ltac typed_raw_shr κ :=
  lazymatch goal with
  | |- iris.proofmode.environments.envs_entails _ (typed_val_expr _ _ _ (Raw _ Shr _) _) =>
      notypeclasses refine (tac_fast_apply (type_raw_shr _ _ _ _ _ _ κ) _)
  end.
