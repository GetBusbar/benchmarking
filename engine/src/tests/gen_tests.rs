use super::*;
// The test peers below are plain blocking servers on their own threads: they stand in for a
// gateway, and holding them to the generator's own async shape would test the harness twice
// instead of testing it against something independent.
use std::io::{Read, Write};
use std::net::TcpListener;

fn stats(lat: &[u64], ok: u64, fail: u64, elapsed: f64) -> GenStats {
    GenStats {
        ok,
        fail,
        elapsed_s: elapsed,
        latencies_us: lat.to_vec(),
        spawn_failed: false,
        rig_refused: 0,
        budget_exceeded: 0,
        p50_us: None,
        p99_us: None,
    }
}

// THE PERCENTILE CONVENTION IS THE ENGINE'S, NOT THIS FILE'S. A one-index difference is a silent
// disagreement between two instruments that both look correct, and this engine shipped one
// (ledger SRCH-04): the load generator resolved a rank with floor while the streaming
// percentiles used ceil, with comments in both files claiming they agreed.
//
// The floor convention this test used to pin is exactly what makes the case below wrong: it put
// p50 at index 5 of ten (the SIXTH value, above the middle) and p99 at index 9 (the MAXIMUM),
// and the assertion literally read "the last value". A percentile that is the maximum is not a
// percentile, which `metric.rs`'s own test asserted in the same crate.
#[test]
fn percentiles_are_nearest_rank_on_the_engines_one_convention() {
    let s = stats(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100], 10, 0, 1.0);
    assert_eq!(
        s.pct_us(0.50),
        50,
        "ceil(10*0.5)=5 -> index 4 -> the 5th value"
    );
    assert_eq!(s.pct_us(0.0), 10);
    assert_eq!(
        s.pct_us(1.0),
        100,
        "q=1.0 clamps to the last index rather than overflowing"
    );
    // Every rank goes through the one function, so this cannot drift back apart.
    for q in [0.0, 0.5, 0.9, 0.95, 0.99, 1.0] {
        let sorted = s.sorted_latencies();
        assert_eq!(
            s.pct_us(q),
            sorted[crate::stats::nearest_rank_index(sorted.len(), q)]
        );
    }
    // Over a hundred samples - the count the streaming legs take - a p99 must not be the max.
    let big: Vec<u64> = (1..=100).collect();
    let s100 = stats(&big, 100, 0, 1.0);
    assert_eq!(s100.pct_us(0.99), 99);
    assert_eq!(s100.pct_us(0.50), 50);
    assert!(
        s100.pct_us(0.99) < s100.pct_us(1.0),
        "a p99 that equals the maximum is the worst sample wearing a percentile's name"
    );
}

#[test]
fn an_empty_sample_has_no_percentile_rather_than_a_wrong_one() {
    assert_eq!(stats(&[], 0, 0, 1.0).pct_us(0.99), 0);
}

#[test]
fn rps_is_successes_over_measured_elapsed_not_nominal_duration() {
    // Measured elapsed, because a run that took longer than asked must not report the rate it
    // would have had. The Go generator makes the same choice.
    assert_eq!(stats(&[1], 1000, 0, 10.0).rps(), 100.0);
    assert_eq!(
        stats(&[1], 1000, 0, 0.0).rps(),
        0.0,
        "no elapsed time is no rate, not a division"
    );
}

#[test]
fn the_stats_line_is_byte_compatible_with_the_existing_parser() {
    let s = stats(&[1000, 2000, 3000, 4000], 4, 1, 2.0);
    let line = s.stats_line();
    for k in ["rps=", "fail=", "p50=", "p99=", "p50us=", "p99us=", "ok="] {
        assert!(line.contains(k), "missing {k} in {line}");
    }
    // The existing Rust parser must read our own output.
    let parsed = crate::loadgen::parse_ugen_line(&line);
    assert!(
        parsed.is_measured(),
        "our own stats line must parse: {line}"
    );
}

#[test]
fn a_failed_request_counts_as_a_failure_and_contributes_no_latency() {
    let s = stats(&[], 0, 5, 1.0);
    assert_eq!(s.fail, 5);
    assert!(
        s.latencies_us.is_empty(),
        "a failure has no latency to report"
    );
    assert_eq!(s.rps(), 0.0, "failures are not throughput");
}

#[test]
fn the_request_carries_the_body_and_every_supplied_header() {
    let cfg = GenConfig {
        addr: "127.0.0.1:1".parse().expect("literal"),
        path: "/v1/chat/completions".into(),
        body: r#"{"a":1}"#.into(),
        headers: vec![("x-one".into(), "1".into()), ("x-two".into(), "2".into())],
        concurrency: 1,
        duration: Duration::from_millis(1),
        ttft_ms: 0,
    };
    let r = build_request(&cfg).expect("a well-formed header list must build");
    assert!(r.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(r.contains("x-one: 1\r\n") && r.contains("x-two: 2\r\n"));
    assert!(r.ends_with(r#"{"a":1}"#));
    assert!(
        r.contains("content-length: 7\r\n"),
        "length must match the body exactly"
    );
}

// End to end against a real socket: the generator must actually move requests and record them.
#[test]
fn drives_a_real_socket_and_records_what_it_measured() {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = Arc::clone(&stop);
    std::thread::spawn(move || {
        for c in l.incoming() {
            if s2.load(Ordering::Relaxed) {
                return;
            }
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while c.read(&mut b).unwrap_or(0) > 0 {
                    if c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    let cfg = GenConfig {
        addr,
        path: "/x".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 2,
        duration: Duration::from_millis(250),
        ttft_ms: 0,
    };
    let g = run(&cfg);
    stop.store(true, Ordering::Relaxed);
    assert!(g.ok > 0, "the generator must actually complete requests");
    assert_eq!(
        g.latencies_us.len() as u64,
        g.ok,
        "every success contributes exactly one latency"
    );
    assert!(g.elapsed_s > 0.0);
    assert!(g.rps() > 0.0, "a run that completed requests has a rate");
}

// A chunked response is what a real gateway sends whenever it does not buffer to compute a
// length. A reader that stops at the header terminator instead of draining the body would leave
// the body on the socket, so the NEXT request on the reused connection would read those bytes as
// its status line and count a success as a failure.
#[test]
fn a_chunked_response_is_drained_so_the_next_request_is_not_corrupted() {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming().take(8) {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                // Answer every request on this connection with a CHUNKED body, so a reader that
                // does not drain will desync on the second one.
                while c.read(&mut b).unwrap_or(0) > 0 {
                    let r = "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n\
                                 4\r\nabcd\r\n4\r\nefgh\r\n0\r\n\r\n";
                    if c.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let cfg = GenConfig {
        addr,
        path: "/x".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 1,
        duration: Duration::from_millis(250),
        ttft_ms: 0,
    };
    let g = run(&cfg);
    assert!(
        g.ok > 1,
        "chunked responses must complete, got ok={} fail={}",
        g.ok,
        g.fail
    );
    // The real tell: with an undrained body the connection desyncs and every request after the
    // first is misread as a non-2xx, so failures would dominate.
    assert_eq!(
        g.fail, 0,
        "a drained connection must not manufacture failures, got {}",
        g.fail
    );
}

// A PEER THAT CLOSES IS NOT A PEER THAT FAILED.
//
// HTTP/1.0 defaults to closing after each response and must opt IN to keep-alive. The generator
// reuses connections, so a failure on a reused connection must be attributed to OUR reuse of a
// connection the peer told us it was closing, never to the target: counting it as a request
// failure would read throughput as roughly half and fail the clean-window gate
// (`Sample.passed` requires fail == 0) for a peer that answered every request correctly.
#[test]
fn a_peer_that_answers_and_closes_is_all_successes_and_no_failures() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            std::thread::spawn(move || {
                // Read one request, answer it in HTTP/1.0 with no keep-alive, then close. Legal,
                // and exactly what a plain HTTP/1.0 server does.
                let mut b = [0u8; 4096];
                if conn.read(&mut b).unwrap_or(0) == 0 {
                    return;
                }
                let _ = conn.write_all(
                        b"HTTP/1.0 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":true}",
                    );
            });
        }
    });

    let stats = run(&GenConfig {
        addr,
        path: "/v1/chat/completions".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 4,
        duration: Duration::from_millis(400),
        ttft_ms: 0,
    });

    assert!(
        stats.ok > 0,
        "the peer answers every request, so there must be successes"
    );
    assert_eq!(
        stats.fail, 0,
        "a peer closing a connection it said it would close is not a failed request: ok={} fail={}",
        stats.ok, stats.fail
    );
}

// The other half of the same rule: a peer that closes WITHOUT answering is a real failure, and
// must still be counted. Otherwise the fix above would silently swallow a gateway that accepts
// connections and refuses to serve, which is a live failure mode (a container that binds its
// port and then dies at config load looks exactly like this).
#[test]
fn a_peer_that_accepts_and_closes_without_answering_is_a_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            drop(conn); // accept, then close immediately, saying nothing
        }
    });

    let stats = run(&GenConfig {
        addr,
        path: "/v1/chat/completions".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 2,
        duration: Duration::from_millis(300),
        ttft_ms: 0,
    });

    assert_eq!(stats.ok, 0, "nothing was ever answered");
    assert!(
        stats.fail > 0,
        "a peer that never answers must be recorded as failing"
    );
}

// A chunked body delivered across MANY small reads. The terminator search must look only at
// newly-arrived bytes, so a terminator split across a read boundary is still found: rescanning
// the whole body from the start on every read is O(N^2), and every microsecond of it lands
// inside the timed window and is charged to the gateway.
#[test]
fn a_chunked_body_split_across_many_reads_still_terminates() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming().take(4) {
            let Ok(mut conn) = conn else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while conn.read(&mut b).unwrap_or(0) > 0 {
                    if conn
                        .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n")
                        .is_err()
                    {
                        return;
                    }
                    // Many small chunks, then the terminator written one byte at a time so it
                    // straddles read boundaries.
                    for _ in 0..64 {
                        if conn.write_all(b"4\r\nabcd\r\n").is_err() {
                            return;
                        }
                    }
                    for byte in b"0\r\n\r\n" {
                        if conn.write_all(&[*byte]).is_err() {
                            return;
                        }
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
            });
        }
    });
    let cfg = GenConfig {
        addr,
        path: "/x".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 1,
        duration: Duration::from_millis(400),
        ttft_ms: 0,
    };
    let stats = run(&cfg);
    assert!(
        stats.ok > 0,
        "a fragmented chunked body must complete, ok={} fail={}",
        stats.ok,
        stats.fail
    );
    assert_eq!(
        stats.fail, 0,
        "a terminator split across reads must not read as a failure"
    );
}

// A body with no length header and no chunking runs to connection close. That is a legitimate
// framing, NOT a zero-length body, and it must not be confused with one.
#[test]
fn a_response_with_no_framing_header_is_not_treated_as_an_empty_body() {
    assert!(matches!(
        framing("http/1.1 200 ok\r\n"),
        Framing::UntilClose
    ));
    assert!(matches!(
        framing("http/1.1 200 ok\r\ncontent-length: 12\r\n"),
        Framing::Length(12)
    ));
    assert!(matches!(
        framing("http/1.1 200 ok\r\ntransfer-encoding: chunked\r\n"),
        Framing::Chunked
    ));
}

// A non-2xx is a FAILURE, not a fast success. Counting an error page as throughput is how a
// broken gateway posts its best number.
#[test]
fn a_non_2xx_response_is_a_failure_not_throughput() {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming().take(64) {
            let Ok(mut c) = c else { continue };
            let mut b = [0u8; 4096];
            let _ = c.read(&mut b);
            let _ = c.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
        }
    });
    let cfg = GenConfig {
        addr,
        path: "/x".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 1,
        duration: Duration::from_millis(150),
        ttft_ms: 0,
    };
    let g = run(&cfg);
    assert_eq!(g.ok, 0, "a 503 is never a success");
    assert!(g.fail > 0, "and it must be counted as a failure");
}

// THE POINT OF THE REWRITE, pinned as a test: a concurrency that a thread-per-connection
// generator could not honour on a small box must now cost tasks, not OS threads. 2048 native
// threads on a CI runner is where the old design started losing to its own scheduler; 2048
// tasks is unremarkable. The assertion is deliberately about COMPLETION, not about a rate: this
// proves the instrument can hold the connections it claims, which is exactly what the failed
// field run disproved for threads.
#[test]
fn a_high_concurrency_window_is_held_by_tasks_rather_than_os_threads() {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while c.read(&mut b).unwrap_or(0) > 0 {
                    if c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    let started = Instant::now();
    let g = run(&GenConfig {
        addr,
        path: "/x".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 2048,
        duration: Duration::from_millis(500),
        ttft_ms: 0,
    });
    let wall = started.elapsed();

    assert!(
        g.ok > 0,
        "2048 concurrent holders must still complete requests, ok={} fail={}",
        g.ok,
        g.fail
    );
    // The old failure mode was wall clock exploding far past the requested window because
    // spawning and joining the holders dwarfed the window itself. A generous bound: this is a
    // regression guard against collapse, not a performance assertion.
    assert!(
        wall < Duration::from_secs(20),
        "a 500ms window at c=2048 took {wall:?}; the holders are not cheap"
    );
}

// A RIG LIMIT MUST NEVER BE INDISTINGUISHABLE FROM A GATEWAY FAILURE.
//
// Both of these used to land in `fail` beside a genuine refusal, where nothing downstream could
// separate them - which is how the search came to publish this host's ephemeral port range as a
// gateway's ceiling. They are counted apart now, and the stats line carries them across the
// subprocess boundary because that line is everything the parent learns from the child.
#[test]
fn the_stats_line_carries_our_own_limits_apart_from_the_gateways_failures() {
    let g = GenStats {
        ok: 100,
        fail: 7,
        elapsed_s: 1.0,
        spawn_failed: false,
        rig_refused: 3,
        budget_exceeded: 2,
        latencies_us: vec![1000, 2000, 3000],
        p50_us: None,
        p99_us: None,
    };
    let line = g.stats_line();
    assert!(
        line.contains("rigrefused=3"),
        "our port/descriptor exhaustion must cross the wire: {line}"
    );
    assert!(
        line.contains("budgetexceeded=2"),
        "our response budget must cross the wire: {line}"
    );
    assert!(
        line.contains("fail=7"),
        "and the gateway's own failure count is unchanged: {line}"
    );

    // The parent reads them back.
    let parsed = crate::loadgen::parse_ugen_line(&line)
        .into_value()
        .expect("a line this generator wrote must parse");
    assert_eq!(parsed.rig_refused, 3);
    assert_eq!(parsed.budget_exceeded, 2);
    assert_eq!(parsed.fail, 7);

    // A line from an older generator has neither field and must still parse: absent means "not
    // reported", which is not the same as "did not happen", and refusing an otherwise complete
    // line would turn a cosmetic version skew into a lost measurement.
    let old = "rps=1000 fail=1 p50=1.00 p99=2.00 p50us=1000 p99us=2000 ok=999";
    let legacy = crate::loadgen::parse_ugen_line(old)
        .into_value()
        .expect("an older line must still parse");
    assert_eq!(legacy.rig_refused, 0);
    assert_eq!(legacy.budget_exceeded, 0);
    assert_eq!(legacy.ok, 999);
}

// `spawnfailed` CROSSES THE WIRE TOO. The parent's `if stats.spawn_failed` check could never fire
// while this flag stopped at the subprocess boundary; nothing pinned that the line carries it.
#[test]
fn the_stats_line_carries_spawn_failed_both_ways() {
    for failed in [true, false] {
        let g = GenStats {
            ok: 10,
            elapsed_s: 1.0,
            spawn_failed: failed,
            latencies_us: vec![1000],
            ..Default::default()
        };
        let line = g.stats_line();
        assert!(
            line.contains(&format!("spawnfailed={}", u8::from(failed))),
            "{line}"
        );
        let parsed = crate::loadgen::parse_ugen_line(&line)
            .into_value()
            .expect("a line this generator wrote must parse");
        assert_eq!(parsed.spawn_failed, failed, "{line}");
    }
}

// ---- chunked framing is parsed by size, never searched for --------------------------------------

// A CHUNK WHOSE DATA CONTAINS THE TERMINATOR'S BYTES. A raw search for `0\r\n\r\n` ended this body
// at the first chunk, leaving the real last chunk on a connection about to be reused.
#[test]
fn a_terminator_shaped_sequence_inside_chunk_data_does_not_end_the_body() {
    // One 7-byte chunk whose data is "ab0\r\n\r\n", then the real last chunk.
    let body = b"7\r\nab0\r\n\r\n\r\n";
    assert_eq!(scan_chunks(body, 0), ChunkScan::NeedMore(12));
    let full = b"7\r\nab0\r\n\r\n\r\n0\r\n\r\n";
    assert_eq!(scan_chunks(full, 0), ChunkScan::Complete);
}

// TRAILERS AND EXTENSIONS ARE LEGAL HTTP/1.1. A byte search never recognised a last chunk that
// carried either, so a correct gateway waited out the whole response budget on every request.
#[test]
fn a_last_chunk_with_trailers_or_an_extension_completes() {
    assert_eq!(
        scan_chunks(b"4\r\nabcd\r\n0\r\nx-trailer: y\r\n\r\n", 0),
        ChunkScan::Complete
    );
    assert_eq!(
        scan_chunks(b"4;name=v\r\nabcd\r\n0;ext\r\n\r\n", 0),
        ChunkScan::Complete
    );
    assert_eq!(
        scan_chunks(b"A\r\n0123456789\r\n0\r\n\r\n", 0),
        ChunkScan::Complete
    );
    // The trailer section is not finished until its empty line arrives.
    assert_eq!(
        scan_chunks(b"4\r\nabcd\r\n0\r\nx-trailer: y\r\n", 0),
        ChunkScan::NeedMore(9)
    );
}

#[test]
fn a_body_that_is_not_chunked_framing_is_malformed() {
    assert_eq!(scan_chunks(b"zz\r\nabcd\r\n", 0), ChunkScan::Malformed);
    assert_eq!(scan_chunks(b"\r\nabcd\r\n", 0), ChunkScan::Malformed);
    // A chunk whose data is not followed by CRLF.
    assert_eq!(scan_chunks(b"2\r\nabcd\r\n", 0), ChunkScan::Malformed);
}

// The parse resumes where it stopped: a body arriving a byte at a time completes exactly when its
// last byte lands, and never before.
#[test]
fn a_chunked_body_arriving_byte_by_byte_completes_only_on_its_last_byte() {
    let whole = b"4\r\nabcd\r\n3\r\nefg\r\n0\r\n\r\n";
    let mut at = 0;
    for end in 1..whole.len() {
        match scan_chunks(&whole[..end], at) {
            ChunkScan::NeedMore(next) => at = next,
            other => panic!("completed early at {end} of {}: {other:?}", whole.len()),
        }
    }
    assert_eq!(scan_chunks(whole, at), ChunkScan::Complete);
}

// A GATEWAY THAT ACCEPTS AND THEN HANGS MUST BE VISIBLE.
//
// A read timeout used to be reported as `ClosedBeforeAnyBytes`, which the worker retries in
// silence on a reused connection - no ok, no fail, no budget count, so a hung gateway showed up
// only as mysteriously low rps - and counted as a bare `fail` on a fresh one, where our own
// thirty-second bound firing read exactly like the gateway refusing. The deadline passing is our
// bound, on every connection, and it says so.
#[test]
fn a_read_timeout_on_a_live_connection_is_our_budget_firing_not_a_stale_connection() {
    let hangs = TcpListener::bind("127.0.0.1:0").expect("bind");
    let hang_addr = hangs.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in hangs.incoming() {
            let Ok(c) = c else { continue };
            // Accept, then say nothing and hold the connection open.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(5));
                drop(c);
            });
        }
    });

    let closes = TcpListener::bind("127.0.0.1:0").expect("bind");
    let close_addr = closes.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in closes.incoming() {
            drop(c); // accept and close at once: a peer going away, not a peer hanging
        }
    });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime to drive one exchange");
    rt.block_on(async move {
        let mut acc: Vec<u8> = Vec::new();
        let mut hung = TcpStream::connect(hang_addr).await.expect("connect");
        let out = read_response(
            &mut hung,
            Instant::now() + Duration::from_millis(150),
            &mut acc,
        )
        .await;
        assert_eq!(
            out,
            Exchange::BudgetExceeded,
            "a peer holding a live connection past the deadline is our bound firing, got {out:?}"
        );

        // The other half of the rule, unchanged: a peer that GOES AWAY with nothing sent is
        // still the stale-connection case the worker retries on a reused connection, so this fix
        // must not turn every idle keep-alive close into a failure.
        let mut gone = TcpStream::connect(close_addr).await.expect("connect");
        let out = read_response(
            &mut gone,
            Instant::now() + Duration::from_secs(30),
            &mut acc,
        )
        .await;
        assert_eq!(
            out,
            Exchange::ClosedBeforeAnyBytes,
            "a peer that closed without answering is not our budget, got {out:?}"
        );
    });
}

// OUR CONNECT BOUND IS NOT THE GATEWAY REFUSING, AND IT IS NOT THE RIG RUNNING OUT EITHER.
//
// The five-second connect timeout used to land in `fail` with no attribution at all, beside
// EADDRNOTAVAIL/EMFILE which do get `rig_refused`. It is filed as our bound firing:
// `rig_refused` is the claim "this host had no port or descriptor left", and run.rs turns a
// window containing any of those into an UNMEASURED one - so filing a hung gateway there would
// erase the failure it caused.
#[test]
fn a_connect_timeout_is_charged_to_our_own_bound_never_to_rig_exhaustion() {
    assert_eq!(connect_fault(None), ConnectFault::OurConnectBound);
    assert_eq!(
        connect_fault(Some(&std::io::Error::from_raw_os_error(24))),
        ConnectFault::RigExhausted,
        "EMFILE is this host out of descriptors"
    );
    assert_eq!(
        connect_fault(Some(&std::io::Error::from(
            std::io::ErrorKind::AddrNotAvailable
        ))),
        ConnectFault::RigExhausted,
        "EADDRNOTAVAIL is this host out of ephemeral ports"
    );
    assert_eq!(
        connect_fault(Some(&std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        ))),
        ConnectFault::PeerRefused
    );

    let charged = |f: ConnectFault| {
        let mut w = WorkerStats::default();
        w.charge_connect_fault(f);
        (w.fail, w.rig_refused, w.budget_exceeded)
    };
    assert_eq!(
        charged(ConnectFault::OurConnectBound),
        (1, 0, 1),
        "a connect timeout is a failure the caller felt AND our bound, never rig exhaustion"
    );
    assert_eq!(charged(ConnectFault::RigExhausted), (1, 1, 0));
    assert_eq!(
        charged(ConnectFault::PeerRefused),
        (1, 0, 0),
        "a refusal is the gateway's alone"
    );
}

// THE RATE'S NUMERATOR AND DENOMINATOR MUST DESCRIBE THE SAME WINDOW.
//
// Every lane's in-flight response used to complete during the post-stop drain and count into
// `ok` while `elapsed_s` stayed frozen at the sleep - one free success per lane, largest exactly
// where the peak search is climbing (high concurrency, slow responses). Here nothing can
// possibly answer inside the window, so an honest window reports nothing rather than a rate
// built entirely out of the drain.
#[test]
fn responses_that_land_after_the_window_closes_are_not_counted_into_its_rate() {
    let served = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counter = Arc::clone(&served);
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let counter = Arc::clone(&counter);
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                if c.read(&mut b).unwrap_or(0) == 0 {
                    return;
                }
                counter.fetch_add(1, Ordering::Relaxed);
                // Answer well after the load window has closed: a real response, delivered
                // during the drain.
                std::thread::sleep(Duration::from_millis(700));
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
            });
        }
    });

    let g = run(&GenConfig {
        addr,
        path: "/x".into(),
        body: "{}".into(),
        headers: vec![],
        concurrency: 4,
        duration: Duration::from_millis(200),
        ttft_ms: 0,
    });

    assert!(
        served.load(Ordering::Relaxed) > 0,
        "the window must actually have driven requests, or this proves nothing"
    );
    assert_eq!(
        g.ok, 0,
        "no response arrived inside the window, so none of them is throughput it sustained"
    );
    assert!(
        g.latencies_us.is_empty(),
        "a completion outside the window contributes no latency either"
    );
    assert_eq!(g.rps(), 0.0);
}

// A HEADER VALUE IS NOT ALLOWED TO CHOOSE WHAT BYTES GO ON THE WIRE.
//
// Header values arrive from the manifest across a process boundary, and they are interpolated
// straight into the request head: a CRLF in one appends a header the manifest never wrote, and a
// double CRLF ends the head and makes the rest a second request on the connection.
#[test]
fn a_header_carrying_crlf_is_refused_rather_than_smuggled_onto_the_wire() {
    let with = |headers: Vec<(String, String)>, path: &str| GenConfig {
        addr: "127.0.0.1:1".parse().expect("literal"),
        path: path.into(),
        body: r#"{"a":1}"#.into(),
        headers,
        concurrency: 1,
        duration: Duration::from_millis(1),
        ttft_ms: 0,
    };

    for (name, value) in [
        ("authorization", "Bearer t\r\nx-injected: yes"),
        ("authorization", "Bearer t\r\n\r\nPOST /other HTTP/1.1"),
        ("authorization", "Bearer t\nx-injected: yes"),
        ("x-route\r\nx-injected", "yes"),
        ("x-route: x", "yes"),
    ] {
        let cfg = with(vec![(name.into(), value.into())], "/v1/chat/completions");
        let err =
            build_request(&cfg).expect_err("a header that would inject must not build a request");
        assert!(
            err.contains("inject"),
            "the refusal must name the defect: {err}"
        );
    }

    // The request line is the same wire format and the same manifest source.
    assert!(build_request(&with(vec![], "/v1/chat HTTP/1.1\r\nx: y")).is_err());

    // And an ordinary header list still builds, so the rule is about line breaks rather than
    // about anything a real credential contains.
    let ok = build_request(&with(
        vec![("authorization".into(), "Bearer sk-abc.DEF_123-+/=".into())],
        "/v1/chat/completions",
    ))
    .expect("a real credential must still send");
    assert!(ok.contains("authorization: Bearer sk-abc.DEF_123-+/=\r\n"));
}

// And the window built from such a manifest NEVER RUNS: a sanitised header would send a
// credential or a routing key the manifest did not write and publish the result under the
// pairing it asked for, while a window of failures would charge the gateway for our refusal.
// `spawn_failed` is the existing "the rig never posed the question" answer.
#[test]
fn a_window_whose_request_would_inject_never_runs_and_charges_the_gateway_nothing() {
    let g = run(&GenConfig {
        addr: "127.0.0.1:1".parse().expect("literal"),
        path: "/x".into(),
        body: "{}".into(),
        headers: vec![("x-route".into(), "a\r\nx-injected: b".into())],
        concurrency: 2,
        duration: Duration::from_millis(50),
        ttft_ms: 0,
    });
    assert!(g.spawn_failed, "the rig could not pose the question");
    assert_eq!((g.ok, g.fail), (0, 0), "and nothing is charged to anyone");
}
