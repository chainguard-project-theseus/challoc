; Saved `define` lines for the self-test of ci/heap-noalias.sh, from
; release IR (optimized, and as rustc emits it before LLVM passes) and
; made up where marked. Each is preceded by what the scan must make of
; it: `flagged` (a Heap method whose self is noalias and not readonly, a
; `&mut Heap`), `readonly` (a noalias readonly self, a `&self`: listed,
; not failed), `ok` (a Heap method whose self is not noalias), `skipped`
; (a Heap method with no self parameter) or `unmatched` (not an inherent
; Heap method).
; none.ll: no inherent Heap method, so it must fail.
; Made up: another type.
; want: unmatched
define internal fastcc void @_RNvMNtCsiUoM9lG5jwN_7challoc5arenaNtB2_5Arena4init(ptr noalias nofree noundef nonnull align 128 dereferenceable(64) %self) unnamed_addr #10 {
; want: unmatched
define internal fastcc void @_RINvNtNtNtCslSNmEl5Cpmc_4core5slice4sort8unstable7ipnsortOhNCINvMB6_SBT_20sort_unstable_by_keyjNCNvMNtNtCsiUoM9lG5jwN_7challoc4heap5flushNtB1C_4Heap5flushs_0E0EB1E_(ptr noalias nofree noundef nonnull align 8 %v.0, i64 noundef range(i64 21, 5368709118) %v.1, ptr noalias nofree noundef nonnull align 8 dereferenceable(8) %is_less) unnamed_addr #3 personality ptr @rust_eh_personality !dbg !5682 {
; want: unmatched
define internal void @_RINvMNtCslSNmEl5Cpmc_4core5sliceSOh20sort_unstable_by_keyjNCNvMNtNtCseBiCXK8p50Z_7challoc4heap5flushNtB11_4Heap5flushs_0EB13_(ptr noalias nofree noundef nonnull align 8 %self.0, i64 noundef range(i64 0, 1152921504606846976) %self.1, ptr noalias nofree noundef readonly align 8 captures(address, read_provenance) dereferenceable(8) %0) unnamed_addr #0 personality ptr @rust_eh_personality !dbg !8 {
