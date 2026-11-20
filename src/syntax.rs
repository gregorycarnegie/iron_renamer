//! Byte-level kernels behind rule syntax and export, formally verified with
//! Verus: numbers in rule arguments parse to exactly their decimal value or
//! fail (never wrap), and CSV quoting triggers on every byte that would
//! otherwise split a field or a row.
//!
//! Plain `cargo build` erases the specs and proofs. To check them:
//!   verus --crate-type=lib src/syntax.rs
//! Keep this file self-contained (no `crate::` imports) so it stays a valid crate root.

#![allow(clippy::len_zero, clippy::manual_range_contains)]

use vstd::prelude::*;
use vstd::slice::slice_subrange;

verus! {

pub open spec fn is_digit(c: u8) -> bool {
    b'0' <= c && c <= b'9'
}

pub open spec fn all_digits(s: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < s.len() ==> is_digit(#[trigger] s[i])
}

/// Decimal value of a digit string: "" = 0, "042" = 42.
pub open spec fn dec_value(s: Seq<u8>) -> int
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        dec_value(s.drop_last()) * 10 + (s.last() - b'0')
    }
}

proof fn lemma_value_nonneg(s: Seq<u8>)
    requires
        all_digits(s),
    ensures
        dec_value(s) >= 0,
    decreases s.len(),
{
    if s.len() > 0 {
        let p = s.drop_last();
        assert(all_digits(p)) by {
            assert forall|i: int| 0 <= i < p.len() implies is_digit(#[trigger] p[i]) by {
                assert(p[i] == s[i]);
            }
        }
        assert(is_digit(s[s.len() - 1]));
        lemma_value_nonneg(p);
    }
}

/// Appending digits never lowers the value (a digit is at least 0 and the
/// prefix is non-negative).
proof fn lemma_prefix_value_le(s: Seq<u8>, k: int)
    requires
        all_digits(s),
        0 <= k <= s.len(),
    ensures
        dec_value(s.subrange(0, k)) <= dec_value(s),
    decreases s.len() - k,
{
    if k < s.len() {
        let p = s.subrange(0, k);
        let q = s.subrange(0, k + 1);
        assert(q.drop_last() =~= p);
        assert(is_digit(s[k]));
        assert(all_digits(p)) by {
            assert forall|i: int| 0 <= i < p.len() implies is_digit(#[trigger] p[i]) by {
                assert(p[i] == s[i]);
            }
        }
        lemma_value_nonneg(p);
        lemma_prefix_value_le(s, k + 1);
    } else {
        assert(s.subrange(0, k) =~= s);
    }
}

/// If the first i+1 digits already exceed u64, so does the whole string
/// (or it contains a non-digit further on).
proof fn overflow_means_too_big(s: Seq<u8>, i: int)
    requires
        0 <= i < s.len(),
        all_digits(s.subrange(0, i + 1)),
        dec_value(s.subrange(0, i)) * 10 + (s[i] - b'0') > u64::MAX,
        s.subrange(0, i + 1).drop_last() =~= s.subrange(0, i),
    ensures
        !all_digits(s) || dec_value(s) > u64::MAX,
{
    if all_digits(s) {
        assert(s.subrange(0, i + 1).last() == s[i]);
        lemma_prefix_value_le(s, i + 1);
    }
}

/// Unsigned decimal: one or more ASCII digits and nothing else (no sign, no
/// spaces). None for anything else, or a value that doesn't fit in u64.
pub fn parse_u64(s: &[u8]) -> (r: Option<u64>)
    ensures
        r matches Some(n) ==> s@.len() > 0 && all_digits(s@) && n == dec_value(s@),
        r is None ==> s@.len() == 0 || !all_digits(s@) || dec_value(s@) > u64::MAX,
{
    if s.len() == 0 {
        return None;
    }
    let mut n: u64 = 0;
    let mut i: usize = 0;
    while i < s.len()
        invariant
            0 <= i <= s@.len(),
            all_digits(s@.subrange(0, i as int)),
            n == dec_value(s@.subrange(0, i as int)),
        decreases s.len() - i,
    {
        let c = s[i];
        if c < b'0' || c > b'9' {
            assert(!is_digit(s@[i as int]));
            return None;
        }
        let d: u64 = (c - b'0') as u64;
        let ghost next = s@.subrange(0, i + 1);
        proof {
            assert(next.drop_last() =~= s@.subrange(0, i as int));
            assert(next.last() == c);
            assert(all_digits(next)) by {
                assert forall|j: int| 0 <= j < next.len() implies is_digit(#[trigger] next[j]) by {
                    if j < i {
                        assert(next[j] == s@.subrange(0, i as int)[j]);
                    }
                }
            }
        }
        let m = match n.checked_mul(10) {
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

pub open spec fn is_negative(s: Seq<u8>) -> bool {
    s.len() > 0 && s[0] == b'-'
}

/// The digits after an optional leading '-'.
pub open spec fn magnitude(s: Seq<u8>) -> Seq<u8> {
    if is_negative(s) { s.subrange(1, s.len() as int) } else { s }
}

pub open spec fn signed_value(s: Seq<u8>) -> int {
    if is_negative(s) { -dec_value(magnitude(s)) } else { dec_value(magnitude(s)) }
}

/// Signed decimal: an optional '-' then one or more digits. A leading '+' is
/// not accepted; rule syntax gives '+' its own meaning (renumber deltas).
/// None for anything else, or a value outside i64 (i64::MIN itself parses).
pub fn parse_i64(s: &[u8]) -> (r: Option<i64>)
    ensures
        r matches Some(n) ==> magnitude(s@).len() > 0 && all_digits(magnitude(s@))
            && n == signed_value(s@),
        r is None ==> magnitude(s@).len() == 0 || !all_digits(magnitude(s@))
            || signed_value(s@) < i64::MIN || signed_value(s@) > i64::MAX,
{
    let neg = s.len() > 0 && s[0] == b'-';
    let body = if neg { slice_subrange(s, 1, s.len()) } else { s };
    assert(body@ =~= magnitude(s@));
    let m = parse_u64(body)?;
    let v: i128 = if neg { 0i128 - m as i128 } else { m as i128 };
    if v < i64::MIN as i128 || v > i64::MAX as i128 {
        None
    } else {
        Some(v as i64)
    }
}

/// Bytes that end a CSV field or row unless the field is quoted.
pub open spec fn breaks_csv(c: u8) -> bool {
    c == b',' || c == b'"' || c == b'\n' || c == b'\r'
}

/// True exactly when some byte would break an unquoted CSV field.
pub fn csv_needs_quotes(s: &[u8]) -> (r: bool)
    ensures
        r == exists|i: int| 0 <= i < s@.len() && breaks_csv(#[trigger] s@[i]),
{
    let mut i: usize = 0;
    while i < s.len()
        invariant
            0 <= i <= s@.len(),
            forall|j: int| 0 <= j < i ==> !breaks_csv(#[trigger] s@[j]),
        decreases s.len() - i,
    {
        let c = s[i];
        if c == b',' || c == b'"' || c == b'\n' || c == b'\r' {
            assert(breaks_csv(s@[i as int]));
            return true;
        }
        i += 1;
    }
    false
}

} // verus!
