use std::sync::OnceLock;

use regex::Regex;

use super::credentials::Credential;

const REDACTED: &str = "[REDACTED]";

/// Remove known credentials and common API-key shapes from text before it can
/// reach diagnostics, logs, or persisted connection-test state.
pub(crate) fn redact(input: &str, credentials: &[&Credential]) -> String {
    let mut output = input.to_string();
    for credential in credentials {
        let secret = credential.expose();
        if !secret.is_empty() {
            output = output.replace(secret, REDACTED);
        }
    }

    for regex in secret_patterns() {
        output = regex.replace_all(&output, REDACTED).into_owned();
    }
    output
}

fn secret_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}",
            r"\bmdb_[A-Za-z0-9_-]{8,}\b",
            r"\bsk-(?:ant-)?[A-Za-z0-9_-]{8,}\b",
            r"\bgh[opsu]_[A-Za-z0-9]{20,}\b",
            r"\bAIza[A-Za-z0-9_-]{20,}\b",
            r#"(?i)\b(?:x-api-key|api[_-]?key|authorization)\s*[:=]\s*(?:\"[^\"]+\"|'[^']+'|[^\s,;]+)"#,
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("static redaction pattern must compile"))
        .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_arbitrary_credentials() {
        let credential = Credential::new("an otherwise unrecognizable secret").unwrap();
        let output = redact(
            "failure: an otherwise unrecognizable secret was rejected",
            &[&credential],
        );
        assert_eq!(output, "failure: [REDACTED] was rejected");
    }

    #[test]
    fn redacts_common_secret_shapes_without_a_known_value() {
        for input in [
            "mdb_abcdefghijklmnopqrstuvwxyz",
            "Authorization: Bearer abcdefghijklmnop",
            "x-api-key=abcdefghijklmnop",
            "sk-ant-abcdefghijklmnop",
        ] {
            let output = redact(input, &[]);
            assert!(!output.contains("abcdefghijklmnop"), "{output}");
            assert!(output.contains(REDACTED), "{output}");
        }
    }
}
