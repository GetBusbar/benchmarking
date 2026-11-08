use super::*;

fn rung(conc: i64, measured: bool) -> SweepPoint {
    // Generic: this fixture mixes Measurement<i64> (the counts) with Measurement<f64> (the rate),
    // and a closure would fix itself to whichever it saw first.
    fn gone<T>() -> Measurement<T> {
        Measurement::absent_because(Absent::NotMeasured, "no window was recorded".to_string())
    }
    SweepPoint {
        conc,
        ok: if measured {
            Measurement::Measured(10_000)
        } else {
            gone()
        },
        rps: if measured {
            Measurement::Measured(6209.0)
        } else {
            gone()
        },
        p99_us: if measured {
            Measurement::Measured(4200)
        } else {
            gone()
        },
        fail: if measured {
            Measurement::Measured(0)
        } else {
            gone()
        },
    }
}

/* A RUNG'S NULLS ARE PUBLISHED NULLS. sweep_max_proxy is a Vec, so absences_of! cannot reach the
Measurements inside it and nothing else walked it - a rung the generator returned no window
for published three bare nulls with its reasons nowhere. These are the very inputs
frontier::Rung::served_cleanly is published for, so a checker reading ok: null could not tell
"no requests completed" from "no window was recorded". */
#[test]
fn an_unmeasured_rung_states_a_reason_for_every_null_it_publishes() {
    let p = CellPerf {
        sweep_max_proxy: vec![rung(1024, false)],
        ..Default::default()
    };
    let a = p.absences();
    for f in ["ok", "rps", "p99_us", "fail"] {
        let k = format!("sweep.c1024.w1.{f}");
        assert!(
            a.contains_key(&k),
            "{k} published a bare null; keys were {:?}",
            a.keys()
        );
    }
}

/// Keyed by concurrency AND window ordinal, so repeated windows at one concurrency do not
/// collide - three windows per rung is the normal shape, and a collision would silently drop two
/// of every three reasons.
#[test]
fn repeated_windows_at_one_concurrency_get_distinct_keys() {
    let p = CellPerf {
        sweep_max_proxy: vec![rung(64, false), rung(64, false), rung(64, false)],
        ..Default::default()
    };
    let a = p.absences();
    for w in 1..=3 {
        assert!(
            a.contains_key(&format!("sweep.c64.w{w}.ok")),
            "window {w} lost its reason"
        );
    }
}

/// A fully measured rung needs no entry at all.
#[test]
fn a_measured_rung_publishes_no_absence() {
    let p = CellPerf {
        sweep_max_proxy: vec![rung(64, true)],
        ..Default::default()
    };
    assert!(p.absences().keys().all(|k| !k.starts_with("sweep.")));
}
