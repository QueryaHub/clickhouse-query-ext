use regex::Regex;
use std::io::{self, Write};
use std::sync::OnceLock;
use tracing_subscriber::{
    EnvFilter,
    fmt::{self, MakeWriter},
    prelude::*,
};

/// Returns the compiled list of sanitization regex patterns.
fn sanitization_patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            (
                Regex::new(r#"(?i)("password"\s*:\s*)"[^"]*""#).unwrap(),
                r#"${1}"[REDACTED BY RUST DRIVER]""#,
            ),
            (
                Regex::new(r#"(?i)("jwtToken"\s*:\s*)"[^"]*""#).unwrap(),
                r#"${1}"[REDACTED BY RUST DRIVER]""#,
            ),
            (
                Regex::new(r#"(?i)(password=)[^&\s"]+"#).unwrap(),
                r#"${1}[REDACTED BY RUST DRIVER]"#,
            ),
            (
                Regex::new(r#"(?i)(X-ClickHouse-Key:\s*)[^\r\n]+"#).unwrap(),
                r#"${1}[REDACTED BY RUST DRIVER]"#,
            ),
            (
                Regex::new(r#"(?i)(Authorization:\s*)[^\r\n]+"#).unwrap(),
                r#"${1}[REDACTED BY RUST DRIVER]"#,
            ),
        ]
    })
}

/// Sanitize any raw string by stripping or redacting sensitive passwords, JWT tokens, and secret headers.
pub fn sanitize_log_message(raw: &str) -> String {
    let mut result = raw.to_string();
    for (re, replacement) in sanitization_patterns() {
        result = re.replace_all(&result, *replacement).to_string();
    }
    result
}

/// A custom write adapter that sanitizes raw log bytes before flushing them to standard error (`stderr`).
#[derive(Clone, Copy, Debug)]
pub struct SanitizedStderr;

impl Write for SanitizedStderr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let input = String::from_utf8_lossy(buf);
        let sanitized = sanitize_log_message(&input);
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        handle.write_all(sanitized.as_bytes())?;
        handle.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

impl<'a> MakeWriter<'a> for SanitizedStderr {
    type Writer = SanitizedStderr;

    fn make_writer(&'a self) -> Self::Writer {
        SanitizedStderr
    }
}

/// Initialize sanitized logging directly to `io::stderr`.
pub fn init_stderr_logger() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,clickhouse_query_ext=debug"));

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(SanitizedStderr)
                .with_target(false)
                .without_time(),
        )
        .with(filter)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_log_message_json_password() {
        let raw = r#"Injecting payload: {"connectionId":101,"password":"SuperSecretPassword123!","jwtToken":null}"#;
        let cleaned = sanitize_log_message(raw);
        assert!(cleaned.contains(r#""password":"[REDACTED BY RUST DRIVER]""#));
        assert!(!cleaned.contains("SuperSecretPassword123!"));
    }

    #[test]
    fn test_sanitize_log_message_url_password() {
        let raw = "Connecting to https://admin:mySecretPass@localhost:8443/?password=SecretDbPassword&user=admin";
        let cleaned = sanitize_log_message(raw);
        assert!(cleaned.contains("password=[REDACTED BY RUST DRIVER]"));
        assert!(!cleaned.contains("SecretDbPassword"));
    }

    #[test]
    fn test_sanitize_log_message_http_headers() {
        let raw = "Request headers: X-ClickHouse-Key: abcdef1234567890\nAuthorization: Bearer jwt.token.here\nHost: localhost";
        let cleaned = sanitize_log_message(raw);
        assert!(cleaned.contains("X-ClickHouse-Key: [REDACTED BY RUST DRIVER]"));
        assert!(cleaned.contains("Authorization: [REDACTED BY RUST DRIVER]"));
        assert!(cleaned.contains("Host: localhost"));
        assert!(!cleaned.contains("abcdef1234567890"));
        assert!(!cleaned.contains("jwt.token.here"));
    }

    #[test]
    fn test_sanitize_log_message_benign() {
        let raw = "Starting clickhouse-query-ext Rust process sandbox driver...";
        assert_eq!(sanitize_log_message(raw), raw);
    }
}
