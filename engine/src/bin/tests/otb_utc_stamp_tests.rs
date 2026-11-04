use super::{utc_stamp, utc_stamp_at};

// THE CASE THAT WAS BROKEN: a stamp with dashes where the time portion's colons belong parses
// as NaN in the site generator's Date.parse, so every gateway silently rendered measured_at as
// null. A real ISO-8601 shape is the whole contract this function exists to uphold.
#[test]
fn the_stamp_is_real_iso_8601_with_colons_in_the_time_portion() {
    let s = utc_stamp();
    assert!(
        regex_free_iso_shape(&s),
        "utc_stamp() produced {s:?}, which is not YYYY-MM-DDTHH:MM:SSZ"
    );
}

// No regex crate in this binary's dependencies - a tiny hand check is clearer than pulling one
// in for a single call site.
fn regex_free_iso_shape(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || b.is_ascii_digit())
}

// THE CALENDAR ARITHMETIC, against dates with known answers. The shape test above passes for a stamp
// that is well-formed and on the wrong day; these pin the civil-from-days constants, leap days and a
// century boundary included.
#[test]
fn known_instants_stamp_to_their_real_dates() {
    for (secs, want) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (1_709_164_800, "2024-02-29T00:00:00Z"),
        (1_762_218_123, "2025-11-04T01:02:03Z"),
        (4_102_444_799, "2099-12-31T23:59:59Z"),
    ] {
        assert_eq!(utc_stamp_at(secs), want, "for {secs}s since the epoch");
    }
}
