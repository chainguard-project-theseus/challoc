// Cases for scripts/kani-boolcmp.sh (ci/kani-boolcmp-test.sh): every line
// marked `want: hit` must be flagged, and no other line.

// Outside Kani code only a function that orders its own bool parameter
// counts; its locals and other comparisons do not.
fn param_order(a: bool, b: (u32, bool)) -> bool {
    let c = b.0 < 7;
    a > c // want: hit
}

fn plain(x: u32, y: u32) -> bool {
    let z = true;
    x < y && z
}

#[cfg(kani)]
mod kani_cases {
    #[derive(PartialEq, PartialOrd)]
    struct Flags {
        n: u32,
        on: bool, // want: hit
    }

    #[derive(PartialEq, PartialOrd)]
    struct Counts {
        n: u32,
        m: u64,
    }

    #[kani::proof]
    fn bad() {
        let x: bool = kani::any();
        let y = kani::any::<bool>();
        let a: (u32, bool) = (kani::any(), kani::any());
        let b = kani::any::<(u32, bool)>();
        assert!(x < y || x >= y); // want: hit
        kani::assume(a <= b); // want: hit
        kani::cover!((a.0, x) > (b.0, y)); // want: hit
        assert!(a.1 >= b.1); // want: hit
        assert!(x.lt(&y)); // want: hit
        let f = |p: bool, q: bool| p < q; // want: hit
        for (u, v) in [(false, true), (true, false)] {
            assert!(u > v || !u); // want: hit
        }
    }

    #[kani::proof]
    fn ok() {
        let x: bool = kani::any();
        let y: bool = kani::any();
        let a: (u32, bool) = (kani::any(), kani::any());
        let b: (u32, bool) = (kani::any(), kani::any());
        let n: u32 = kani::any();
        let v: Vec<bool> = Vec::new();
        assert!(a.0 < b.0 || a.0 >= b.0);
        assert!(x == y || x != y);
        assert_eq!(x.cmp(&y), (x as u8).cmp(&(y as u8)));
        assert!((x as u8) < (y as u8) || !x & y | (x & !y) | (x == y));
        assert!(x < true || x >= false);
        assert!(n << 1 >= n || n >> 1 <= n);
        assert!(key(a) < key(b));
        assert!(Some(x).is_some() && v.len() < 3);
        let s = "x < y";
        let c = '<';
        // x < y in a comment
        // kani-boolcmp: allow (a reviewed exception)
        assert!(x < y || !x);
    }

    fn key(k: (u32, bool)) -> u64 {
        ((k.0 as u64) << 1) | k.1 as u64
    }

    fn arrows(t: bool) -> bool {
        match t {
            true => false,
            false => true,
        }
    }
}

fn by_ref(a: &bool, b: &(u32, bool)) -> bool {
    a < &b.1 // want: hit
}

fn sig_only(a: bool, b: bool) -> bool;

fn after_sig(a: u32, b: u32) -> bool {
    a < b
}

fn multi_line(
    a: (u32, bool),
    b: bool,
) -> bool {
    b > a.1 // want: hit
}

#[cfg(any(test, kani))]
mod kani_any {
    #[derive(PartialEq, PartialOrd)]
    struct Pair(u32, bool); // want: hit

    #[derive(PartialEq, PartialOrd)]
    struct Plain(u32, u64);

    #[derive(PartialEq, PartialOrd)]
    enum E {
        A(u32),
        B(bool), // want: hit
    }

    fn destructure() {
        let (x, y): (bool, bool) = kani::any();
        let (n, m): (u32, u32) = kani::any();
        assert!(x < y || n < m); // want: hit
    }
}

#[cfg(not(kani))]
mod not_kani {
    fn f() {
        let x: bool = true;
        let y: bool = false;
        let _ = x < y;
    }
}
