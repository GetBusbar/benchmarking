// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// CATCH-ALL DNS (engine v1.2). Every A query, for every name, is answered with ONE address: the
// address this responder is bound on, which is the mock's own. AAAA (and every other type) gets a
// NOERROR answer with no records, so a dual-stack client falls back to the IPv4 answer instead of
// treating the name as missing. There is no hostname list: a gateway that pins
// `bedrock-runtime.us-east-1.amazonaws.com` or `generativelanguage.googleapis.com` reaches the mock
// exactly like one configured with `http://127.0.0.1:8000`, and a gateway that reaches for anything
// else on the internet reaches the mock too, which is what keeps a measured gateway off the network.
//
// The box resolver is pointed here only after setup (image pulls, builds) and only for the
// gateway's lifetime; the engine's own traffic is by IP and never asks. See the engine's
// `egress.rs` for the switch.
//
// Deliberately tiny: one question per message (what every stub resolver sends), no recursion, no
// EDNS, no compression beyond the one pointer back to the question. A message it cannot parse is
// dropped (UDP) or ends the connection (TCP), never answered with a guess.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

/// Seconds a resolver may cache the answer. Short, because the resolver switch is undone at the end
/// of the run and nothing should outlive it for long; long enough that a gateway resolving per
/// connection is not a query per request against a resolver that caches.
const TTL_S: u32 = 60;

const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;

/// The answer to one DNS query message, or `None` for a message that is not a parseable query.
pub fn answer(query: &[u8], ip: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    // A response (QR set) is not a query; answering it would let two responders loop.
    if flags & 0x8000 != 0 {
        return None;
    }
    let opcode = (flags >> 11) & 0x0f;
    let rd = flags & 0x0100;
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    let mut out = Vec::with_capacity(query.len() + 16);
    out.extend_from_slice(&query[0..2]);
    // QR | opcode | AA | RD (copied) | RA
    let base = 0x8000 | (opcode << 11) | 0x0400 | rd | 0x0080;
    if opcode != 0 || qdcount != 1 {
        // NOTIMP for anything but a standard query, FORMERR for a query without exactly one
        // question. Header only: there is no single question to echo.
        let rcode = if opcode != 0 { 4 } else { 1 };
        out.extend_from_slice(&(base | rcode).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        return Some(out);
    }
    // Walk the question name: length-prefixed labels ending in a zero byte. A pointer in a question
    // is malformed for a stub query, and refused rather than followed.
    let mut i = 12usize;
    loop {
        let len = *query.get(i)? as usize;
        if len == 0 {
            i += 1;
            break;
        }
        if len & 0xc0 != 0 || len > 63 {
            return None;
        }
        i += 1 + len;
        if i > 12 + 255 {
            return None;
        }
    }
    let qtype = u16::from_be_bytes([*query.get(i)?, *query.get(i + 1)?]);
    let qclass = u16::from_be_bytes([*query.get(i + 2)?, *query.get(i + 3)?]);
    let qend = i + 4;
    let give_a = qtype == TYPE_A && qclass == CLASS_IN;
    out.extend_from_slice(&base.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&u16::from(give_a).to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&[0, 0, 0, 0]); // NSCOUNT, ARCOUNT (any EDNS OPT record is dropped)
    out.extend_from_slice(&query[12..qend]);
    if give_a {
        out.extend_from_slice(&[0xc0, 0x0c]); // name: pointer to the question at offset 12
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&TTL_S.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&ip.octets());
    }
    Some(out)
}

/// The address the responder answers with: the IPv4 address it is bound on. An unspecified or IPv6
/// bind has no single answer to give, so it is refused.
pub fn answer_ip(bind: SocketAddr) -> Result<Ipv4Addr, String> {
    match bind {
        SocketAddr::V4(a) if !a.ip().is_unspecified() => Ok(*a.ip()),
        other => Err(format!(
            "--dns {other} must be a specific IPv4 address (the mock's own), because that address is the answer to every query"
        )),
    }
}

/// Bind UDP and TCP on `bind` and serve on spawned tasks. Returns once both are bound, so a bind
/// failure is the caller's to treat as fatal before anything is served.
pub async fn start(bind: SocketAddr) -> Result<(), String> {
    let ip = answer_ip(bind)?;
    let udp = UdpSocket::bind(bind)
        .await
        .map_err(|e| format!("dns: udp bind {bind}: {e}"))?;
    let tcp = TcpListener::bind(bind)
        .await
        .map_err(|e| format!("dns: tcp bind {bind}: {e}"))?;
    eprintln!("mock dns listening on {bind} (udp+tcp): every A query -> {ip}, AAAA -> no records");
    tokio::spawn(serve_tcp(tcp, ip));
    tokio::spawn(serve_udp(Arc::new(udp), ip));
    Ok(())
}

async fn serve_udp(sock: Arc<UdpSocket>, ip: Ipv4Addr) {
    let mut buf = vec![0u8; 4096];
    loop {
        let (n, peer) = match sock.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("mock dns: udp recv failed: {e}");
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        if let Some(reply) = answer(&buf[..n], ip) {
            let _ = sock.send_to(&reply, peer).await;
        }
    }
}

async fn serve_tcp(listener: TcpListener, ip: Ipv4Addr) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        tokio::spawn(async move {
            // Length-prefixed messages until the peer closes or goes quiet.
            loop {
                let mut len = [0u8; 2];
                let read =
                    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut len));
                if !matches!(read.await, Ok(Ok(_))) {
                    return;
                }
                let mut msg = vec![0u8; usize::from(u16::from_be_bytes(len))];
                if stream.read_exact(&mut msg).await.is_err() {
                    return;
                }
                let Some(reply) = answer(&msg, ip) else {
                    return;
                };
                let Ok(rlen) = u16::try_from(reply.len()) else {
                    return;
                };
                if stream.write_all(&rlen.to_be_bytes()).await.is_err()
                    || stream.write_all(&reply).await.is_err()
                {
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub resolver's query for `name` of `qtype`, with an EDNS OPT record appended the way
    /// glibc and Go send one.
    fn query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&id.to_be_bytes());
        q.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
        q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 1]); // QD=1, AR=1
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        q.extend_from_slice(&[0, 0, 41, 0x04, 0xd0, 0, 0, 0, 0, 0, 0]); // OPT
        q
    }

    fn ancount(r: &[u8]) -> u16 {
        u16::from_be_bytes([r[6], r[7]])
    }

    #[test]
    fn any_name_gets_one_a_record_with_the_mocks_address() {
        let ip = Ipv4Addr::new(127, 0, 0, 1);
        for name in [
            "bedrock-runtime.us-east-1.amazonaws.com",
            "generativelanguage.googleapis.com",
            "anything.at.all.example",
        ] {
            let r = answer(&query(0xbeef, name, TYPE_A), ip).expect("a query is answered");
            assert_eq!(&r[0..2], &[0xbe, 0xef], "the id is echoed");
            assert_eq!(r[2] & 0x80, 0x80, "QR is set");
            assert_eq!(r[3] & 0x0f, 0, "NOERROR");
            assert_eq!(ancount(&r), 1, "{name}: exactly one answer");
            assert_eq!(
                &r[r.len() - 4..],
                &[127, 0, 0, 1],
                "{name}: the answer is the mock"
            );
        }
    }

    #[test]
    fn aaaa_and_other_types_get_noerror_with_no_records() {
        let ip = Ipv4Addr::new(10, 0, 0, 9);
        for qtype in [28u16, 65, 15] {
            let r = answer(&query(7, "api.openai.com", qtype), ip).expect("answered");
            assert_eq!(
                r[3] & 0x0f,
                0,
                "NOERROR, not NXDOMAIN, so the A answer stands"
            );
            assert_eq!(ancount(&r), 0, "type {qtype} has no records");
        }
    }

    #[test]
    fn garbage_and_responses_are_not_answered() {
        let ip = Ipv4Addr::new(127, 0, 0, 1);
        assert!(answer(b"short", ip).is_none());
        let mut resp = query(1, "x.example", TYPE_A);
        resp[2] |= 0x80;
        assert!(answer(&resp, ip).is_none(), "a response is never answered");
        let mut truncated = query(1, "x.example", TYPE_A);
        truncated.truncate(16);
        assert!(answer(&truncated, ip).is_none());
    }

    #[test]
    fn an_unspecified_bind_is_refused_because_it_has_no_single_answer() {
        assert!(answer_ip("0.0.0.0:53".parse().expect("addr")).is_err());
        assert!(answer_ip("[::1]:53".parse().expect("addr")).is_err());
        assert_eq!(
            answer_ip("127.0.0.1:53".parse().expect("addr")),
            Ok(Ipv4Addr::LOCALHOST)
        );
    }

    // Over real sockets, both transports.
    #[tokio::test]
    async fn the_responder_answers_over_udp_and_tcp() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("pick a port");
        let addr = probe.local_addr().expect("addr");
        drop(probe);
        start(addr).await.expect("the responder binds");

        let client = UdpSocket::bind("127.0.0.1:0").await.expect("client");
        client
            .send_to(
                &query(42, "bedrock-runtime.us-east-1.amazonaws.com", TYPE_A),
                addr,
            )
            .await
            .expect("send");
        let mut buf = [0u8; 512];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .expect("udp answer in time")
            .expect("recv");
        assert_eq!(ancount(&buf[..n]), 1);
        assert_eq!(&buf[n - 4..n], &[127, 0, 0, 1]);

        let mut tcp = tokio::net::TcpStream::connect(addr).await.expect("tcp");
        let q = query(43, "oauth2.googleapis.com", TYPE_A);
        tcp.write_all(&(q.len() as u16).to_be_bytes())
            .await
            .expect("w");
        tcp.write_all(&q).await.expect("w");
        let mut len = [0u8; 2];
        tcp.read_exact(&mut len).await.expect("len");
        let mut r = vec![0u8; usize::from(u16::from_be_bytes(len))];
        tcp.read_exact(&mut r).await.expect("msg");
        assert_eq!(ancount(&r), 1);
        assert_eq!(&r[r.len() - 4..], &[127, 0, 0, 1]);
    }
}
