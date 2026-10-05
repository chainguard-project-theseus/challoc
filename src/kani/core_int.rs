//! The integer and byte methods of `core` that `vspec/core_int.rs`,
//! `vspec/config.rs` and `vspec/sys.rs` specify, each checked against
//! its `assume_specification` for every input (no bound: the inputs are
//! machine integers, and every loop below runs at most 65 times). The
//! `spec_*` functions restate the Verus spec functions the contracts
//! name, in the same recursion, over `u128` where the spec's `int`
//! arithmetic could leave 64 bits. `usize::div_ceil` (`vspec/core_int.rs`)
//! and `u64::div_ceil` (`vspec/policy.rs`) have no harness: with symbolic
//! 64-bit operands CBMC did not finish in hours (it has to show two
//! 64-bit dividers equal), so those seams stay trusted.

/// vstd's `u64_trailing_zeros`: 64 for 0, else the count of low zero bits,
/// by its recursion on `i / 2`.
fn spec_u64_trailing_zeros(mut i: u64) -> u32 {
    if i == 0 {
        return 64;
    }
    let mut r = 0;
    while i & 1 == 0 {
        r += 1;
        i /= 2;
    }
    r
}

/// vstd's `u64_leading_zeros`: 64 for 0, else one less than that of
/// `i / 2`.
fn spec_u64_leading_zeros(mut i: u64) -> i64 {
    let mut r = 64;
    while i != 0 {
        r -= 1;
        i /= 2;
    }
    r
}

/// `vspec::core_int::popcount`: `x % 2` plus that of `x / 2`.
fn spec_popcount(mut x: u64) -> u32 {
    let mut r = 0;
    while x != 0 {
        r += (x % 2) as u32;
        x /= 2;
    }
    r
}

/// `exists|k: nat| k < bits && x == pow2(k)`.
fn spec_is_pow2_below(x: u128, bits: u32) -> bool {
    let mut k = 0;
    while k < bits {
        if x == 1u128 << k {
            return true;
        }
        k += 1;
    }
    false
}

/// `vspec::core_int::rotr32`.
fn spec_rotr32(x: u32, n: u32) -> u32 {
    if n % 32 == 0 {
        x
    } else {
        (x >> (n % 32)) | (x << (32 - n % 32))
    }
}

/// `config::to_digit_spec`.
fn spec_to_digit(c: u32, radix: u32) -> Option<u32> {
    let d = if 48 <= c && c <= 57 {
        c - 48
    } else if 97 <= c && c <= 122 {
        c - 97 + 10
    } else if 65 <= c && c <= 90 {
        c - 65 + 10
    } else {
        36
    };
    if d < radix { Some(d) } else { None }
}

/// S282 `usize::trailing_zeros`: `r == u64_trailing_zeros(x as u64)`.
#[kani::proof]
#[kani::unwind(66)]
fn usize_trailing_zeros() {
    let x: usize = kani::any();
    assert_eq!(x.trailing_zeros(), spec_u64_trailing_zeros(x as u64));
}

/// S283 `usize::leading_zeros`: `r as int == u64_leading_zeros(x as u64)`.
#[kani::proof]
#[kani::unwind(66)]
fn usize_leading_zeros() {
    let x: usize = kani::any();
    assert_eq!(x.leading_zeros() as i64, spec_u64_leading_zeros(x as u64));
}

/// S284 `u64::count_ones`: `r as nat == popcount(x)`.
#[kani::proof]
#[kani::unwind(66)]
fn u64_count_ones() {
    let x: u64 = kani::any();
    assert_eq!(x.count_ones(), spec_popcount(x));
}

/// S285 `usize::count_ones`: `r as nat == popcount(x as u64)`.
#[kani::proof]
#[kani::unwind(66)]
fn usize_count_ones() {
    let x: usize = kani::any();
    assert_eq!(x.count_ones(), spec_popcount(x as u64));
}

/// S286 `usize::is_power_of_two`: `r <==> exists k < 64, x == pow2(k)`.
#[kani::proof]
#[kani::unwind(66)]
fn usize_is_power_of_two() {
    let x: usize = kani::any();
    assert_eq!(x.is_power_of_two(), spec_is_pow2_below(x as u128, 64));
}

/// S287 `u32::is_power_of_two`: `r <==> exists k < 32, x == pow2(k)`.
#[kani::proof]
#[kani::unwind(34)]
fn u32_is_power_of_two() {
    let x: u32 = kani::any();
    assert_eq!(x.is_power_of_two(), spec_is_pow2_below(x as u128, 32));
}

/// S364 `u64::is_power_of_two`: `r <==> exists k < 64, x == pow2(k)`.
#[kani::proof]
#[kani::unwind(66)]
fn u64_is_power_of_two() {
    let x: u64 = kani::any();
    assert_eq!(x.is_power_of_two(), spec_is_pow2_below(x as u128, 64));
}

/// S288 `usize::next_power_of_two`: under `x <= 2^63`, `r` is a power of
/// two below `2^64`, `x <= r`, `r == 1` for `x <= 1` and `r < 2 * x`
/// otherwise.
#[kani::proof]
#[kani::unwind(66)]
fn usize_next_power_of_two() {
    let x: usize = kani::any();
    kani::assume(x <= 0x8000_0000_0000_0000);
    let r = x.next_power_of_two();
    assert!(spec_is_pow2_below(r as u128, 64));
    assert!(x <= r);
    if x <= 1 {
        assert_eq!(r, 1);
    } else {
        assert!((r as u128) < 2 * x as u128);
    }
}

/// S290 `u32::rotate_right`: `r == rotr32(x, n)`, for every `n` (also
/// `n >= 32`).
#[kani::proof]
fn u32_rotate_right() {
    let x: u32 = kani::any();
    let n: u32 = kani::any();
    assert_eq!(x.rotate_right(n), spec_rotr32(x, n));
}

/// S280 `i64::wrapping_neg`: `-x`, and `i64::MIN` for `i64::MIN`.
#[kani::proof]
fn i64_wrapping_neg() {
    let x: i64 = kani::any();
    let r = x.wrapping_neg();
    if x == i64::MIN {
        assert_eq!(r, i64::MIN);
    } else {
        assert_eq!(r as i128, -(x as i128));
    }
}

/// S358 `isize::wrapping_neg`: `-x`, and `isize::MIN` for `isize::MIN`.
#[kani::proof]
fn isize_wrapping_neg() {
    let x: isize = kani::any();
    let r = x.wrapping_neg();
    if x == isize::MIN {
        assert_eq!(r, isize::MIN);
    } else {
        assert_eq!(r as i128, -(x as i128));
    }
}

/// S281 `usize::ilog2`: under `x > 0`, `r < 64` and
/// `pow2(r) <= x < pow2(r + 1)`.
#[kani::proof]
fn usize_ilog2() {
    let x: usize = kani::any();
    kani::assume(x > 0);
    let r = x.ilog2();
    assert!(r < 64);
    assert!(1u128 << r <= x as u128);
    assert!((x as u128) < 1u128 << (r + 1));
}

/// S279 `char::to_digit`: under `2 <= radix <= 36`,
/// `r == to_digit_spec(c as u32, radix)`, for every `char`.
#[kani::proof]
fn char_to_digit() {
    let c: char = kani::any();
    let radix: u32 = kani::any();
    kani::assume(2 <= radix && radix <= 36);
    assert_eq!(c.to_digit(radix), spec_to_digit(c as u32, radix));
}

/// S355 `u8::is_ascii_digit`: `r <==> 48 <= b <= 57`.
#[kani::proof]
fn u8_is_ascii_digit() {
    let b: u8 = kani::any();
    assert_eq!(b.is_ascii_digit(), 48 <= b && b <= 57);
}

/// S356 `u8::is_ascii_whitespace`: space, tab, line feed, form feed and
/// carriage return.
#[kani::proof]
fn u8_is_ascii_whitespace() {
    let b: u8 = kani::any();
    assert_eq!(
        b.is_ascii_whitespace(),
        b == 32 || b == 9 || b == 10 || b == 12 || b == 13
    );
}
