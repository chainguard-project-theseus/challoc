//! A test-only checker of what [`tcaches::heap_at`] trusts, and the tests
//! that run it on every heap `heap_at` hands out.
//!
//! `heap_at` is `external_body`: it ensures `r.wf()` and `r.home_at(p)`
//! for the heap at `p` with no proof. [`check`] tests every part of those
//! two specs that a heap's bytes can show, for a `&Heap` and the pointer
//! `p` its caller had: it is a transcription of `Heap::wf`, `Heap::home_at`
//! and the specs they name, conjunct by conjunct, in [`SPECS`]. Each
//! conjunct there is [`Cover::Checked`] (tested as written),
//! [`Cover::ByWalk`] (true of the list the checker walks, which it builds
//! from the `next` chain the conjunct describes), [`Cover::Implied`]
//! (ghost, with a consequence in memory the checker tests) or
//! [`Cover::Ghost`] (not tested). One more check, [`Broken::KeyRange`],
//! is not a `Heap::wf` conjunct.
//! `checker_transcribes_heap_wf_conjunct_by_conjunct` pins [`SPECS`]
//! against the source, so a conjunct added to, removed from or reworded in
//! any of those specs fails it until the table follows; [`check`] is tied
//! to the table only through the [`Broken`] variants each row names,
//! which the test requires to exist, so a review of a table change is
//! what keeps the checker in step. The part of [`check`] that needs no
//! sets is `heap_facts::set_free` and `heap_facts::homed_ok`, which the
//! debug TLS check (`tlscheck::deep`) runs as well (`heap_facts::no_sets`),
//! so the table covers both, and the mutants below run both
//! (`check_both`) but for the set-based conjuncts past a node met twice.
//!
//! The `shared` conjunct names the process's `Shared`
//! (`Shared::storage()`); the checker takes the `Shared` the heap was
//! created on, which is the process's for the C library and a
//! [`TestShared`](crate::testutil::TestShared) in the unit tests.
//!
//! The tests reach every caller of `heap_at` and run the checker on the
//! heap that caller is handed, with the pointer it hands `heap_at`:
//! `Tcaches::get` (the table entry), `heap_destructor` (the thread key's
//! value) and `Heap::destroy` (the pointer `Heap::create` returned, the
//! table entry `Tcaches::destroy` swapped out, the key's value). The
//! `MALLOCX_TCACHE` paths of the C API are `api::heap_at_tests`.
//!
//! `checker_fails_each_corrupted_fact` has a mutant for each [`Broken`]
//! variant but these:
//! [`Broken::NodeWraps`], since a node is first checked to lie in the
//! metadata region, which ends below the top of the address space;
//! [`Broken::PadBelowZero`], since that region starts at a nonzero page,
//! above `64 * 15`; and, on a host without pointer authentication,
//! [`Broken::EntryNotStored`], since `stored` is then the identity.

use super::heap_facts::{self, Broken, Hdr, Seen, plain};
use super::*;
use crate::layout::{CLASSES, class_of};
use crate::testutil::{TestShared, xorshift};
use core::ffi::c_void;
use std::collections::HashSet;
use std::string::String;
use std::vec::Vec;
use vstd::prelude::Tracked;

/// How [`check`] covers one conjunct of [`SPECS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cover {
    /// Tested as written.
    Checked,
    /// True of the list the checker walks: a `let`, or a fact of the
    /// `next` chain the walk follows.
    ByWalk,
    /// Ghost state, with a consequence in memory that is tested.
    Implied,
    /// Not tested: ghost state, which no bytes of the heap or its caches
    /// show, or (`regions_ok`) a fact of the process's layout, not of the
    /// heap.
    Ghost,
}

/// A spec [`check`] transcribes: its file under `src/heap/`, its
/// signature line (trimmed), and its top-level conjuncts in source order,
/// each with how the checker covers it and the [`Broken`] variants (or the
/// reason it cannot be tested).
struct Spec {
    file: &'static str,
    sig: &'static str,
    items: &'static [(&'static str, Cover, &'static str)],
}

use Cover::{ByWalk, Checked, Ghost, Implied};

/// `Heap::wf`, `Heap::home_at`, the specs they name down to ghost state,
/// and the definitions [`check`] computes with (`home_hdr`, `node_hdr`,
/// `plain`, `stored`, `in_prefix`, `block_size`, `node_block`), conjunct
/// by conjunct (whitespace collapsed; a body with no top-level `&&&` is
/// one item). Not listed: `CachePerms::holds`, which is `slot_entry`; the
/// specs about ghost state only (`CachePerms::tok_at`, `homed_on`,
/// `in_alloc`); and the spec `Regions::is_slot` and `Regions::in_meta`,
/// whose exec counterparts `Regions::classify` and `Regions::in_meta` the
/// checker calls.
const SPECS: &[Spec] = &[
    Spec {
        file: "mod.rs",
        sig: "pub closed spec fn wf(&self) -> bool {",
        items: &[
            ("&&& self.perms@.wf()", Implied, "CachePerms::wf"),
            ("&&& self.perms@.tracks()", Implied, "CachePerms::tracks"),
            ("&&& self.perms@.zero_ok()", Implied, "CachePerms::zero_ok"),
            ("&&& self.perms@.tokened()", Implied, "CachePerms::tokened"),
            ("&&& self.perms@.claimed()", Implied, "CachePerms::claimed"),
            ("&&& regions_ok()", Ghost, "regions_ok"),
            (
                "&&& crate::api::ready()",
                Ghost,
                "uninterpreted: the `Shared` is published",
            ),
            (
                "&&& self.shared == Shared::storage() as *const Shared",
                Checked,
                "Shared (against the heap's own `Shared`)",
            ),
            ("&&& self.list_ok()", Implied, "Heap::list_ok"),
            ("&&& self.home_ok()", Implied, "Heap::home_ok"),
            ("&&& self.homed_ok()", Implied, "Heap::homed_ok"),
        ],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn wf(&self) -> bool {",
        items: &[
            (
                "&&& forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) ==> cell_ok(p, self.hdrs[p], self.slots)",
                Implied,
                "cell_ok of every header `home_ok` and `owns_node` name owned; \
                 the owned set itself is ghost",
            ),
            (
                "&&& forall|q: *mut *mut u8| #[trigger] self.slots.contains_key(q) ==> self.slots[q].ptr() == q",
                Ghost,
                "stack-entry permissions",
            ),
        ],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn cell_ok(p: *mut CacheHeader, hp: PointsTo<CacheHeader>, slots: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {",
        items: &[
            ("&&& hp.ptr() == p", Ghost, "the header's permission"),
            ("&&& hp.is_init()", Ghost, "the header's permission"),
            ("&&& hp.value().inv()", Checked, "CacheHeader::inv"),
            ("&&& stack_ok(hp.value(), slots)", Ghost, "stack_ok"),
            ("&&& cell_in_meta(p, hp.value())", Checked, "cell_in_meta"),
        ],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn cell_in_meta(p: *mut CacheHeader, h: CacheHeader) -> bool {",
        items: &[
            (
                "&&& process_regions().in_meta(p@.addr as int, 64)",
                Checked,
                "HeaderOutsideMeta",
            ),
            (
                "&&& h.cap != 0 ==> process_regions().in_meta(h.slots@.addr as int, 8 * h.cap)",
                Checked,
                "StackOutsideMeta",
            ),
        ],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn stack_ok(h: CacheHeader, slots: Map<*mut *mut u8, PointsTo<*mut u8>>) -> bool {",
        items: &[
            (
                "&&& forall|i: int| 0 <= i < h.cap ==> { let q = #[trigger] slot_ptr(h.slots, i); &&& slots.contains_key(q) &&& slots[q].ptr() == q &&& (i < h.len ==> slots[q].is_init()) }",
                Ghost,
                "stack-entry permissions",
            ),
            (
                "&&& forall|i: int| 0 <= i < h.cap ==> #[trigger] slot_in(h.slots, i)",
                Ghost,
                "allocation bounds (`in_alloc`, provenance)",
            ),
            (
                "&&& h.cap != 0 ==> slot_in(h.slots, h.cap as int)",
                Ghost,
                "allocation bounds (`in_alloc`, provenance)",
            ),
        ],
    },
    Spec {
        file: "cache.rs",
        sig: "pub(super) open spec fn inv(&self) -> bool {",
        items: &[
            (
                "&&& self.run_end <= self.low_water <= self.len <= self.cap",
                Checked,
                "HeaderOrder",
            ),
            (
                "&&& self.cap <= crate::config::MAX_NSLOTS",
                Checked,
                "HeaderCap",
            ),
            ("&&& self.zeroed <= self.low_water", Checked, "HeaderZeroed"),
        ],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn tracks(&self) -> bool {",
        items: &[(
            "forall|p: *mut CacheHeader, i: int| #![trigger self.ent(p, i)] self.hdrs.contains_key(p) && 0 <= i < self.hdr(p).cap ==> { &&& self.owner.contains_key(self.ent(p, i)) &&& self.owner[self.ent(p, i)] == (p, i) &&& (i < self.hdr(p).len ==> self.holds(self.cls[p], self.ent(p, i), self.hdr(p).slots)) }",
            Implied,
            "`owner` is ghost; one owner per entry: StacksOverlap; `holds` with \
             `cls[p]` the header's index in its array (`home_ok`, `owns_node`): \
             slot_entry",
        )],
    },
    Spec {
        file: "perms.rs",
        sig: "pub open spec fn slot_entry(c: int, e: *mut u8, base: *mut *mut u8) -> bool {",
        items: &[
            (
                "&&& process_regions().is_slot(c, plain(e, base))",
                Checked,
                "EntryNotSlot (`Regions::classify`, guard slabs excluded)",
            ),
            (
                "&&& e@.addr == stored(plain(e, base), base)",
                Checked,
                "EntryNotStored",
            ),
        ],
    },
    Spec {
        file: "zero.rs",
        sig: "pub(super) closed spec fn zero_ok(&self) -> bool {",
        items: &[(
            "forall|p: *mut CacheHeader, i: int| #![trigger self.ent(p, i)] self.hdrs.contains_key(p) && Self::in_prefix(self.hdr(p), i) ==> self.zero_at(p, i)",
            Implied,
            "zero_at",
        )],
    },
    Spec {
        file: "zero.rs",
        sig: "pub(super) open spec fn zero_at(&self, p: *mut CacheHeader, i: int) -> bool {",
        items: &[
            ("let q = self.ent(p, i);", ByWalk, "entry `i` of the stack"),
            (
                "&&& self.zero.contains_key(q)",
                Implied,
                "the token is ghost; what it states (the bytes read as zero): NotZero",
            ),
            (
                "&&& self.zero[q].addr() == plain(self.slots[q].value(), self.hdr(p).slots)",
                Implied,
                "NotZero reads the slot the entry stands for",
            ),
            (
                "&&& self.zero[q].len() >= class_table()[self.cls[p]].size",
                Implied,
                "NotZero reads the class's size",
            ),
        ],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn tokened(&self) -> bool {",
        items: &[(
            "forall|p: *mut CacheHeader, i: int| #![trigger self.ent(p, i)] self.hdrs.contains_key(p) && 0 <= i < self.hdr(p).len ==> self.tok_at(p, i)",
            Implied,
            "slot tokens are ghost; one token per slot: SlotTwice",
        )],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn claimed(&self) -> bool {",
        items: &[(
            "forall|p: *mut CacheHeader| #[trigger] self.hdrs.contains_key(p) && self.hdr(p).cap != 0 ==> stack_claimed(self.hdr(p), self.blocks)",
            Implied,
            "stack_claimed",
        )],
    },
    Spec {
        file: "perms.rs",
        sig: "pub(super) open spec fn stack_claimed(h: CacheHeader, blocks: Map<*mut *mut u8, crate::meta::MetaClaim>) -> bool {",
        items: &[
            ("&&& blocks.contains_key(h.slots)", Ghost, "metadata claims"),
            (
                "&&& blocks[h.slots].held(crate::meta::process_live_id(), h.slots as *mut u8, 8 * h.cap, 16)",
                Implied,
                "the claim is ghost; its alignment: StackMisaligned",
            ),
        ],
    },
    Spec {
        file: "theorems.rs",
        sig: "pub open spec fn regions_ok() -> bool {",
        items: &[(
            "process_regions().wf() && process_regions().separated()",
            Ghost,
            "not a fact of the heap: `process_regions()` is the `regions` of \
             the process's `Shared`, which `Regions::from_parts` makes well \
             formed and separated; the checker reads it (`Regions::classify`, \
             `Regions::in_meta`) and does not test it",
        )],
    },
    Spec {
        file: "mod.rs",
        sig: "pub closed spec fn list_ok(&self) -> bool {",
        items: &[
            ("let l = self.list@;", ByWalk, "the walk's nodes"),
            (
                "let k = self.keys@;",
                ByWalk,
                "the walk's nodes' `arena` fields",
            ),
            (
                "&&& l.len() == 0 ==> self.others@.addr == 0",
                ByWalk,
                "the walk",
            ),
            (
                "&&& l.len() > 0 ==> self.others == l[0]",
                ByWalk,
                "the walk",
            ),
            (
                "&&& forall|i: int| 0 <= i < l.len() ==> (#[trigger] l[i])@.addr != 0 && self.owns_node(l[i])",
                Implied,
                "non-null: the walk; owns_node",
            ),
            (
                "&&& forall|i: int, j: int| #![trigger l[i], l[j]] 0 <= i < j < l.len() ==> l[i] != l[j]",
                Checked,
                "ListRepeats",
            ),
            ("&&& k.len() == l.len()", ByWalk, "one key per node"),
            (
                "&&& forall|i: int, j: int| #![trigger k[i], k[j]] 0 <= i < j < k.len() ==> k[i] != k[j]",
                Checked,
                "KeyRepeats",
            ),
            (
                "&&& forall|i: int| 0 <= i < k.len() ==> #[trigger] k[i] != self.arena",
                Checked,
                "KeyIsHome",
            ),
            (
                "&&& others::heads_ok(self.heads@, l, k)",
                Implied,
                "head_ok",
            ),
        ],
    },
    Spec {
        file: "mod.rs",
        sig: "pub closed spec fn owns_node(&self, node: *mut ArenaCaches) -> bool {",
        items: &[
            (
                "&&& forall|c: int| #![trigger node_hdr(node, c)] 0 <= c < NCLASSES ==> self.owns(node_hdr(node, c)) && self.hdr_class(node_hdr(node, c)) == c",
                Implied,
                "`owns` and `hdr_class` are ghost; cell_ok of each header, its \
                 entries of class `c`, no header shared by two nodes: HeaderTwice",
            ),
            ("&&& node_placed(node)", Implied, "node_placed"),
        ],
    },
    Spec {
        file: "perms.rs",
        sig: "pub open spec fn node_placed(node: *mut ArenaCaches) -> bool {",
        items: &[
            (
                "&&& crate::vspec::ptr::in_alloc(node as *mut u8, 64)",
                Ghost,
                "allocation bounds (`in_alloc`, provenance)",
            ),
            (
                "&&& crate::vspec::ptr::in_alloc(node_hdr(node, 0), 64 * NCLASSES)",
                Ghost,
                "allocation bounds (`in_alloc`, provenance)",
            ),
            (
                "&&& node@.addr + 64 + 64 * NCLASSES <= usize::MAX",
                Checked,
                "NodeWraps",
            ),
        ],
    },
    Spec {
        file: "others.rs",
        sig: "pub open spec fn heads_ok(heads: Map<*mut ArenaCaches, NodeHead>, l: Seq<*mut ArenaCaches>, k: Seq<u16>) -> bool {",
        items: &[(
            "forall|i: int| 0 <= i < l.len() ==> #[trigger] head_ok(heads, l, k, i)",
            Implied,
            "head_ok of every node",
        )],
    },
    Spec {
        file: "others.rs",
        sig: "pub open spec fn head_ok(heads: Map<*mut ArenaCaches, NodeHead>, l: Seq<*mut ArenaCaches>, k: Seq<u16>, i: int) -> bool {",
        items: &[
            ("let n = l[i];", ByWalk, "the walk's node"),
            (
                "&&& heads.contains_key(n)",
                Ghost,
                "the node's field permissions",
            ),
            (
                "&&& heads[n].arena.ptr() == n as *mut u16",
                Ghost,
                "the node's field permissions",
            ),
            (
                "&&& heads[n].arena.is_init()",
                Ghost,
                "the node's field permissions",
            ),
            (
                "&&& heads[n].arena.value() == k[i]",
                ByWalk,
                "the key is the field the walk reads",
            ),
            (
                "&&& heads[n].next.ptr() == next_ptr(n)",
                Ghost,
                "the node's field permissions",
            ),
            (
                "&&& heads[n].next.is_init()",
                Ghost,
                "the node's field permissions",
            ),
            (
                "&&& i + 1 < l.len() ==> heads[n].next.value() == l[i + 1]",
                ByWalk,
                "the walk follows `next`",
            ),
            (
                "&&& i + 1 == l.len() ==> heads[n].next.value()@.addr == 0",
                ByWalk,
                "the walk ends at null",
            ),
            ("&&& block_ok(heads[n], n)", Implied, "block_ok"),
        ],
    },
    Spec {
        file: "others.rs",
        sig: "pub open spec fn block_ok(nh: NodeHead, n: *mut ArenaCaches) -> bool {",
        items: &[
            (
                "let pad = nh.pad.value();",
                ByWalk,
                "the `pad` field the walk reads",
            ),
            (
                "&&& nh.pad.ptr() == pad_ptr(n)",
                Ghost,
                "the node's field permissions",
            ),
            (
                "&&& nh.pad.is_init()",
                Ghost,
                "the node's field permissions",
            ),
            ("&&& pad < 16", Checked, "PadRange"),
            ("&&& 64 * pad <= n@.addr", Checked, "PadBelowZero"),
            (
                "&&& nh.rest.dom() == rest_dom(n, pad)",
                Ghost,
                "the block's permissions",
            ),
            (
                "&&& nh.rest.provenance() == n@.provenance",
                Ghost,
                "provenance",
            ),
            (
                "&&& nh.claim.held(crate::meta::process_live_id(), node_block(n, pad), block_size(), 64)",
                Implied,
                "the claim is ghost; its block in the metadata region, 64-aligned: \
                 BlockMisplaced, NodeOutsideMeta",
            ),
        ],
    },
    Spec {
        file: "mod.rs",
        sig: "pub closed spec fn home_ok(&self) -> bool {",
        items: &[
            (
                "&&& self.home_hdrs@.len() == NCLASSES",
                Ghost,
                "`home_hdrs` is ghost (the `caches` array has `NCLASSES` headers by its type)",
            ),
            (
                "&&& forall|c: int| 0 <= c < NCLASSES ==> self.owns(#[trigger] self.home_hdrs@[c]) && self.hdr_class(self.home_hdrs@[c]) == c",
                Implied,
                "`owns` and `hdr_class` are ghost (a `CacheHeader` has no class \
                 field); cell_ok of each home header and its entries of class `c`",
            ),
            (
                "&&& forall|c: int, i: int, k: int| 0 <= c < NCLASSES && 0 <= i < self.list@.len() && 0 <= k < NCLASSES ==> #[trigger] self.home_hdrs@[c] != #[trigger] node_hdr(self.list@[i], k)",
                Checked,
                "NodeOnHome",
            ),
        ],
    },
    Spec {
        file: "mod.rs",
        sig: "pub closed spec fn homed_ok(&self) -> bool {",
        items: &[
            (
                "&&& self.bound == self.homed@ is Some",
                Implied,
                "the ticket is ghost; with the next conjunct, a counted heap's \
                 home arena counts it: BoundUncounted",
            ),
            (
                "&&& self.homed@ matches Some(t) ==> homed_on(t, self.arena as int)",
                Implied,
                "the ticket is ghost; it is for the home arena, whose bound count \
                 is then at least one: BoundUncounted",
            ),
            ("&&& self.rehome ==> self.bound", Checked, "RehomeUnbound"),
        ],
    },
    Spec {
        file: "mod.rs",
        sig: "pub(super) closed spec fn home_at(&self, h: *mut Heap) -> bool {",
        items: &[
            (
                "&&& self.home_hdrs@ == Seq::new(NCLASSES as nat, |c: int| home_hdr(h, c))",
                Implied,
                "`home_hdrs` is ghost; the `caches` the heap's methods use are at \
                 `home_hdr(p, c)`: HomeAt",
            ),
            (
                "&&& h@.addr + vstd::layout::size_of::<Heap>() <= usize::MAX",
                Checked,
                "HomeWraps",
            ),
            (
                "&&& self.head@.0.is_range(h@.addr as int, 128)",
                Ghost,
                "the permission to the heap's plain fields",
            ),
            (
                "&&& self.head@.0.provenance() == h@.provenance",
                Ghost,
                "the permission to the heap's plain fields",
            ),
            (
                "&&& heap_claim(h, self.head@.1)",
                Ghost,
                "the heap block's metadata claim",
            ),
        ],
    },
    // The definitions the checker computes with, pinned so that a change
    // to one reaches the arithmetic `check` copies from it.
    Spec {
        file: "mod.rs",
        sig: "pub(super) open spec fn home_hdr(h: *mut Heap, class: int) -> *mut CacheHeader {",
        items: &[(
            "vstd::raw_ptr::ptr_mut_from_data(vstd::raw_ptr::PtrData::<CacheHeader> { addr: (h@.addr + 128 + 64 * class) as usize, provenance: h@.provenance, metadata: (), })",
            Checked,
            "`p + 128 + 64 * c`: HomeAt (the provenance is ghost)",
        )],
    },
    Spec {
        file: "perms.rs",
        sig: "pub open spec fn node_hdr(node: *mut ArenaCaches, class: int) -> *mut CacheHeader {",
        items: &[(
            "vstd::raw_ptr::ptr_mut_from_data::<CacheHeader>( vstd::raw_ptr::PtrData { addr: (node@.addr + 64 + 64 * class) as usize, provenance: node@.provenance, metadata: () }, )",
            Checked,
            "`node + 64 + 64 * k`: NodeOnHome, HeaderTwice (the provenance is ghost)",
        )],
    },
    Spec {
        file: "perms.rs",
        sig: "pub open spec fn plain(e: *mut u8, base: *mut *mut u8) -> usize {",
        items: &[(
            "if pac_mode() { crate::sys::pac::spec_auth(e@.addr, base@.addr) } else { e@.addr }",
            Checked,
            "`pac::strip` with `Shared::pac`, which is `pac::auth` of an entry \
             EntryNotStored accepts",
        )],
    },
    Spec {
        file: "perms.rs",
        sig: "pub open spec fn stored(p: usize, base: *mut *mut u8) -> usize {",
        items: &[(
            "if pac_mode() { crate::sys::pac::spec_sign(p, base@.addr) } else { p }",
            Checked,
            "`pac::sign` with `Shared::pac`: EntryNotStored",
        )],
    },
    Spec {
        file: "zero.rs",
        sig: "pub(super) open spec fn in_prefix(h: CacheHeader, i: int) -> bool {",
        items: &[(
            "0 <= i < h.len && i < h.zeroed",
            Checked,
            "the first `min(len, zeroed)` entries: NotZero",
        )],
    },
    Spec {
        file: "others.rs",
        sig: "pub open spec fn block_size() -> int {",
        items: &[(
            "size_of::<ArenaCaches>() as int + 64 * 15",
            Checked,
            "BlockMisplaced",
        )],
    },
    Spec {
        file: "others.rs",
        sig: "pub open spec fn node_block(n: *mut ArenaCaches, pad: u8) -> *mut u8 {",
        items: &[(
            "crate::vspec::ptr::ptr_sub(n as *mut u8, (64 * pad) as usize)",
            Checked,
            "`node - 64 * pad`: BlockMisplaced",
        )],
    },
];

/// The name of each [`Broken`] variant. Exhaustive, so a new variant
/// needs a name here, and [`BROKEN`] lists the same names.
fn variant_name(b: &Broken) -> &'static str {
    match b {
        Broken::Shared { .. } => "Shared",
        Broken::HomeAt { .. } => "HomeAt",
        Broken::HomeWraps => "HomeWraps",
        Broken::ListRepeats { .. } => "ListRepeats",
        Broken::NodeOutsideMeta { .. } => "NodeOutsideMeta",
        Broken::NodeWraps { .. } => "NodeWraps",
        Broken::NodeOnHome { .. } => "NodeOnHome",
        Broken::HeaderTwice { .. } => "HeaderTwice",
        Broken::PadRange { .. } => "PadRange",
        Broken::PadBelowZero { .. } => "PadBelowZero",
        Broken::BlockMisplaced { .. } => "BlockMisplaced",
        Broken::KeyRange { .. } => "KeyRange",
        Broken::KeyIsHome { .. } => "KeyIsHome",
        Broken::KeyRepeats { .. } => "KeyRepeats",
        Broken::HeaderOrder { .. } => "HeaderOrder",
        Broken::HeaderCap { .. } => "HeaderCap",
        Broken::HeaderZeroed { .. } => "HeaderZeroed",
        Broken::HeaderOutsideMeta { .. } => "HeaderOutsideMeta",
        Broken::StackOutsideMeta { .. } => "StackOutsideMeta",
        Broken::StackMisaligned { .. } => "StackMisaligned",
        Broken::EntryNotStored { .. } => "EntryNotStored",
        Broken::EntryNotSlot { .. } => "EntryNotSlot",
        Broken::StacksOverlap { .. } => "StacksOverlap",
        Broken::SlotTwice { .. } => "SlotTwice",
        Broken::NotZero { .. } => "NotZero",
        Broken::RehomeUnbound => "RehomeUnbound",
        Broken::BoundUncounted { .. } => "BoundUncounted",
    }
}

/// Every [`Broken`] variant's name ([`variant_name`]), in declaration
/// order.
const BROKEN: &[&str] = &[
    "Shared",
    "HomeAt",
    "HomeWraps",
    "ListRepeats",
    "NodeOutsideMeta",
    "NodeWraps",
    "NodeOnHome",
    "HeaderTwice",
    "PadRange",
    "PadBelowZero",
    "BlockMisplaced",
    "KeyRange",
    "KeyIsHome",
    "KeyRepeats",
    "HeaderOrder",
    "HeaderCap",
    "HeaderZeroed",
    "HeaderOutsideMeta",
    "StackOutsideMeta",
    "StackMisaligned",
    "EntryNotStored",
    "EntryNotSlot",
    "StacksOverlap",
    "SlotTwice",
    "NotZero",
    "RehomeUnbound",
    "BoundUncounted",
];

/// Check what `heap_at(p)` ensures, `r.wf()` and `r.home_at(p)`, of
/// `h`, the heap `heap_at` handed out for `p`, created on `s`; the first
/// broken conjunct, in the order below, if any. [`SPECS`] says how each
/// conjunct is covered. The conjuncts that need no sets are
/// [`heap_facts::set_free`] and [`heap_facts::homed_ok`], which the debug
/// TLS check (`tlscheck::deep`) runs too; this adds a node met twice,
/// a header of two nodes, two stacks overlapping, a slot cached twice and
/// the zero prefix.
///
/// # Safety
/// `h` is a live heap that nothing else uses while the check runs; every
/// `others` node the check reaches is read, so a corrupted list must stay
/// inside mapped memory.
unsafe fn check(h: &Heap, p: *const Heap, s: &Shared) -> Result<(), Broken> {
    // `Heap::list_ok`: the nodes are distinct; `Heap::home_ok`: each
    // header having one class, no two nodes share one.
    let mut nodes: HashSet<usize> = HashSet::new();
    let mut node_hdrs: HashSet<usize> = HashSet::new();
    // SAFETY: the caller's.
    unsafe {
        heap_facts::set_free(h, p, s, |seen| match seen {
            Seen::Node(node) if !nodes.insert(node) => Err(Broken::ListRepeats { node }),
            Seen::Hdr(hdr) if !node_hdrs.insert(hdr) => Err(Broken::HeaderTwice { hdr }),
            _ => Ok(()),
        })?;
    }
    let mut hdrs: Vec<Hdr> = Vec::new();
    // SAFETY: the caller's, and `set_free` passed the list.
    unsafe {
        heap_facts::headers(h, |d| {
            hdrs.push(d);
            Ok(())
        })?;
    }
    let pac = s.pac();
    // `CachePerms::tracks`: `owner` maps every entry of every stack to one
    // header, so no two stacks share an entry.
    let mut stacks: Vec<(usize, usize, usize)> = hdrs
        .iter()
        .filter(|d| d.h.cap != 0)
        .map(|d| {
            (
                d.h.slots as usize,
                d.h.slots as usize + 8 * d.h.cap as usize,
                d.at,
            )
        })
        .collect();
    stacks.sort();
    for w in stacks.windows(2) {
        if w[1].0 < w[0].1 {
            return Err(Broken::StacksOverlap {
                a: w[0].2,
                b: w[1].2,
            });
        }
    }
    // `CachePerms::tokened`: each cached entry holds its slot's token, one
    // per slot, so no slot is cached twice.
    let mut slots: HashSet<usize> = HashSet::new();
    for d in &hdrs {
        for &e in d.h.cached() {
            let slot = plain(e, pac);
            if !slots.insert(slot) {
                return Err(Broken::SlotTwice { slot });
            }
        }
    }
    // `CachePerms::zero_ok`: each entry of the zero prefix (the first
    // `min(len, zeroed)`) reads as zero for its class's size.
    for d in &hdrs {
        let k = d.h;
        let n = k.len.min(k.zeroed) as usize;
        for (i, &e) in k.cached()[..n].iter().enumerate() {
            let slot = plain(e, pac) as *const u8;
            // SAFETY: a cached slot start of `d.class` (checked above), so
            // its class's size of bytes are mapped slab memory no one uses.
            let bytes = unsafe { core::slice::from_raw_parts(slot, CLASSES[d.class].size()) };
            if bytes.iter().any(|&b| b != 0) {
                return Err(Broken::NotZero { hdr: d.at, i });
            }
        }
    }
    heap_facts::homed_ok(h, s)
}

/// [`check`], and `heap_facts::no_sets` (what the debug TLS check's
/// `deep` tests) of the same heap, which must report the same broken
/// conjunct; `check`'s result. Where `check` found a node met twice,
/// `no_sets` must stop at that node's key, met twice too. Not compared:
/// the conjuncts only `check`'s sets show past that (a header of two
/// nodes, two stacks overlapping, a slot cached twice, the zero prefix).
///
/// # Safety
/// As for [`check`].
unsafe fn check_both(h: &Heap, p: *const Heap, s: &Shared) -> Result<(), Broken> {
    // SAFETY: the caller's.
    let got = unsafe { check(h, p, s) };
    if matches!(
        got,
        Err(Broken::HeaderTwice { .. }
            | Broken::StacksOverlap { .. }
            | Broken::SlotTwice { .. }
            | Broken::NotZero { .. })
    ) {
        return got;
    }
    // SAFETY: the caller's.
    let free = unsafe { heap_facts::no_sets(h, p, s) };
    if let Err(Broken::ListRepeats { node }) = got {
        assert!(
            matches!(free, Err(Broken::KeyRepeats { node: n, .. }) if n == node),
            "no_sets on a cycle at {node:#x}: got = {free:?}, want = KeyRepeats there"
        );
    } else {
        assert_eq!(free, got, "no_sets: got = {free:?}, want = {got:?}");
    }
    got
}

/// [`check`] of explicit cache `id` of `s` as [`Tcaches::get`] hands it
/// out, with the table's entry as `p`; panics when `id` has no cache.
fn check_tcache(s: &Shared, id: u16) -> Result<(), Broken> {
    let p = s.tcaches.entry(id);
    let h = s
        .tcaches
        .get(id, Tracked::assume_new())
        .expect("a live explicit cache");
    // SAFETY: the caller's cache, which it uses on no other thread.
    unsafe { check_both(h, p, s) }
}

/// [`check_tcache`], which must pass.
#[track_caller]
pub(crate) fn assert_tcache_ok(s: &Shared, id: u16, after: &str) {
    if let Err(e) = check_tcache(s, id) {
        panic!("explicit cache {id} after {after}: {e:?}");
    }
}

/// The top-level items of the body of the spec fn whose trimmed line is
/// `sig` in `text`, as [`SPECS`] spells them: an item starts at a line of
/// the body's own indentation that starts with `&&& ` or `let `, and
/// takes the deeper lines after it; a body with no such line is one item.
/// Comments are skipped and whitespace collapsed.
fn spec_items(text: &str, sig: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let at: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].trim() == sig)
        .collect();
    assert_eq!(at.len(), 1, "`{sig}`: got = {} lines, want = 1", at.len());
    let indent = |l: &str| l.len() - l.trim_start().len();
    let own = indent(lines[at[0]]);
    let mut items: Vec<String> = Vec::new();
    let mut base = None;
    for l in &lines[at[0] + 1..] {
        let t = l.trim();
        if indent(l) == own && t == "}" {
            return items
                .iter()
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect();
        }
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        let b = *base.get_or_insert(indent(l));
        if indent(l) == b && (t.starts_with("&&& ") || t.starts_with("let ")) || items.is_empty() {
            items.push(String::from(t));
        } else {
            let last = items.last_mut().unwrap();
            last.push(' ');
            last.push_str(t);
        }
    }
    panic!("`{sig}`: no closing brace");
}

/// [`SPECS`] is `Heap::wf`, `Heap::home_at` and the specs they name as
/// the source has them: each spec's top-level conjuncts, in order, word
/// for word. A conjunct that changes fails this until the table follows.
/// Every [`Broken`] variant is named by a row the checker tests
/// (`KeyRange`, not a conjunct, excepted), and every variant name a row
/// cites is one of them, so a row and the check it names cannot part
/// silently.
#[test]
fn checker_transcribes_heap_wf_conjunct_by_conjunct() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/heap");
    for spec in SPECS {
        let text = std::fs::read_to_string(dir.join(spec.file)).unwrap();
        let got = spec_items(&text, spec.sig);
        let want: Vec<&str> = spec.items.iter().map(|i| i.0).collect();
        assert_eq!(got, want, "heap/{}: `{}`", spec.file, spec.sig);
    }
    // The words of the tested rows' reasons that name a variant, or look
    // like one: letters only, starting with a capital, with another
    // capital and a lowercase letter.
    let words: Vec<&str> = SPECS
        .iter()
        .flat_map(|s| s.items.iter())
        .filter(|i| i.1 == Checked || i.1 == Implied)
        .flat_map(|i| i.2.split(|ch: char| !ch.is_ascii_alphabetic()))
        .filter(|w| {
            BROKEN.contains(w)
                || (w.starts_with(|ch: char| ch.is_ascii_uppercase())
                    && w.chars().filter(|ch| ch.is_ascii_uppercase()).count() >= 2
                    && w.chars().any(|ch| ch.is_ascii_lowercase()))
        })
        .collect();
    for w in &words {
        // Type names a reason cites, not variants.
        let types = ["CachePerms", "CacheHeader"];
        assert!(
            BROKEN.contains(w) || types.contains(w),
            "SPECS names `{w}`: not a `Broken` variant"
        );
    }
    for name in BROKEN {
        assert!(
            *name == "KeyRange" || words.contains(name),
            "`Broken::{name}`: no tested row of SPECS names it"
        );
    }
    // `BROKEN` is `variant_name`'s names, in order.
    let all = [
        Broken::Shared { got: 0, want: 0 },
        Broken::HomeAt { class: 0 },
        Broken::HomeWraps,
        Broken::ListRepeats { node: 0 },
        Broken::NodeOutsideMeta { node: 0 },
        Broken::NodeWraps { node: 0 },
        Broken::NodeOnHome { node: 0, class: 0 },
        Broken::HeaderTwice { hdr: 0 },
        Broken::PadRange { node: 0, pad: 0 },
        Broken::PadBelowZero { node: 0, pad: 0 },
        Broken::BlockMisplaced { node: 0 },
        Broken::KeyRange { node: 0, key: 0 },
        Broken::KeyIsHome { node: 0, key: 0 },
        Broken::KeyRepeats { node: 0, key: 0 },
        Broken::HeaderOrder { hdr: 0 },
        Broken::HeaderCap { hdr: 0 },
        Broken::HeaderZeroed { hdr: 0 },
        Broken::HeaderOutsideMeta { hdr: 0 },
        Broken::StackOutsideMeta { hdr: 0 },
        Broken::StackMisaligned { hdr: 0 },
        Broken::EntryNotStored { hdr: 0, i: 0 },
        Broken::EntryNotSlot { hdr: 0, i: 0 },
        Broken::StacksOverlap { a: 0, b: 0 },
        Broken::SlotTwice { slot: 0 },
        Broken::NotZero { hdr: 0, i: 0 },
        Broken::RehomeUnbound,
        Broken::BoundUncounted { arena: 0 },
    ];
    let names: Vec<&str> = all.iter().map(variant_name).collect();
    assert_eq!(names, BROKEN);
}

/// The `Shared` a test owns, with a second arena for `MALLOCX_ARENA` and
/// `others` nodes.
fn fixture() -> (&'static Shared, u16) {
    let s = TestShared::new().build();
    let a1 = s
        .arenas
        .create(s, Tracked::assume_new(), Tracked::assume_new())
        .expect("arenas.create");
    (s, a1)
}

pub(crate) fn tcache_create(s: &Shared, arena: u16) -> u16 {
    s.tcaches
        .create(s, arena, Tracked::assume_new(), Tracked::assume_new())
        .expect("tcache.create")
}

fn tcache_destroy(s: &Shared, id: u16) -> bool {
    s.tcaches
        .destroy(s, id, Tracked::assume_new(), Tracked::assume_new())
}

/// `mallocx` of `size` (a class size or huge) through explicit cache
/// `t`, from `arena` when given.
fn xalloc(s: &Shared, size: usize, arena: Option<u16>, t: u16) -> *mut u8 {
    let p = match class_of(size) {
        Some(c) => {
            mallocx::alloc(
                s,
                c,
                arena,
                mallocx::Tcache::Explicit(t),
                Tracked::assume_new(),
                Tracked::assume_new(),
            )
            .0
        }
        None => {
            mallocx::alloc_huge(
                s,
                size,
                1,
                arena,
                mallocx::Tcache::Explicit(t),
                false,
                Tracked::assume_new(),
            )
            .0
        }
    };
    assert!(!p.is_null(), "{size} bytes through cache {t}");
    p
}

/// `dallocx` of `p` through explicit cache `t`.
fn xfree(s: &Shared, p: *mut u8, t: u16) {
    mallocx::free(
        s,
        p,
        mallocx::Tcache::Explicit(t),
        Tracked::assume_new(),
        Tracked::assume_new(),
    );
}

/// `h` is as `create_detached` leaves a heap: no home header has a stack
/// or has served a pop, and `others` is null.
fn is_fresh(h: &Heap) -> bool {
    h.others.is_null()
        && h.caches
            .iter()
            .all(|k| k.len == 0 && k.cap == 0 && k.allocs == 0)
}

/// An explicit cache through its whole life, the checker on its heap
/// after every step: `tcache.create`, allocations and frees through
/// `MALLOCX_TCACHE` (home arena, `MALLOCX_ARENA` into an `others` node, a
/// plain free of another arena's slot onto `FOREIGN`, a huge run),
/// `tcache.flush` (every cache drained), `tcache.destroy` (the id then
/// names no cache), and `tcache.create` again, which reuses the id for a
/// heap that passes and is fresh, not the old one with its caches.
#[test]
fn explicit_cache_lifecycle_passes_the_checker() {
    let (s, a1) = fixture();
    let t = tcache_create(s, 0);
    assert_tcache_ok(s, t, "create");
    let mut live = Vec::new();
    for size in [16, 64, 100, 1000, 4096, 20_000] {
        for _ in 0..3 {
            live.push(xalloc(s, size, None, t));
            assert_tcache_ok(s, t, "mallocx");
        }
        live.push(xalloc(s, size, Some(a1), t));
        assert_tcache_ok(s, t, "mallocx with MALLOCX_ARENA");
    }
    // A slot of `a1` from another explicit cache homed there, freed
    // through `t`: onto `t`'s FOREIGN node.
    let u = tcache_create(s, a1);
    let q = xalloc(s, 64, None, u);
    assert_tcache_ok(s, u, "mallocx on a1");
    xfree(s, q, t);
    assert_tcache_ok(s, t, "a foreign free");
    let huge = xalloc(s, crate::layout::MEDIUM_MAX + 1, None, t);
    assert_tcache_ok(s, t, "a huge mallocx");
    xfree(s, huge, t);
    for p in live.drain(..) {
        xfree(s, p, t);
        assert_tcache_ok(s, t, "dallocx");
    }
    let h = s.tcaches.get(t, Tracked::assume_new()).unwrap();
    let nodes = Heap::nodes(h.others).count();
    assert_eq!(nodes, 2, "the a1 node and the FOREIGN node");
    assert!(h.caches.iter().any(|k| k.len > 0), "caches hold slots");
    assert!(s.tcaches.flush(s, t, Tracked::assume_new()));
    assert_tcache_ok(s, t, "tcache.flush");
    let h = s.tcaches.get(t, Tracked::assume_new()).unwrap();
    assert!(h.caches.iter().all(|k| k.len == 0));
    for o in Heap::nodes(h.others) {
        // SAFETY: a node of the live heap's list.
        assert!(unsafe { &*o }.caches.iter().all(|k| k.len == 0));
    }
    let old = s.tcaches.entry(t);
    assert!(tcache_destroy(s, t));
    assert!(s.tcaches.get(t, Tracked::assume_new()).is_none());
    assert!(s.tcaches.entry(t).is_null());
    // The id comes back, for a fresh heap that passes.
    assert_eq!(tcache_create(s, 0), t);
    assert_tcache_ok(s, t, "the id's reuse");
    let h = s.tcaches.get(t, Tracked::assume_new()).unwrap();
    assert!(
        is_fresh(h),
        "the reused id's heap ({:#x}, the old one was {:#x}) is not fresh",
        s.tcaches.entry(t) as usize,
        old as usize
    );
    assert!(tcache_destroy(s, t));
    assert!(tcache_destroy(s, u));
    assert!(s.tcaches.is_empty());
}

/// The heap at `h` passes the checker, with `h` as the pointer.
#[track_caller]
fn assert_heap_ok(h: *mut Heap, s: &Shared, after: &str) {
    // SAFETY: the caller's live heap, used by this thread only.
    if let Err(e) = unsafe { check_both(&*h, h, s) } {
        panic!("heap {:#x} after {after}: {e:?}", h as usize);
    }
}

/// Fill a thread heap's caches: home, an `others` node for `a1`, and the
/// FOREIGN node through a plain free of an `a1` slot.
fn fill_thread_heap(s: &Shared, a1: u16) {
    let c = class_of(64).unwrap();
    let mut ps = Vec::new();
    for _ in 0..8 {
        ps.push(alloc(s, c, Tracked::assume_new(), Tracked::assume_new()));
    }
    let h = existing_heap(s).expect("the thread's heap");
    for _ in 0..4 {
        let (p, _) = h.alloc_in(
            s,
            a1,
            c,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert!(!p.is_null());
        ps.push(p);
    }
    for p in ps {
        free(s, p, Tracked::assume_new(), Tracked::assume_new());
    }
}

/// The thread key's value on this thread: the pointer the C library
/// hands `heap_destructor`, which hands it to `heap_at`.
fn key_value(s: &Shared) -> *mut Heap {
    let key = s.thread_key().expect("the thread key");
    // SAFETY: `key` is the allocator's live thread key.
    unsafe { libc::pthread_getspecific(key) as *mut Heap }
}

/// Thread heaps reach `heap_at` through `heap_destructor` and
/// `Heap::destroy`: the checker passes on the heap the thread key holds
/// with its caches filled, and the destructor then frees it, run by hand
/// (the bound count and the metadata go back) and by thread exit (the
/// bound count goes back).
#[test]
fn thread_heaps_pass_the_checker_before_their_destructor() {
    let (s, a1) = fixture();
    let meta = |s: &Shared| s.meta.stats(Tracked::assume_new()).allocated;
    // By thread exit: glibc runs the destructor with the key's value. This
    // also builds the class tables the fill touches, so the run by hand
    // below allocates metadata for its heap alone.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        fill_thread_heap(s, a1);
        let kv = key_value(s);
        assert_heap_ok(kv, s, "filling a thread heap");
        // SAFETY: the live heap of this thread.
        let home = unsafe { (*kv).arena };
        tx.send((home, s.arenas.bound(home))).unwrap();
    })
    .join()
    .unwrap();
    let (home, bound) = rx.recv().unwrap();
    assert_eq!(s.arenas.bound(home), bound - 1, "the destructor unbound it");
    // By hand, as `heap::linux_tests` does: clear the key first, as glibc
    // does before calling a destructor.
    std::thread::spawn(move || {
        let meta_before = meta(s);
        fill_thread_heap(s, a1);
        let kv = key_value(s);
        assert!(!kv.is_null());
        assert_eq!(
            existing_heap(s).map(|h| h as *mut Heap),
            Some(kv),
            "TLS and the key name one heap"
        );
        assert_heap_ok(kv, s, "filling a thread heap");
        // SAFETY: the live heap of this thread.
        let home = unsafe { (*kv).arena };
        let bound = s.arenas.bound(home);
        let key = s.thread_key().unwrap();
        assert_eq!(
            // SAFETY: `key` is the allocator's live thread key.
            unsafe { libc::pthread_setspecific(key, core::ptr::null()) },
            0
        );
        // SAFETY: this thread's heap as registered with the key, which was
        // just cleared, so neither the key nor TLS hands it out again.
        unsafe { heap_destructor(kv as *mut c_void) };
        assert_eq!(
            crate::sys::tls::get(Tracked::assume_new()) as usize,
            TLS_EXITED
        );
        assert_eq!(s.arenas.bound(home), bound - 1);
        assert_eq!(meta(s), meta_before, "the heap and its nodes are freed");
    })
    .join()
    .unwrap();
}

/// `Heap::destroy` of a heap `Heap::create` returned, the checker on it
/// first; `Tcaches::destroy` hands `Heap::destroy` the entry it swapped
/// out, which is `explicit_cache_lifecycle_passes_the_checker`'s.
#[test]
fn created_heap_passes_the_checker_before_destroy() {
    let (s, a1) = fixture();
    let (h, _, _) = Heap::create(s, Tracked::assume_new());
    assert!(!h.is_null());
    assert_heap_ok(h, s, "Heap::create");
    let c = class_of(100).unwrap();
    let mut ps = Vec::new();
    for arena in [None, Some(a1)] {
        for _ in 0..5 {
            // SAFETY: the heap `create` returned, used by this thread only.
            let heap = unsafe { &mut *h };
            let (p, _) = match arena {
                None => heap.alloc_zeroed(
                    s,
                    c,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                ),
                Some(a) => heap.alloc_in(
                    s,
                    a,
                    c,
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                    Tracked::assume_new(),
                ),
            };
            assert!(!p.is_null());
            ps.push(p);
            assert_heap_ok(h, s, "an allocation");
        }
    }
    for p in ps {
        // SAFETY: as above.
        unsafe { &mut *h }.free(
            s,
            p,
            Tracked::assume_new(),
            Tracked::assume_new(),
            Tracked::assume_new(),
        );
        assert_heap_ok(h, s, "a free");
    }
    // SAFETY: as above.
    let home = unsafe { (*h).arena };
    let bound = s.arenas.bound(home);
    // SAFETY: `h` came from `Heap::create` on `s`; nothing reaches it
    // after this.
    unsafe { Heap::destroy(h, s, Tracked::assume_new(), Tracked::assume_new()) };
    assert_eq!(s.arenas.bound(home), bound - 1);
}

/// Threads, each owning two explicit cache ids, run random sequences of
/// `mallocx` (home arena, `MALLOCX_ARENA`, huge), `dallocx` through
/// either of their caches, `tcache.flush` and `tcache.destroy` with the
/// id's re-creation, the checker on both caches after every operation.
#[test]
fn explicit_caches_pass_the_checker_under_concurrent_random_use() {
    let (s, a1) = fixture();
    let threads: Vec<_> = (0..4u64)
        .map(|n| {
            std::thread::spawn(move || {
                let mut rng = 0x9e37_79b9_7f4a_7c15 ^ (n + 1);
                let mut ids = [tcache_create(s, 0), tcache_create(s, a1)];
                let mut live: Vec<*mut u8> = Vec::new();
                for op in 0..600 {
                    let r = xorshift(&mut rng);
                    let t = ids[(r >> 8) as usize % 2];
                    match r % 16 {
                        0..=6 => {
                            let sizes = [8, 16, 48, 64, 200, 1000, 3000, 9000];
                            let size = sizes[(r >> 16) as usize % sizes.len()];
                            let arena = [None, Some(0), Some(a1)][(r >> 24) as usize % 3];
                            // `MALLOCX_ARENA` of the cache's own home goes
                            // to its home caches; of the other, a node.
                            live.push(xalloc(s, size, arena, t));
                        }
                        7..=12 if !live.is_empty() => {
                            let i = (r >> 16) as usize % live.len();
                            xfree(s, live.swap_remove(i), t);
                        }
                        13 => {
                            let p = xalloc(s, crate::layout::MEDIUM_MAX + 1, None, t);
                            xfree(s, p, t);
                        }
                        14 => assert!(s.tcaches.flush(s, t, Tracked::assume_new())),
                        15 => {
                            let i = ids.iter().position(|&x| x == t).unwrap();
                            let home = [0, a1][i];
                            assert!(tcache_destroy(s, t));
                            ids[i] = tcache_create(s, home);
                        }
                        _ => {}
                    }
                    for &id in &ids {
                        if let Err(e) = check_tcache(s, id) {
                            panic!("thread {n}, op {op}: cache {id}: {e:?}");
                        }
                    }
                }
                for p in live {
                    xfree(s, p, ids[0]);
                }
                for id in ids {
                    assert_tcache_ok(s, id, "the last frees");
                    assert!(tcache_destroy(s, id));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert!(s.tcaches.is_empty());
}

/// A fork child uses the explicit cache its parent created, as the C
/// contract allows (one thread uses it at a time; the child is a copy
/// with its own): the checker passes on it in the child before and after
/// allocations and frees through it, a flush and its destroy, and in the
/// parent afterwards, which the child's writes never reached.
#[test]
fn fork_child_uses_the_parents_explicit_cache() {
    let (s, a1) = fixture();
    let t = tcache_create(s, 0);
    let kept: Vec<*mut u8> = (0..6)
        .map(|i| xalloc(s, 64, [None, Some(a1)][i % 2], t))
        .collect();
    for &p in &kept[..3] {
        xfree(s, p, t);
    }
    assert_tcache_ok(s, t, "the parent's frees");
    let kept: Vec<usize> = kept[3..].iter().map(|&p| p as usize).collect();
    let child_kept = kept.clone();
    crate::testutil::in_fork_child("explicit cache in a fork child", 20_000, move || {
        assert_tcache_ok(s, t, "fork");
        let ps: Vec<*mut u8> = (0..20).map(|_| xalloc(s, 200, Some(a1), t)).collect();
        assert_tcache_ok(s, t, "the child's mallocx");
        for p in ps {
            xfree(s, p, t);
        }
        for &p in &child_kept {
            xfree(s, p as *mut u8, t);
        }
        assert_tcache_ok(s, t, "the child's dallocx");
        assert!(s.tcaches.flush(s, t, Tracked::assume_new()));
        assert_tcache_ok(s, t, "the child's flush");
        assert!(tcache_destroy(s, t));
        assert!(s.tcaches.get(t, Tracked::assume_new()).is_none());
    });
    assert_tcache_ok(s, t, "the child's exit");
    for &p in &kept {
        xfree(s, p as *mut u8, t);
    }
    assert_tcache_ok(s, t, "the parent's last frees");
    assert!(tcache_destroy(s, t));
}

/// An explicit cache homed on arena 0 with home caches, the `a1` node and
/// the FOREIGN node filled, for the mutation tests; its slot stacks hold
/// a dirty slot above the zero prefix. Returns the id, the heap pointer
/// and `a1`.
fn filled_cache() -> (&'static Shared, u16, *mut Heap, u16) {
    let (s, a1) = fixture();
    let t = tcache_create(s, 0);
    let u = tcache_create(s, a1);
    let mut live = Vec::new();
    for size in [64, 100] {
        for _ in 0..4 {
            live.push(xalloc(s, size, None, t));
            live.push(xalloc(s, size, Some(a1), t));
        }
    }
    let foreign = xalloc(s, 64, None, u);
    xfree(s, foreign, t);
    for p in live {
        // SAFETY: a live 64-byte (at least) allocation: dirty it, so its
        // cache entry above the zero prefix does not read as zero.
        unsafe { core::ptr::write_bytes(p, 0xa5, 64) };
        xfree(s, p, t);
    }
    assert_tcache_ok(s, t, "filling");
    (s, t, s.tcaches.entry(t), a1)
}

/// Each mutation corrupts one fact the checker reads, which the heap
/// shows in memory, then puts it back: the checker reports the conjunct
/// it breaks, and passes again once it is restored. The mutations go
/// through raw pointers, so the checker's and `heap_at`'s references are
/// the only ones to the heap while they live.
#[test]
fn checker_fails_each_corrupted_fact() {
    let (s, t, hp, a1) = filled_cache();
    let c64 = class_of(64).unwrap();
    let c100 = class_of(100).unwrap();
    // `check` makes its own `&Heap`, and `restored` reaches the heap
    // through `heap_at`'s `&mut Heap`, so no reference to the heap, a
    // header or a node is live across either call: every mutation reads
    // and writes in place through `hp`, `hdr(..)` and the node pointers,
    // and the only references are the ones `check` makes for its call.
    let hdr = move |c: usize| -> *mut CacheHeader {
        // SAFETY: a field of the explicit cache's live heap, projected
        // without a reference.
        unsafe { &raw mut (*hp).caches[c] }
    };
    // SAFETY: the explicit cache's live heap, which this test alone uses,
    // read in place. Each `unsafe` below marked "as above" reads or writes
    // a field of that heap, of one of its headers or of one of its nodes in
    // the same way, with no reference live, and every write is put back.
    let nodes: Vec<*mut ArenaCaches> = Heap::nodes(unsafe { (*hp).others }).collect();
    assert_eq!(nodes.len(), 2);
    // SAFETY: as above.
    let a1_node = if unsafe { (*nodes[0]).arena } == a1 {
        nodes[0]
    } else {
        nodes[1]
    };
    let run = move |what: &str, want: fn(&Broken) -> bool| {
        // SAFETY: the live heap, with no reference to it live (see above);
        // `check`'s `&Heap` lasts for the call.
        let got = unsafe { check_both(&*hp, hp, s) };
        assert!(
            got.as_ref().err().is_some_and(want),
            "mutant `{what}`: got = {got:?}"
        );
    };
    let restored = |what: &str| assert_tcache_ok(s, t, what);

    // `shared` names another `Shared`.
    // SAFETY: as above.
    let saved = unsafe { (*hp).shared };
    // SAFETY: as above.
    unsafe { (*hp).shared = (s as *const Shared).wrapping_add(1) };
    run("shared", |e| matches!(e, Broken::Shared { .. }));
    // SAFETY: as above.
    unsafe { (*hp).shared = saved };
    restored("shared");

    // The pointer `heap_at` is handed is not the heap's.
    // SAFETY: as in `run`.
    let got = unsafe { check_both(&*hp, (hp as usize + 64) as *const Heap, s) };
    assert_eq!(got, Err(Broken::HomeAt { class: 0 }), "mutant `p`");

    // The pointer `heap_at` is handed is a `Heap`'s size from the top of
    // the address space (the checker does arithmetic on it, no more).
    let top = usize::MAX - core::mem::size_of::<Heap>() + 1;
    // SAFETY: as in `run`.
    let got = unsafe { check_both(&*hp, top as *const Heap, s) };
    assert_eq!(got, Err(Broken::HomeWraps), "mutant `p wraps`");

    // Home header order: `low_water > len`.
    let k = hdr(c64);
    // SAFETY: as above.
    let saved = unsafe { ((*k).low_water, (*k).zeroed, (*k).run_end) };
    // SAFETY: as above.
    unsafe {
        assert!((*k).len > 0);
        (*k).low_water = (*k).len + 1;
    }
    run("low_water", |e| matches!(e, Broken::HeaderOrder { .. }));
    // SAFETY: as above.
    unsafe { ((*k).low_water, (*k).zeroed, (*k).run_end) = saved };
    restored("low_water");

    // `run_end > low_water`.
    // SAFETY: as above.
    let saved = unsafe { (*k).run_end };
    // SAFETY: as above.
    unsafe { (*k).run_end = (*k).low_water + 1 };
    run("run_end", |e| matches!(e, Broken::HeaderOrder { .. }));
    // SAFETY: as above.
    unsafe { (*k).run_end = saved };
    restored("run_end");

    // `zeroed > low_water`.
    // SAFETY: as above.
    let saved = unsafe { (*k).zeroed };
    // SAFETY: as above.
    unsafe { (*k).zeroed = (*k).low_water + 1 };
    run("zeroed", |e| matches!(e, Broken::HeaderZeroed { .. }));
    // SAFETY: as above.
    unsafe { (*k).zeroed = saved };
    restored("zeroed");

    // `cap > MAX_NSLOTS`, on a header with no stack.
    let unused = (0..NCLASSES)
        .find(|&c| {
            // SAFETY: as above.
            unsafe { (*hdr(c)).cap == 0 }
        })
        .unwrap();
    let u = hdr(unused);
    // SAFETY: as above.
    unsafe { (*u).cap = crate::config::MAX_NSLOTS + 1 };
    run("cap", |e| matches!(e, Broken::HeaderCap { .. }));
    // SAFETY: as above.
    unsafe { (*u).cap = 0 };
    restored("cap");

    // A stack outside the metadata region.
    let mut outside = [core::ptr::null_mut::<u8>(); 2];
    // SAFETY: as above.
    unsafe { ((*u).cap, (*u).slots) = (2, outside.as_mut_ptr()) };
    run("stack outside meta", |e| {
        matches!(e, Broken::StackOutsideMeta { .. })
    });
    // SAFETY: as above.
    unsafe { ((*u).cap, (*u).slots) = (0, core::ptr::null_mut()) };
    restored("stack outside meta");

    // A stack off its 16-byte alignment (one entry up).
    // SAFETY: as above; a bitwise copy of the header, written back whole.
    let saved = unsafe { core::ptr::read(k) };
    // SAFETY: as above; `slots + 1` is inside the stack (`cap > 1`), and
    // nothing reads through it.
    unsafe {
        assert!((*k).cap > 1);
        (*k).slots = (*k).slots.add(1);
        (*k).cap -= 1;
        ((*k).len, (*k).low_water, (*k).zeroed, (*k).run_end) = (0, 0, 0, 0);
    }
    run("stack alignment", |e| {
        matches!(e, Broken::StackMisaligned { .. })
    });
    // SAFETY: as above.
    unsafe { core::ptr::write(k, saved) };
    restored("stack alignment");

    // The entries below are of the class's stack, which is metadata the
    // heap owns, not the heap: they are read and written through `slots`.
    // SAFETY: as above.
    let stack = unsafe { (*k).slots };
    let sign = |p: usize| {
        if s.pac() {
            crate::sys::pac::sign(p, stack as usize)
        } else {
            p
        }
    };

    // A cached entry that is a slot of another class.
    // SAFETY: as above; entry 0 of the other class's stack, below its
    // `len` (asserted).
    let other = unsafe {
        let o = hdr(c100);
        assert!((*o).len > 0);
        plain((*o).slots.read(), s.pac())
    };
    // SAFETY: entry 0 of the stack, below `len`.
    let saved = unsafe { stack.read() };
    // SAFETY: as above.
    unsafe { stack.write(sign(other) as *mut u8) };
    run("entry class", |e| matches!(e, Broken::EntryNotSlot { .. }));
    // SAFETY: as above.
    unsafe { stack.write(saved) };
    restored("entry class");

    // A cached entry that is not a slot start (one byte in).
    // SAFETY: as above.
    unsafe { stack.write(sign(plain(saved, s.pac()) + 1) as *mut u8) };
    run("entry inside a slot", |e| {
        matches!(e, Broken::EntryNotSlot { .. })
    });
    // SAFETY: as above.
    unsafe { stack.write(saved) };
    restored("entry inside a slot");

    // A cached entry stored unsigned under pointer authentication: the
    // entry's own slot address, plain, in the first entry whose signed
    // form differs from it (a signature can be zero). Without pointer
    // authentication `stored` is the identity, so every entry is stored
    // as a push stores it.
    if s.pac() {
        // SAFETY: as above.
        let len = unsafe { (*k).len } as usize;
        let (i, entry, p) = (0..len)
            .map(|i| {
                // SAFETY: entry `i`, below `len`.
                let e = unsafe { stack.add(i).read() };
                (i, e, plain(e, true))
            })
            .find(|&(_, _, p)| sign(p) != p)
            .expect("every entry's signed form is its address");
        // SAFETY: entry `i`, below `len`.
        unsafe { stack.add(i).write(p as *mut u8) };
        run("entry unsigned", |e| {
            matches!(e, Broken::EntryNotStored { .. })
        });
        // SAFETY: as above.
        unsafe { stack.add(i).write(entry) };
        restored("entry unsigned");
    } else {
        crate::testutil::print_skip("heap_at/entry_unsigned", "no pointer authentication");
    }

    // A slot cached twice: entry 1 is entry 0's slot.
    // SAFETY: as above.
    assert!(unsafe { (*k).len } >= 2);
    // SAFETY: entry 1, below `len`.
    let saved = unsafe { stack.add(1).read() };
    // SAFETY: entries 0 and 1, below `len`.
    unsafe {
        stack
            .add(1)
            .write(sign(plain(stack.read(), s.pac())) as *mut u8)
    };
    run("slot twice", |e| matches!(e, Broken::SlotTwice { .. }));
    // SAFETY: as above.
    unsafe { stack.add(1).write(saved) };
    restored("slot twice");

    // Two stacks sharing entries: the `a1` node's header for the class
    // is a copy of the home one.
    // SAFETY: as above.
    let nk = unsafe { &raw mut (*a1_node).caches[c64] };
    // SAFETY: as above; bitwise copies of the two headers.
    let saved = unsafe { core::ptr::read(nk) };
    // SAFETY: as above.
    unsafe { core::ptr::write(nk, core::ptr::read(k)) };
    run("stacks overlap", |e| {
        matches!(e, Broken::StacksOverlap { .. })
    });
    // SAFETY: as above.
    unsafe { core::ptr::write(nk, saved) };
    restored("stacks overlap");

    // The zero prefix covering a dirty slot.
    // SAFETY: as above.
    let saved = unsafe { ((*k).low_water, (*k).zeroed) };
    // SAFETY: as above.
    unsafe { ((*k).low_water, (*k).zeroed) = ((*k).len, (*k).len) };
    run("zero prefix", |e| matches!(e, Broken::NotZero { .. }));
    // SAFETY: as above.
    unsafe { ((*k).low_water, (*k).zeroed) = saved };
    restored("zero prefix");

    let (n0, n1) = (nodes[0], nodes[1]);

    // A node keyed with the home arena.
    // SAFETY: as above.
    let saved = unsafe { (*n0).arena };
    // SAFETY: as above.
    unsafe { (*n0).arena = (*hp).arena };
    run("key is home", |e| matches!(e, Broken::KeyIsHome { .. }));
    // SAFETY: as above.
    unsafe { (*n0).arena = saved };
    restored("key is home");

    // Two nodes with one key.
    // SAFETY: as above.
    let saved = unsafe { (*n1).arena };
    // SAFETY: as above.
    unsafe { (*n1).arena = (*n0).arena };
    run("key twice", |e| matches!(e, Broken::KeyRepeats { .. }));
    // SAFETY: as above.
    unsafe { (*n1).arena = saved };
    restored("key twice");

    // A key that is no arena and not FOREIGN.
    // SAFETY: as above.
    let saved = unsafe { (*n0).arena };
    // SAFETY: as above.
    unsafe { (*n0).arena = crate::arena::MAX_ARENAS as u16 };
    run("key range", |e| matches!(e, Broken::KeyRange { .. }));
    // SAFETY: as above.
    unsafe { (*n0).arena = saved };
    restored("key range");

    // A cycle: the last node's `next` is the first.
    // SAFETY: as above.
    assert!(unsafe { (*n1).next }.is_null());
    // SAFETY: as above.
    unsafe { (*n1).next = n0 };
    run("cycle", |e| matches!(e, Broken::ListRepeats { .. }));
    // SAFETY: as above.
    unsafe { (*n1).next = core::ptr::null_mut() };
    restored("cycle");

    // A node `pad` out of range.
    // SAFETY: as above.
    let saved = unsafe { (*n0).pad };
    // SAFETY: as above.
    unsafe { (*n0).pad = 16 };
    run("pad", |e| matches!(e, Broken::PadRange { .. }));
    // SAFETY: as above.
    unsafe { (*n0).pad = saved };
    restored("pad");

    // A node whose headers are the home headers: `others` one line into
    // the heap.
    // SAFETY: as above.
    let saved = unsafe { (*hp).others };
    // SAFETY: as above.
    unsafe { (*hp).others = (hp as usize + 64) as *mut ArenaCaches };
    run("node on home", |e| {
        matches!(e, Broken::NodeOnHome { class: 0, .. })
    });
    // SAFETY: as above.
    unsafe { (*hp).others = saved };
    restored("node on home");

    // A node outside the metadata region (the checker reads no field of
    // it).
    let outside = 0u64;
    // SAFETY: as above.
    unsafe { (*hp).others = &outside as *const u64 as *mut ArenaCaches };
    run("node outside meta", |e| {
        matches!(e, Broken::NodeOutsideMeta { .. })
    });
    // SAFETY: as above.
    unsafe { (*hp).others = saved };
    restored("node outside meta");

    // A node whose block, `node - 64 * pad`, starts below the metadata
    // region: the only node, at the region's base, with `pad` 1. (A node
    // is 64-aligned, as `ArenaCaches` is, so its block is too: placement
    // is the one way to misplace it.) Its `arena`, `pad` and `next` are
    // the region's first bytes, put back after; the checker stops at the
    // block, before it reads the node's headers.
    let low = s.regions.meta_base as *mut ArenaCaches;
    assert!(
        hp as usize >= low as usize + core::mem::size_of::<ArenaCaches>(),
        "the heap's home headers are clear of the node's"
    );
    // SAFETY: the start of the metadata region, committed (the object side
    // grows from the base) and read or written by nothing else until it is
    // put back: the test allocates nothing from `s` in between. Fields are
    // read and written in place, and the checker reads it as a node.
    let low_saved = unsafe { ((*low).arena, (*low).pad, (*low).next) };
    // SAFETY: as above.
    unsafe { ((*low).arena, (*low).pad, (*low).next) = (a1, 1, core::ptr::null_mut()) };
    // SAFETY: as above (the heap).
    unsafe { (*hp).others = low };
    run("block outside meta", |e| {
        matches!(e, Broken::BlockMisplaced { .. })
    });
    // SAFETY: as above.
    unsafe { (*hp).others = saved };
    // SAFETY: as above (the region's start).
    unsafe { ((*low).arena, (*low).pad, (*low).next) = low_saved };
    restored("block outside meta");

    // Home headers outside the metadata region: a copy of the heap on
    // the stack, checked at its own address (its nodes and stacks are the
    // heap's). The copy is never dropped or used as a heap.
    // SAFETY: as above; a bitwise copy of the live heap, read in place.
    let copy = core::mem::ManuallyDrop::new(unsafe { core::ptr::read(hp) });
    let at: *const Heap = &*copy;
    // SAFETY: the copy is live for the call and nothing writes it; the
    // nodes and stacks it names are the heap's, with no reference live.
    let got = unsafe { check_both(&copy, at, s) };
    assert!(
        matches!(got, Err(Broken::HeaderOutsideMeta { .. })),
        "mutant `heap outside meta`: got = {got:?}"
    );
    restored("heap outside meta");

    // Two nodes sharing headers: the second node one line after the first.
    // SAFETY: as above.
    let saved = unsafe { (*n0).next };
    // SAFETY: as above.
    unsafe { (*n0).next = (n0 as usize + 64) as *mut ArenaCaches };
    run("header twice", |e| matches!(e, Broken::HeaderTwice { .. }));
    // SAFETY: as above.
    unsafe { (*n0).next = saved };
    restored("header twice");

    // `rehome` on an uncounted heap.
    // SAFETY: as above.
    unsafe { (*hp).rehome = true };
    run("rehome", |e| matches!(e, Broken::RehomeUnbound));
    // SAFETY: as above.
    unsafe { (*hp).rehome = false };
    restored("rehome");

    // `bound` on a heap its arena does not count: an explicit cache homed
    // on `a1`, where no thread heap lives.
    let u = tcache_create(s, a1);
    let hu = s.tcaches.entry(u);
    assert_eq!(s.arenas.bound(a1), 0);
    // SAFETY: the cache this test just created, written in place.
    unsafe { (*hu).bound = true };
    // SAFETY: as above; `check`'s `&Heap` lasts for the call.
    let got = unsafe { check_both(&*hu, hu, s) };
    assert_eq!(
        got,
        Err(Broken::BoundUncounted { arena: a1 }),
        "mutant `bound`"
    );
    // SAFETY: as above.
    unsafe { (*hu).bound = false };
    assert_tcache_ok(s, u, "bound");
}
