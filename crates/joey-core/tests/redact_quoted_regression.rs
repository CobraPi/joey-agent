use joey_core::redact::*;

/// Regression: quoted secret values must redact INCLUDING spaces — the old
/// regex's (\S+) value group stopped at the first space, leaking the tail.
/// Also: a URL anywhere in the text must not disable redaction for other
/// lines (per-line gate, was whole-text).
#[test]
fn quoted_secrets_with_spaces_are_fully_redacted() {
    let out = redact_sensitive_text("MY_TOKEN=\"abcdefghij secret part two\"");
    assert!(
        !out.contains("secret part two"),
        "quoted tail leaked: {out:?}"
    );
    assert!(!out.contains("abcdefghij"), "quoted head leaked: {out:?}");

    // URL on one line must not disable password redaction on another line.
    let out2 = redact_sensitive_text(
        "see https://example.com/docs for info\npassword=hunter2hunter2hunter2\n",
    );
    assert!(!out2.contains("hunter2"), "password leaked past URL gate: {out2:?}");

    // YAML quoted value with spaces.
    let out3 = redact_sensitive_text("password: \"hunter2 hunter2 hunter2\"");
    assert!(!out3.contains("hunter2"), "yaml quoted leaked: {out3:?}");
}
