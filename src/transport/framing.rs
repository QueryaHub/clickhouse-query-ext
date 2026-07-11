use std::io::{self, Write};

/// Thread-safe / mutex-guarded NDJSON response writer to standard output.
/// Ensures the payload is strictly emitted as a single line (replacing any internal newlines)
/// followed by a single newline byte `\n`.
pub fn write_ndjson_stdout(payload: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    write_ndjson(&mut handle, payload)
}

/// Generic NDJSON writer that works with any `std::io::Write` sink (useful for testing).
pub fn write_ndjson<W: Write>(writer: &mut W, payload: &str) -> io::Result<()> {
    // If the JSON payload contains raw '\n' or '\r' bytes (not escaped inside strings),
    // replace them with spaces or strip to guarantee exact NDJSON framing.
    if payload.contains('\n') || payload.contains('\r') {
        let sanitized: String = payload
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        writer.write_all(sanitized.as_bytes())?;
    } else {
        writer.write_all(payload.as_bytes())?;
    }
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_ndjson_clean() {
        let mut buffer = Vec::new();
        let json_str = r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#;
        write_ndjson(&mut buffer, json_str).unwrap();
        assert_eq!(
            String::from_utf8(buffer).unwrap(),
            format!("{}\n", r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#)
        );
    }

    #[test]
    fn test_write_ndjson_sanitizes_newlines() {
        let mut buffer = Vec::new();
        let dirty = "{\"jsonrpc\":\"2.0\",\n\"id\":1,\r\n\"result\":\"ok\"}";
        write_ndjson(&mut buffer, dirty).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert_eq!(output.matches('\n').count(), 1);
        assert!(output.ends_with('\n'));
        assert_eq!(output, "{\"jsonrpc\":\"2.0\", \"id\":1,  \"result\":\"ok\"}\n");
    }
}
