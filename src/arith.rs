//! Integer kernels behind counters, ranges and random tags, formally
//! verified with Verus: no overflow, and results match the specs.
//!
//! Plain `cargo build` erases the specs and proofs. To check them:
//!   verus --crate-type=lib src/arith.rs
//! Keep this file self-contained (no `crate::` imports) so it stays a valid crate root.

// Plain comparisons keep the exec code close to the specs Verus reasons about.
#![allow(clippy::len_zero, clippy::manual_range_contains)]

use vstd::prelude::*;

verus! {

pub open spec fn is_letter(c: u8) -> bool {
    (b'a' <= c && c <= b'z') || (b'A' <= c && c <= b'Z')
}

/// a/A = 1 ... z/Z = 26.
pub open spec fn letter_digit(c: u8) -> int {
    if b'a' <= c && c <= b'z' { c - b'a' + 1 } else { c - b'A' + 1 }
}

/// Bijective base-26 value of a letter string: "a" = 1, "z" = 26, "aa" = 27.
pub open spec fn letters_value(s: Seq<u8>) -> int
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        letters_value(s.drop_last()) * 26 + letter_digit(s.last())
    }
}

pub open spec fn all_letters(s: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < s.len() ==> is_letter(#[trigger] s[i])
}

/// Every digit is at least 1, so appending letters never lowers the value.
proof fn lemma_prefix_value_le(s: Seq<u8>, k: int)
    requires
        all_letters(s),
        0 <= k <= s.len(),
    ensures
        letters_value(s.subrange(0, k)) <= letters_value(s),
    decreases s.len() - k,
{
    if k < s.len() {
        let p = s.subrange(0, k);
        let q = s.subrange(0, k + 1);
        assert(q.drop_last() =~= p);
        assert(is_letter(s[k]));
        lemma_value_nonneg(p);
        lemma_prefix_value_le(s, k + 1);
    } else {
        assert(s.subrange(0, k) =~= s);
    }
}

proof fn lemma_value_nonneg(s: Seq<u8>)
    requires
        all_letters(s),
    ensures
        letters_value(s) >= 0,
    decreases s.len(),
{
    if s.len() > 0 {
        let p = s.drop_last();
        assert(all_letters(p)) by {
            assert forall|i: int| 0 <= i < p.len() implies is_letter(#[trigger] p[i]) by {
                assert(p[i] == s[i]);
            }
        }
        assert(is_letter(s[s.len() - 1]));
        lemma_value_nonneg(p);
    }
}

/// Parses a letter START for <alpha> ("aa" = 27), case-insensitively.
/// None for empty input, a non-letter, or a value that doesn't fit in i64.
pub fn alpha_to_num(s: &[u8]) -> (r: Option<i64>)
    ensures
        r matches Some(n) ==> s@.len() > 0 && all_letters(s@) && n == letters_value(s@),
        r is None ==> s@.len() == 0 || !all_letters(s@) || letters_value(s@) > i64::MAX,
{
    if s.len() == 0 {
        return None;
    }
    let mut n: i64 = 0;
    let mut i: usize = 0;
    while i < s.len()
        invariant
            0 <= i <= s@.len(),
            all_letters(s@.subrange(0, i as int)),
            n == letters_value(s@.subrange(0, i as int)),
            n >= 0,
        decreases s.len() - i,
    {
        let c = s[i];
        let d: i64 = if b'a' <= c && c <= b'z' {
            (c - b'a' + 1) as i64
        } else if b'A' <= c && c <= b'Z' {
            (c - b'A' + 1) as i64
        } else {
            assert(!is_letter(s@[i as int]));
            return None;
        };
        assert(d == letter_digit(s@[i as int]) && d >= 1);
        let ghost next = s@.subrange(0, i + 1);
        proof {
            assert(next.drop_last() =~= s@.subrange(0, i as int));
            assert(all_letters(next)) by {
                assert forall|j: int| 0 <= j < next.len() implies is_letter(#[trigger] next[j]) by {
                    if j < i {
                        assert(next[j] == s@.subrange(0, i as int)[j]);
                    }
                }
            }
        }
        let m = match n.checked_mul(26) {
            Some(m) => m,
            None => {
                proof { overflow_means_too_big(s@, i as int); }
                return None;
            },
        };
        n = match m.checked_add(d) {
            Some(v) => v,
            None => {
                proof { overflow_means_too_big(s@, i as int); }
                return None;
            },
        };
        i += 1;
    }
    assert(s@.subrange(0, s@.len() as int) =~= s@);
    Some(n)
}

/// If the value of the first i+1 letters exceeds i64, so does the whole string
/// (or it contains a non-letter further on).
proof fn overflow_means_too_big(s: Seq<u8>, i: int)
    requires
        0 <= i < s.len(),
        all_letters(s.subrange(0, i + 1)),
        letters_value(s.subrange(0, i) ) * 26 + letter_digit(s[i]) > i64::MAX,
        s.subrange(0, i + 1).drop_last() =~= s.subrange(0, i),
    ensures
        !all_letters(s) || letters_value(s) > i64::MAX,
{
    if all_letters(s) {
        assert(s.subrange(0, i + 1).last() == s[i]);
        lemma_prefix_value_le(s, i + 1);
    }
}

/// START + STEP*i, clamped below at 0 like before. Computed in i128, where it
/// cannot overflow; None only if the result is too large for i64.
pub fn counter_value(start: i64, step: i64, i: usize) -> (r: Option<i64>)
    ensures
        r == (if start + step * i > i64::MAX {
            None::<i64>
        } else if start + step * i < 0 {
            Some(0i64)
        } else {
            Some((start + step * i) as i64)
        }),
{
    let st = step as i128;
    let ix = i as i128;
    proof {
        assert(-0x8000_0000_0000_0000 <= st <= 0x7fff_ffff_ffff_ffff);
        assert(0 <= ix <= 0xffff_ffff_ffff_ffff);
        assert(-0x7fff_ffff_ffff_ffff_8000_0000_0000_0000 <= st * ix
            <= 0x7fff_ffff_ffff_fffe_8000_0000_0000_0001) by (nonlinear_arith)
            requires
                -0x8000_0000_0000_0000 <= st <= 0x7fff_ffff_ffff_ffff,
                0 <= ix <= 0xffff_ffff_ffff_ffff;
    }
    let v = start as i128 + st * ix;
    if v > i64::MAX as i128 {
        None
    } else if v < 0 {
        Some(0)
    } else {
        Some(v as i64)
    }
}

/// Char range [from, to) for START,LEN over a string of `n` chars, both ends
/// clamped to the string. A START past the end gives the empty range (n, n).
pub fn char_window(start: usize, len: usize, n: usize) -> (r: (usize, usize))
    ensures
        r.0 == (if start < n { start } else { n }),
        r.1 == (if start + len < n { (start + len) as usize } else { n }),
        r.0 <= r.1 <= n,
{
    let from = if start < n { start } else { n };
    let to = if len < n - from { from + len } else { n };
    (from, to)
}

/// Uniform-ish pick in [min, max] from a raw random word; None if max < min.
/// The span can be the full 2^64 range, so it is computed in u128.
pub fn rand_in(min: i64, max: i64, r: u64) -> (v: Option<i64>)
    ensures
        max < min ==> v is None,
        min <= max ==> v == Some((min + (r as int) % (max - min + 1)) as i64),
        v matches Some(x) ==> min <= x <= max,
{
    if max < min {
        return None;
    }
    let span = (max as i128 - min as i128 + 1) as u128;
    let off = (r as u128) % span;
    assert(off < span);
    Some((min as i128 + off as i128) as i64)
}

/// One xorshift64 step (Marsaglia); 13/7/17 is the published shift triple.
/// Each step is invertible, so a nonzero state never becomes 0.
pub fn xorshift64(x: u64) -> (r: u64)
    ensures
        x != 0 ==> r != 0,
{
    let a = x ^ (x << 13u64);
    let b = a ^ (a >> 7u64);
    let c = b ^ (b << 17u64);
    proof {
        if x != 0 {
            assert((x ^ (x << 13u64)) != 0) by (bit_vector)
                requires
                    x != 0,
            ;
            assert((a ^ (a >> 7u64)) != 0) by (bit_vector)
                requires
                    a != 0,
            ;
            assert((b ^ (b << 17u64)) != 0) by (bit_vector)
                requires
                    b != 0,
            ;
        }
    }
    c
}

/// Seed from a clock reading. xorshift64 maps 0 to 0, so the seed must not be.
pub fn seed_from(nanos: u64) -> (r: u64)
    ensures
        r != 0,
{
    assert((nanos | 1u64) != 0) by (bit_vector);
    nanos | 1
}

} // verus!
