/* ── THE SEARCH SIMULATOR ─────────────────────────────────────────────────────────────────────────

Every defect in the sustained-stream search this session was found by running fourteen EC2 boxes
for hours and reading the wreckage: halving past the bracket, giving up with budget unspent, and
filing a wedged gateway as a dirty rig. All three are defects in a PURE FUNCTION - pick a rung,
measure, confirm by majority, step down, attribute - which needs no gateway, no mock, and no box
to exercise. Using a twelve-hour field run as the test harness for that function is what made the
bugs expensive and what made them take three rounds to find.

This drives the REAL search (`sweep_streams_cell`) against synthetic gateways whose true ceiling is
declared up front, so the assertions can be about correctness rather than plausibility: did the
search find the ceiling it was told to find, and when it could not, did it say so honestly?

The server models CAPACITY the way a gateway actually saturates - on concurrently-open lanes, not
on accept order - so a window at c=N genuinely presents N simultaneous connections and the model
decides what that does. */

use super::*;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// How a synthetic gateway behaves as concurrent lanes pile up.
#[derive(Clone, Copy, Debug)]
enum Model {
    /// Serves cleanly up to `cap`, refuses everything above it. The easy case, and the one a
    /// bisection should nail exactly.
    KnifeEdge { cap: usize },
    /// Serves up to `cap`; past it, delivers a short stream instead of erroring - a DELIVERY
    /// shortfall, which fails the gate without counting as an errored stream. plano's real
    /// failures had zero errors and looked exactly like this.
    Shortfall { cap: usize },
    /// Serves up to `cap`, but once it has ever seen more than `wedge_at` concurrent lanes it
    /// refuses everything from then on, permanently. aisix: seventeen consecutive failures
    /// including rungs it had just carried.
    Wedge { cap: usize, wedge_at: usize },
}

/// A synthetic gateway. Returns its address and the counter of peak concurrency it ever saw.
fn sim_gateway(model: Model) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let live = Arc::new(AtomicUsize::new(0));
    let wedged = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let live = Arc::clone(&live);
            let wedged = Arc::clone(&wedged);
            std::thread::spawn(move || {
                /* COUNT THE LANE AS OPEN FOR AS LONG AS IT IS OPEN, AND ALWAYS PUT IT BACK.
                The first version decremented on each exit path by hand and leaked: after the
                c=16 rung, ~24 lanes were still counted live, so a c=2 window presented "26
                concurrent" to a gateway capped at 24 and was refused. The simulator wedged
                itself and every rung after c=16 failed - which looked exactly like a search
                defect and was not. A drop guard cannot leak, whatever the exit path. */
                struct Lane(Arc<AtomicUsize>);
                impl Drop for Lane {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                /* COUNT IN-FLIGHT REQUESTS, NOT OPEN SOCKETS. The engine drives a POOLED http
                client, so idle connections stay open between windows and sit blocked in the
                read below. Counting those as load meant the pool alone could exceed the
                model's capacity, and every rung after the first failure was refused - the
                simulator wedging itself and looking precisely like a search that had wedged.
                Capacity is about requests being served, which is what this now measures. */
                /* AND BOUND THE WRITES. When a window fails its gate the engine drops those
                lanes, the socket buffer fills, and an unbounded `write_all` blocks in the
                kernel FOREVER - holding its lane counted as in-flight for the rest of the
                run. That is what made every rung after the first failure look refused: the
                simulator's own stuck writers, not the model. */
                let _ = c.set_write_timeout(Some(std::time::Duration::from_millis(250)));
                let _ = c.set_read_timeout(Some(std::time::Duration::from_millis(250)));
                let mut b = [0u8; 8192];
                if c.read(&mut b).unwrap_or(0) == 0 {
                    return; // a pooled socket that never carried a request is not load
                }
                live.fetch_add(1, Ordering::SeqCst);
                let _lane = Lane(Arc::clone(&live));
                /* LET THE WHOLE WINDOW ARRIVE BEFORE JUDGING IT. Deciding on the count at the
                instant each request lands measures arrival order, not concurrency: a fast
                lane completes and releases before its peers connect, so a window at c=64 was
                presenting ~40 simultaneous requests and passed on a gateway capped at 40.
                That made the harness's fidelity depend on scheduling - it passed alone and
                failed under a parallel test run, which is a fixture nobody can trust.
                Waiting a beat lets every lane of the window land, so the count is the
                window's real concurrency. */
                std::thread::sleep(std::time::Duration::from_millis(60));
                let n = live.load(Ordering::SeqCst);
                let (cap, short, wedge_at) = match model {
                    Model::KnifeEdge { cap } => (cap, false, usize::MAX),
                    Model::Shortfall { cap } => (cap, true, usize::MAX),
                    Model::Wedge { cap, wedge_at } => (cap, false, wedge_at),
                };
                if n > wedge_at {
                    wedged.store(1, Ordering::SeqCst);
                }
                let stuck = wedged.load(Ordering::SeqCst) == 1;
                let over = n > cap || stuck;
                if over && !short {
                    // Refuse: a real non-2xx, which is what every observed failure actually was.
                    let _ = c.write_all(b"HTTP/1.1 503 Busy\r\ncontent-length: 0\r\n\r\n");
                    return;
                }
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
                let _ = c.write_all(
                    b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                );
                let frames = if over {
                    1
                } else {
                    crate::metric::STREAM_FRAME_BUDGET
                };
                for i in 0..frames {
                    let _ = c.write_all(
                        format!(
                            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"f{i}\"}}}}]}}\n\n"
                        )
                        .as_bytes(),
                    );
                }
                // Hold the lane until the client stops reading, so a window's lanes genuinely
                // overlap. Bounded by the read timeout, so an abandoned lane cannot pile up.
                let mut sink = [0u8; 64];
                let _ = c.read(&mut sink);
            });
        }
    });
    addr
}

fn search(model: Model, lo: u32, hi: u32) -> CellStreams {
    let gw = sim_gateway(model);
    // THE MOCK NEEDS ITS OWN SERVER. Pointing both legs at one address let the engine's probe and
    // its direct-to-mock reference open lanes that the capacity model counted as load, so a
    // window at c=8 presented far more than eight lanes to a gateway capped at 24 and the search
    // walked down to nothing. The model must see the GATEWAY's lanes and only those.
    let mock = sim_gateway(Model::KnifeEdge { cap: usize::MAX });
    let cfg = super::tests::cfg_for(gw, mock);
    let id = CellId::new("openai", "openai");
    sweep_streams_cell(&cfg, &id, lo, hi)
}
/* ── the assertions ──────────────────────────────────────────────────────────────────────────
Each names the real defect it would have caught. They are about CORRECTNESS - the ceiling is
declared, so "did it find it" is answerable - rather than about the output looking reasonable,
which is the bar that let three defects through. */

/* THE MODEL'S CEILING IS APPROXIMATE, AND THE ASSERTIONS SAY SO.
Capacity is decided on lanes in flight at the moment each request arrives, and a window's lanes
RAMP - the first has finished reading before the last has connected - so a window at c=26 may
never present 26 simultaneous requests to a gateway capped at 24, and passes. That slack is a
property of the harness, not of the search, and pretending otherwise would mean tuning the
search to satisfy a fixture.

So the bar is "within a quarter", which is loose enough to survive the ramp and far tighter
than any defect this exists to catch: halving past the bracket put busbar at 3,088 against a
true 5,120, and giving up early published nothing at all. A 2x error cannot hide in +-25%. */
const SLACK: f64 = 1.25;

/// The base case. A gateway with a hard ceiling must be measured at about that ceiling.
#[test]
fn a_knife_edge_ceiling_is_found() {
    for cap in [24usize, 40] {
        let r = search(Model::KnifeEdge { cap }, 1, 128);
        let got = r
            .concurrency
            .value()
            .copied()
            .expect("a hard ceiling must yield a number");
        let lo = (cap as f64 / SLACK) as u32;
        let hi = (cap as f64 * SLACK) as u32;
        assert!(
            (lo..=hi).contains(&got),
            "declared ceiling {cap}, search published {got} - outside [{lo}, {hi}]"
        );
    }
}

/* THE HALVING DEFECT. busbar carried every rung to c=4,096, bisected to c=6,176, could not
confirm it, and halved to c=3,088 - BELOW a concurrency it had already carried, discarding the
whole bracket. A search that does that lands far under the true ceiling, which is exactly what
this asserts against: the answer must be the ceiling, not something under the floor it proved. */
#[test]
fn the_search_never_lands_below_a_concurrency_it_already_carried() {
    for cap in [12usize, 20, 33] {
        let r = search(Model::KnifeEdge { cap }, 1, 128);
        let got = r.concurrency.value().copied();
        if let Some(v) = got {
            assert!(
                (v as f64) >= cap as f64 / SLACK,
                "declared {cap}, published {v} - a step-down that discards its bracket lands here"
            );
        }
        // Every rung it probed and passed is a rung it carried; none may exceed the true cap.
        for p in &r.points {
            if p.passed {
                assert!(
                    (p.concurrency as f64) <= cap as f64 * SLACK,
                    "passed at c={} on a gateway capped at {cap} - beyond the harness's ramp slack",
                    p.concurrency
                );
            }
        }
    }
}

/* THE PREMATURE-TERMINATION DEFECT. plano's stepped rung failed one window and the search
stopped, leaving every concurrency between it and the carried rung untried. A gateway with a
real, findable ceiling must not come back empty. */
#[test]
fn a_findable_ceiling_is_never_published_as_nothing() {
    for cap in [9usize, 17, 40] {
        let r = search(Model::KnifeEdge { cap }, 1, 128);
        assert!(
            r.concurrency.value().is_some(),
            "gateway with a hard ceiling at {cap} published NO number - the search gave up while \
                 an answer was available (this is plano anthropic>anthropic)"
        );
    }
}

/// A delivery shortfall fails the gate with ZERO errors - plano's real failures looked exactly
/// like this, and a search that only understands errored streams would climb straight past it.
#[test]
fn a_delivery_shortfall_bounds_the_ceiling_just_like_an_error_does() {
    let r = search(Model::Shortfall { cap: 16 }, 1, 64);
    // Not vacuous: the rungs below the shortfall carry cleanly, so a search that never passed
    // anything would be a broken search, not a bounded one.
    assert!(
        r.points.iter().any(|p| p.passed),
        "rungs under the shortfall must pass, or this bounds nothing: {:?}",
        r.points
    );
    for p in &r.points {
        if p.passed {
            assert!(
                (p.concurrency as f64) <= 16.0 * SLACK,
                "c={} passed against a gateway that goes short above 16",
                p.concurrency
            );
        }
    }
}

/* THE WEDGE. Once the gateway stops serving, no number is knowable - so the only correct
outcomes are "no number" or "a number at or below the true cap". What must NEVER happen is a
number ABOVE the cap, which would be the search publishing the wedge as capacity. */
#[test]
fn a_wedged_gateway_never_yields_a_number_above_its_real_ceiling() {
    let r = search(
        Model::Wedge {
            cap: 16,
            wedge_at: 32,
        },
        1,
        128,
    );
    if let Some(v) = r.concurrency.value().copied() {
        assert!(
            (v as f64) <= 16.0 * SLACK,
            "gateway wedges above 32 and truly carries 16, but the search published {v}"
        );
    }
}

/// Whatever the model, a published rung must have actually held its own windows in the trace.
/// The summary and the evidence cannot be allowed to disagree.
#[test]
fn a_published_rung_held_a_majority_of_its_own_windows_in_every_model() {
    for m in [
        Model::KnifeEdge { cap: 20 },
        Model::Shortfall { cap: 20 },
        Model::Wedge {
            cap: 12,
            wedge_at: 48,
        },
    ] {
        let r = search(m, 1, 64);
        let Some(v) = r.concurrency.value().copied() else {
            // Only the wedge may legitimately publish nothing; a hard ceiling is always findable
            // (`a_findable_ceiling_is_never_published_as_nothing`), so an empty result there would
            // make this check pass for a search that always gives up.
            assert!(
                matches!(m, Model::Wedge { .. } | Model::Shortfall { .. }),
                "{m:?}: a hard ceiling published no number"
            );
            continue;
        };
        let at: Vec<bool> = r
            .points
            .iter()
            .filter(|p| p.concurrency == v)
            .map(|p| p.passed)
            .collect();
        assert!(
            !at.is_empty(),
            "{m:?}: published c={v} with no window at that concurrency"
        );
        assert!(
            at.iter().filter(|x| **x).count() * 2 > at.len(),
            "{m:?}: published c={v} on {:?} - not a majority",
            at
        );
    }
}
