use std::cmp::Ordering;

/// Compare two `[epoch:]version[-release]` strings the way alpm/pacman
/// does (the "rpmvercmp" algorithm plus epoch/release handling).
pub fn vercmp(a: &str, b: &str) -> Ordering {
    let (epoch_a, ver_a, rel_a) = split_evr(a);
    let (epoch_b, ver_b, rel_b) = split_evr(b);

    match epoch_a.cmp(&epoch_b) {
        Ordering::Equal => {}
        other => return other,
    }

    match rpmvercmp(ver_a, ver_b) {
        Ordering::Equal => {}
        other => return other,
    }

    // If either side omits a release, pacman treats release as a non-factor.
    match (rel_a, rel_b) {
        (Some(ra), Some(rb)) => rpmvercmp(ra, rb),
        _ => Ordering::Equal,
    }
}

fn split_evr(s: &str) -> (u64, &str, Option<&str>) {
    let (epoch, rest) = match s.split_once(':') {
        Some((e, r)) => (e.parse().unwrap_or(0), r),
        None => (0, s),
    };
    match rest.rsplit_once('-') {
        Some((v, r)) => (epoch, v, Some(r)),
        None => (epoch, rest, None),
    }
}

/// The segment-wise alpha/numeric comparison at the heart of alpm/rpm
/// version comparison. This is a direct port of `rpmvercmp` from
/// libalpm's `lib/libalpm/version.c` (verified against pacman 7.1.0's
/// `vercmp` binary and the upstream source) — it's not a re-derivation
/// from the general "numeric beats alpha" idea, because that idea alone
/// gets two real cases backwards:
///   - `1.0a` < `1.0` (alpha glued directly onto the last segment is a
///     pre-release marker: older)
///   - `1.0.rc1` > `1.0` (an alpha run reached only *after* consuming a
///     fresh separator counts as an extra component: newer)
///
/// The difference hinges on whether the exhausted side is checked for
/// emptiness before or after skipping separator characters, plus a
/// separate rule when the two sides skip different numbers of separator
/// characters in the same round.
fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut i = 0;
    let mut j = 0;

    while i < a.len() && j < b.len() {
        let round_start_i = i;
        let round_start_j = j;

        while i < a.len() && !a[i].is_ascii_alphanumeric() {
            i += 1;
        }
        while j < b.len() && !b[j].is_ascii_alphanumeric() {
            j += 1;
        }

        // If either ran out while skipping separators, we're done: fall
        // through to the tail rule below using this pre-extraction state.
        if i >= a.len() || j >= b.len() {
            break;
        }

        // Differing separator run lengths decide it outright (e.g. the
        // `1.0.0a` vs `1.0.0.a` glued-vs-dotted alpha suffix case).
        let skip_i = i - round_start_i;
        let skip_j = j - round_start_j;
        if skip_i != skip_j {
            return if skip_i < skip_j {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }

        let a_start = i;
        let b_start = j;
        let is_num = a[i].is_ascii_digit();

        if is_num {
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        } else {
            while i < a.len() && a[i].is_ascii_alphabetic() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
        }

        // `b` produced an empty segment here: either it ran out, or its
        // next run is the other type. Numeric beats alpha either way.
        if b_start == j {
            return if is_num { Ordering::Greater } else { Ordering::Less };
        }

        let a_seg = &a[a_start..i];
        let b_seg = &b[b_start..j];

        if is_num {
            let a_trimmed = trim_leading_zeros(a_seg);
            let b_trimmed = trim_leading_zeros(b_seg);
            match a_trimmed.len().cmp(&b_trimmed.len()) {
                Ordering::Equal => {}
                other => return other,
            }
            match a_trimmed.cmp(b_trimmed) {
                Ordering::Equal => continue,
                other => return other,
            }
        } else {
            match a_seg.cmp(b_seg) {
                Ordering::Equal => continue,
                other => return other,
            }
        }
    }

    let one_empty = i >= a.len();
    let two_empty = j >= b.len();
    if one_empty && two_empty {
        return Ordering::Equal;
    }

    // "We never want a remaining alpha string to beat an empty string":
    // - if `a` is exhausted and `b`'s next char isn't alpha, `b` is newer.
    // - if `a`'s next char is alpha, `b` is newer.
    // - otherwise `a` is newer.
    let one_isalpha = !one_empty && a[i].is_ascii_alphabetic();
    let two_isalpha = !two_empty && b[j].is_ascii_alphabetic();
    if (one_empty && !two_isalpha) || one_isalpha {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn trim_leading_zeros(seg: &[u8]) -> &[u8] {
    let mut k = 0;
    while k < seg.len() - 1 && seg[k] == b'0' {
        k += 1;
    }
    &seg[k..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ord(n: i32) -> Ordering {
        match n {
            n if n < 0 => Ordering::Less,
            0 => Ordering::Equal,
            _ => Ordering::Greater,
        }
    }

    /// Each pair here was cross-checked against the real `vercmp` binary
    /// (pacman 7.1.0) on the host system.
    #[test]
    fn matches_real_vercmp() {
        let cases: &[(&str, &str, i32)] = &[
            ("1.0-1", "1.0-2", -1),
            ("1.0a", "1.0", -1),
            ("1.0alpha", "1.0beta", -1),
            ("1:1.0-1", "2.0-1", 1),
            ("1.0.0", "1.0", 1),
            ("1.0", "1.0", 0),
            ("1.0.1", "1.1", -1),
            ("1.0-1", "1.0-1", 0),
            ("1", "1.0", -1),
            ("1.0", "1.0a", 1),
            ("2.0", "1.0", 1),
            ("1.0rc1", "1.0", -1),
            ("1.0-2", "1.0-10", -1),
            ("1.0.rc1", "1.0", 1),
            ("1.0+git1", "1.0", 1),
            ("1.0.0.a", "1.0.0", 1),
            ("1.0.0a", "1.0.0.a", -1),
            ("1.0-1.1", "1.0.rc1", -1),
            ("01.0", "1.0.rc1", -1),
        ];
        for (a, b, expected) in cases {
            assert_eq!(
                vercmp(a, b),
                ord(*expected),
                "vercmp({a:?}, {b:?}) expected {expected}"
            );
        }
    }

    #[test]
    fn epoch_dominates_version() {
        assert_eq!(vercmp("1:0.1", "0.99"), Ordering::Greater);
    }
}
