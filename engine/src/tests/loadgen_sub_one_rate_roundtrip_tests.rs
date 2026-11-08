use super::*;

// THE SUBPROCESS BOUNDARY IS WHERE THE FRACTIONAL RATE ALMOST DIED.
//
// `GenStats::rps` was changed to publish a fraction below 1/s, because truncating "one request in
// four seconds" to `0` says the gateway carried NOTHING. The child prints that with `{}`, so the
// wire carries `rps=0.25` - and this parser read `rps` as an i64, which FAILS. `parse_ugen_line`
// then classifies the entire window as a HarnessError, so the sub-1/s rungs the change was written
// for would have been DISCARDED instead of published: strictly worse than the false `0` it replaced.
//
// Nothing caught it. The engine's own unit tests all sit on one side of the boundary or the other,
// and the format string and the parser are in different files. This pins the seam.
#[test]
fn a_sub_one_per_second_rate_survives_the_stats_line_round_trip() {
    for (line, want) in [
        (
            "rps=0.25 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=1",
            0.25_f64,
        ),
        (
            "rps=0.5 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=2",
            0.5,
        ),
        (
            "rps=19 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=76",
            19.0,
        ),
        (
            "rps=44363 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=177452",
            44_363.0,
        ),
    ] {
        let got = parse_ugen_line(line)
            .into_value()
            .unwrap_or_else(|| panic!("a well-formed stats line must parse: {line}"));
        assert!(
            (got.rps - want).abs() < 1e-9,
            "rps round-tripped as {} from {line}, wanted {want}",
            got.rps
        );
    }
}

// The rate is the ONLY field allowed to be fractional. A fractional COUNT is a corrupt line and
// must still be refused - relaxing every field to f64 would have hidden that.
#[test]
fn a_fractional_count_is_still_refused() {
    assert!(
        parse_ugen_line("rps=10 fail=0.5 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=40")
            .into_value()
            .is_none(),
        "a fractional failure COUNT is a corrupt line, not a measurement"
    );
}

// A non-finite rate must not reach a cast that would SATURATE it into a plausible-looking integer.
#[test]
fn a_non_finite_rate_is_refused_rather_than_saturated() {
    for bad in ["rps=inf", "rps=NaN"] {
        let line = format!("{bad} fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=1");
        assert!(
            parse_ugen_line(&line).into_value().is_none(),
            "a non-finite rate must be refused: {line}"
        );
    }
}
