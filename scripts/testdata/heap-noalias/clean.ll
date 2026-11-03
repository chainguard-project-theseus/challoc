; Saved `define` lines for the self-test of ci/heap-noalias.sh, from
; release IR (optimized, and as rustc emits it before LLVM passes) and
; made up where marked. Each is preceded by what the scan must make of
; it: `flagged` (a Heap method whose self is noalias and not readonly, a
; `&mut Heap`), `readonly` (a noalias readonly self, a `&self`: listed,
; not failed), `ok` (a Heap method whose self is not noalias), `skipped`
; (a Heap method with no self parameter) or `unmatched` (not an inherent
; Heap method).
; clean.ll: must pass.
; want: ok
define internal fastcc noundef range(i32 0, -2147483648) i32 @_RNvMNtNtCsiUoM9lG5jwN_7challoc4heap5flushNtB4_4Heap6refill(ptr noundef nonnull align 64 %self, ptr noundef nonnull align 128 %shared, ptr noundef nonnull align 128 %arena, ptr noundef nonnull captures(address, read_provenance) %cache, i64 noundef range(i64 0, 1024) %class) unnamed_addr #0 personality ptr @rust_eh_personality !dbg !11556 {
; want: ok
define internal noundef i32 @_RNvMNtNtCseBiCXK8p50Z_7challoc4heap5flushNtB4_4Heap6refill(ptr noundef nonnull align 64 %self, ptr noundef nonnull align 128 %shared, ptr noundef nonnull align 128 %arena, ptr noundef %cache, i64 noundef %class) unnamed_addr #1 !dbg !16838 {
; Made up: self after a noalias parameter.
; want: ok
define internal fastcc void @_RNvMNtNtCsiUoM9lG5jwN_7challoc4heap5pathsNtB4_4Heap10flush_into(ptr noalias nofree noundef nonnull align 8 dereferenceable(16) %out, ptr noundef nonnull align 64 %self) unnamed_addr #10 {
; want: readonly
define internal { i64, i64 } @_RNvMsj_NtCseBiCXK8p50Z_7challoc4heapNtB5_4Heap5stats(ptr noalias nofree noundef readonly align 64 captures(address, read_provenance) dereferenceable(3456) %self) unnamed_addr #1 !dbg !33784 {
; Made up: no self.
; want: skipped
define internal fastcc noundef i64 @_RNvMNtNtCsiUoM9lG5jwN_7challoc4heap5flushNtB4_4Heap8capacity(i64 noundef range(i64 0, 1024) %class) unnamed_addr #10 {
; Made up: another type.
; want: unmatched
define internal fastcc void @_RNvMNtCsiUoM9lG5jwN_7challoc5arenaNtB2_5Arena4init(ptr noalias nofree noundef nonnull align 128 dereferenceable(64) %self) unnamed_addr #10 {
; want: unmatched
define internal void @_RINvMNtCslSNmEl5Cpmc_4core5sliceSOh20sort_unstable_by_keyjNCNvMNtNtCseBiCXK8p50Z_7challoc4heap5flushNtB11_4Heap5flushs_0EB13_(ptr noalias nofree noundef nonnull align 8 %self.0, i64 noundef range(i64 0, 1152921504606846976) %self.1, ptr noalias nofree noundef readonly align 8 captures(address, read_provenance) dereferenceable(8) %0) unnamed_addr #0 personality ptr @rust_eh_personality !dbg !8 {
