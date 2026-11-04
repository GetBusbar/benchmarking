use super::resolve_minted_auth;

// A tiny throwaway directory per test, so tests can run concurrently without treading on each
// other's `.minted-auth`. Not `tempfile`: this crate takes no dev-dependency for one file.
fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "otb-minted-auth-test-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// THE CASE THIS EXISTS FOR: a gateway whose real credential can only be known after it boots
// gets that credential into the probe, not the manifest's placeholder.
#[test]
fn a_minted_file_overrides_the_declared_auth() {
    let dir = scratch_dir("present");
    std::fs::write(dir.join(".minted-auth"), "sk-real-key-123\n").unwrap();
    assert_eq!(
        resolve_minted_auth(&dir),
        Ok(Some("sk-real-key-123".to_string()))
    );
}

// TWELVE OF THIRTEEN ENTRANTS: no file, so the declared `auth` stands untouched.
#[test]
fn no_file_means_no_override() {
    let dir = scratch_dir("absent");
    assert_eq!(resolve_minted_auth(&dir), Ok(None));
}

// A command that ran but wrote nothing useful (a failed mint that still touched the file, an
// empty redirect) must not silently authenticate with an empty bearer token - that is a
// different, worse failure mode than "kept the placeholder".
#[test]
fn a_whitespace_only_file_means_no_override() {
    let dir = scratch_dir("blank");
    std::fs::write(dir.join(".minted-auth"), "   \n\t\n").unwrap();
    assert_eq!(resolve_minted_auth(&dir), Ok(None));
}

#[test]
fn surrounding_whitespace_is_trimmed() {
    let dir = scratch_dir("padded");
    std::fs::write(dir.join(".minted-auth"), "  sk-abc  \n").unwrap();
    assert_eq!(resolve_minted_auth(&dir), Ok(Some("sk-abc".to_string())));
}

// SOMETHING WAS MINTED AND WE COULD NOT READ IT. That is not "nothing was minted": folding it into the
// absent case skipped the stale-credential refusal and authenticated with the placeholder. A
// directory where the file should be is unreadable as a file on every platform, root or not.
#[test]
fn an_unreadable_minted_file_is_an_error_not_an_absence() {
    let dir = scratch_dir("unreadable");
    let _ = std::fs::remove_dir_all(dir.join(".minted-auth"));
    std::fs::create_dir_all(dir.join(".minted-auth")).unwrap();
    let got = resolve_minted_auth(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let err = got.expect_err("an unreadable credential must not read as no credential");
    assert!(
        err.contains(".minted-auth"),
        "the error names the file: {err}"
    );
}
