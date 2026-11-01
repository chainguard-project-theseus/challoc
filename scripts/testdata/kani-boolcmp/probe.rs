// A positive control for scripts/kani-boolcmp.sh: Kani probes of bool
// comparisons, written to be appended to src/arena.rs (they call its
// `key_less`). Run there in regular format with `--cbmc-args --verbosity 9`,
// `p1_bool_lt` makes CBMC 6.11 print `warning: ignoring >=`; the lint must
// flag its `x < y`. The others call `key_less` or compare integers, which
// the lint does not read.

#[cfg(kani)]
mod kani_probe {
    use super::key_less;

    fn key(k: (u32, bool)) -> u64 {
        ((k.0 as u64) << 1) | k.1 as u64
    }

    #[kani::proof]
    fn p1_bool_lt() {
        let x: bool = kani::any();
        let y: bool = kani::any();
        assert_eq!(x < y, !x && y);
    }

    #[kani::proof]
    fn p2_bool_cmp() {
        let x: bool = kani::any();
        let y: bool = kani::any();
        assert_eq!(x.cmp(&y), (x as u8).cmp(&(y as u8)));
    }

    #[kani::proof]
    fn p3_tuple_branch() {
        let a: (u32, bool) = (kani::any(), kani::any());
        let b: (u32, bool) = (kani::any(), kani::any());
        let want = key(a) < key(b);
        let r = key_less(a, b);
        assert!(if r { want } else { !want });
    }

    #[kani::proof]
    fn p4_tuple_eq() {
        let a: (u32, bool) = (kani::any(), kani::any());
        let b: (u32, bool) = (kani::any(), kani::any());
        let want = key(a) < key(b);
        assert_eq!(key_less(a, b), want);
    }

    #[kani::proof]
    fn p5_tuple_plain_assert() {
        let a: (u32, bool) = (kani::any(), kani::any());
        let b: (u32, bool) = (kani::any(), kani::any());
        let want = key(a) < key(b);
        assert!(key_less(a, b) == want);
    }

    #[kani::proof]
    fn p6_lexi_eq() {
        let a: (u32, bool) = (kani::any(), kani::any());
        let b: (u32, bool) = (kani::any(), kani::any());
        let want = a.0 < b.0 || (a.0 == b.0 && !a.1 && b.1);
        assert_eq!(key_less(a, b), want);
    }

    #[kani::proof]
    fn p7_key_only() {
        let a: (u32, bool) = (kani::any(), kani::any());
        let b: (u32, bool) = (kani::any(), kani::any());
        let want = a.0 < b.0 || (a.0 == b.0 && !a.1 && b.1);
        assert_eq!(key(a) < key(b), want);
    }

    #[kani::proof]
    fn p8_bool_assert_eq() {
        let x: bool = kani::any();
        let y: bool = !!x;
        assert_eq!(x, y);
    }
}
