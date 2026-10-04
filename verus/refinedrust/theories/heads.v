(** Struct prefixes ([repr(C)] structs of the first fields of a challoc
    struct), for specifications that own only a struct's first bytes
    ([struct_prefix.v]). *)
From refinedrust Require Import typing.

(** The first line of an `others` node ([heap::others::ArenaCaches]):
    [arena], [pad], [next]; 16 bytes, 8-aligned. *)
Definition ArenaCachesHead_sls : struct_layout_spec :=
  mk_sls "ArenaCachesHead" [
    ("arena", IntSynType U16);
    ("pad", IntSynType U8);
    ("next", PtrSynType)] StructReprC.

(** The fields of a [heap::Heap] before its [rng] ([init_fields] writes
    them); [thread_stats] is [heap::ThreadStats]'s syntactic type, which
    the generated code defines. Its fields end at byte 74 and it is 80
    bytes, 8-aligned, under rustc's layout (offsets in
    [mark_bound]'s record). *)
Definition HeapHead_sls (thread_stats : syn_type) : struct_layout_spec :=
  mk_sls "HeapHead" [
    ("shared", PtrSynType);
    ("tid", IntSynType U32);
    ("arena", IntSynType U16);
    ("slow_ticks", IntSynType U32);
    ("last_gc", IntSynType U64);
    ("uncached", thread_stats);
    ("others", PtrSynType);
    ("snapshot", thread_stats);
    ("tcache_enabled", BoolSynType);
    ("bound", BoolSynType)] StructReprC.

(** The node head as owned, uninitialised memory (any bytes, of the
    head's layout), and its refinement, for [node_link]'s specification. *)
Notation ArenaCachesHead_uninit :=
  (struct_t ArenaCachesHead_sls
    +[uninit (IntSynType U16); uninit (IntSynType U8); uninit PtrSynType])
  (only parsing).
Notation ArenaCachesHead_rfn := (-[#(); #(); #()]) (only parsing).

(** The heap head of a fresh heap, as [mark_bound]'s specification owns
    it: [bound] holds the flag [b] (what [FreshHeap::bound] records), and
    every other field the bytes it holds ([value_t false]: any bytes of
    the field's layout, refined by those bytes, so the bytes are named
    and can be stated unchanged). *)
Notation HeapHead_fresh thread_stats :=
  (struct_t (HeapHead_sls thread_stats)
    +[value_t false PtrSynType; value_t false (IntSynType U32);
      value_t false (IntSynType U16); value_t false (IntSynType U32);
      value_t false (IntSynType U64); value_t false thread_stats;
      value_t false PtrSynType; value_t false thread_stats;
      value_t false BoolSynType; bool_t])
  (only parsing).
Notation HeapHead_fresh_rfn sh tid ar st lg un ot sn te b :=
  (-[#sh; #tid; #ar; #st; #lg; #un; #ot; #sn; #te; #b]) (only parsing).

(** The first fields of a [heap::Heap], through [arena]: 14 bytes, 8-aligned
    (offsets 0, 8, 12 under rustc's layout and the C layout alike), as
    [Heap::arena_volatile] reads it through [&self]. *)
Definition HeapArenaHead_sls : struct_layout_spec :=
  mk_sls "HeapArenaHead" [
    ("shared", PtrSynType);
    ("tid", IntSynType U32);
    ("arena", IntSynType U16)] StructReprC.
Notation HeapArenaHead_ty :=
  (struct_t HeapArenaHead_sls +[alias_ptr_t; int U32; int U16]) (only parsing).
