; Saved `define` lines for the self-test of ci/heap-noalias.sh, from
; release IR (optimized, and as rustc emits it before LLVM passes) and
; made up where marked. Each is preceded by what the scan must make of
; it: `flagged` (a Heap method whose self is noalias and not readonly, a
; `&mut Heap`), `readonly` (a noalias readonly self, a `&self`: listed,
; not failed), `ok` (a Heap method whose self is not noalias), `skipped`
; (a Heap method with no self parameter) or `unmatched` (not an inherent
; Heap method).
; ro-only.ll: only a `&self` receiver, so it must fail.
; want: readonly
define internal { i64, i64 } @_RNvMsj_NtCseBiCXK8p50Z_7challoc4heapNtB5_4Heap5stats(ptr noalias nofree noundef readonly align 64 captures(address, read_provenance) dereferenceable(3456) %self) unnamed_addr #1 !dbg !33784 {
