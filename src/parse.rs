//! Strict text → number. `/proc` and `/sys` only ever contain plain decimals, so anything
//! else (empty, words, hex, exponents, "inf", "NaN", commas) is a bug and becomes an error.

use crate::error::{Result, err};

/// `-?digits(.digits)?`
fn is_decimal(t: &str) -> bool {
    let b = t.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == int_start {
        return false;
    }
    if i == b.len() {
        return true;
    }
    if b[i] != b'.' {
        return false;
    }
    i += 1;
    let frac_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i > frac_start && i == b.len()
}

pub fn parse_f64(s: &str, what: &str) -> Result<f64> {
    let t = s.trim();
    if !is_decimal(t) {
        return Err(err(format!("bad number for {what}: {s:?}")));
    }
    let n: f64 = t
        .parse()
        .map_err(|_| err(format!("bad number for {what}: {s:?}")))?;
    if !n.is_finite() {
        return Err(err(format!("number too large for {what}: {s:?}")));
    }
    Ok(n)
}

/// Counters (bytes, ticks): exact up to 2^64, unlike a float.
pub fn parse_u64(s: &str, what: &str) -> Result<u64> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|c| c.is_ascii_digit()) {
        return Err(err(format!("bad counter for {what}: {s:?}")));
    }
    t.parse()
        .map_err(|_| err(format!("counter too large for {what}: {s:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_decimals() {
        assert_eq!(parse_f64("0.29", "t").unwrap(), 0.29);
        assert_eq!(parse_f64(" 42\n", "t").unwrap(), 42.0);
        assert_eq!(parse_f64("-3", "t").unwrap(), -3.0);
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "", "  ", "abc", "0x1A", "1e3", "inf", "NaN", "1.", ".5", "1,000", "--1",
        ] {
            assert!(parse_f64(bad, "t").is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn counters() {
        assert_eq!(parse_u64("18446744073709551615", "t").unwrap(), u64::MAX);
        assert!(parse_u64("18446744073709551616", "t").is_err());
        assert!(parse_u64("-1", "t").is_err());
        assert!(parse_u64("1.5", "t").is_err());
    }
}
