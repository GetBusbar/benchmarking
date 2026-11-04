use super::*;

/* A CRLF PAIR IN THE BODY MUST NOT BE MISTAKEN FOR THE END OF THE HEAD. The bare-LF fallback
used to fire only when no CRLF existed anywhere in the buffer, and the buffer already holds
body bytes - so an LF-terminated head followed by CRLF-terminated frames lost its first
frame and credited TTFT to the second. */
#[test]
fn the_earlier_blank_line_ends_the_head_whichever_form_it_takes() {
    let mixed =
        b"HTTP/1.1 200 OK\nContent-Type: text/event-stream\n\ndata: alpha\r\n\r\ndata: beta\n\n";
    let end = find_head_end(mixed).expect("head must be found");
    let body = &mixed[end..];
    assert!(
        body.starts_with(b"data: alpha"),
        "the first frame must survive; body began {:?}",
        String::from_utf8_lossy(&body[..body.len().min(24)])
    );
}

/// The ordinary all-CRLF head must still end at its own blank line, not at a later one.
#[test]
fn a_normal_crlf_head_ends_at_its_own_blank_line() {
    let normal = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: one\r\n\r\n";
    let end = find_head_end(normal).expect("head must be found");
    assert!(normal[end..].starts_with(b"data: one"));
}

/// And an all-LF stream is unchanged.
#[test]
fn an_all_lf_head_is_unchanged() {
    let lf = b"HTTP/1.1 200 OK\nx: 1\n\ndata: one\n\n";
    let end = find_head_end(lf).expect("head must be found");
    assert!(lf[end..].starts_with(b"data: one"));
}

/// An incomplete head is still "not yet", not a wrong answer.
#[test]
fn a_head_that_has_not_arrived_yet_is_none() {
    assert_eq!(
        find_head_end(b"HTTP/1.1 200 OK\r\ncontent-type: text/"),
        None
    );
}
