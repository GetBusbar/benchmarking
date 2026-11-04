use super::*;

// A CAP THAT CANNOT FIRE IS NOT A CAP, so this drives the actual reader against a peer that does
// the thing the cap exists for: answers with endless short, legal header lines.
#[test]
fn an_endless_header_stream_is_refused_rather_than_accumulated() {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        // Consume the request head so the client is not blocked on its own write.
        let mut r = BufReader::new(sock.try_clone().expect("clone"));
        let mut line = String::new();
        while r.read_line(&mut line).unwrap_or(0) > 0 {
            if line == "\r\n" || line.is_empty() {
                break;
            }
            line.clear();
        }
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\n");
        // Endless legal headers. If the cap does not fire the client reads until its deadline.
        loop {
            if sock
                .write_all(b"X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n")
                .is_err()
            {
                break;
            }
        }
    });

    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .expect("write");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let got = read_head(&mut stream, deadline);
    drop(stream);
    let _ = server.join();

    let err = got.expect_err("an endless header stream must be refused, not accumulated");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("exceeds the") && msg.contains("byte cap"),
        "the refusal must name the cap that fired rather than reading to the deadline: {msg}"
    );
}
