use super::{arg_or, parse_or};

// A MALFORMED ARGUMENT IS REFUSED, NOT REPLACED BY THE DEFAULT. `otb loadgen ... abc` used to run a
// c=1 window and print an ordinary stats line, published as the concurrency that was asked for.
#[test]
fn a_present_but_malformed_argument_is_an_error() {
    let args: Vec<String> = ["loadgen", "127.0.0.1:1", "/", "abc"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let err = arg_or::<u32>(&args, 3, 1).expect_err("abc is not a concurrency");
    assert!(
        err.contains("abc"),
        "the refusal quotes what was passed: {err}"
    );
    assert_eq!(
        arg_or::<u64>(&args, 4, 5),
        Ok(5),
        "an absent argument takes the default"
    );
}

// EMPTY IS ABSENT. run-on-ec2.sh exports an unset override as an empty string, so treating "" as
// malformed would refuse every field run that did not narrow its range.
#[test]
fn an_empty_or_unset_setting_takes_the_default_and_garbage_does_not() {
    assert_eq!(parse_or::<u32>(None, 7), Ok(7));
    assert_eq!(parse_or::<u32>(Some(""), 7), Ok(7));
    assert_eq!(parse_or::<u32>(Some("  "), 7), Ok(7));
    assert_eq!(parse_or::<u32>(Some("1024"), 7), Ok(1024));
    assert_eq!(parse_or::<u32>(Some("1o24"), 7), Err("1o24".to_string()));
    assert_eq!(parse_or::<u32>(Some("-1"), 7), Err("-1".to_string()));
}
