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
    // replace them with spaces to guarantee exact NDJSON framing. Both are single-byte
    // ASCII code points, so this writes existing byte slices straight to `writer`
    // between them instead of collecting a sanitized copy of the whole payload onto
    // the heap first (issue #50) — a no-op payload (the common case) is written in
    // one `write_all` call, same as before.
    let bytes = payload.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' || b == b'\r' {
            writer.write_all(&bytes[start..i])?;
            writer.write_all(b" ")?;
            start = i + 1;
        }
    }
    writer.write_all(&bytes[start..])?;
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
        assert_eq!(
            output,
            "{\"jsonrpc\":\"2.0\", \"id\":1,  \"result\":\"ok\"}\n"
        );
    }

    #[test]
    fn test_write_ndjson_sanitizes_edge_positions_and_runs() {
        // Regression for issue #50: the byte-slice rewrite must handle a
        // newline as the very first/last byte and consecutive newlines
        // (an empty slice between them) without panicking or dropping bytes.
        let mut buffer = Vec::new();
        let dirty = "\nleading\r\rmiddle\n\ntrailing\n";
        write_ndjson(&mut buffer, dirty).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert_eq!(output, " leading  middle  trailing \n");
    }

    #[test]
    fn test_write_ndjson_sanitizes_large_payload() {
        // A payload well past any small-buffer fast path, to exercise the
        // byte-slice rewrite over a realistic multi-megabyte tabular result.
        let mut payload = "x".repeat(2 * 1024 * 1024);
        payload.push('\n');
        payload.push_str(&"y".repeat(1024));

        let mut buffer = Vec::new();
        write_ndjson(&mut buffer, &payload).unwrap();
        let output = String::from_utf8(buffer).unwrap();

        assert_eq!(output.len(), payload.len() + 1);
        assert_eq!(&output[..2 * 1024 * 1024], "x".repeat(2 * 1024 * 1024));
        assert_eq!(output.as_bytes()[2 * 1024 * 1024], b' ');
        assert!(output.ends_with(&format!("{}\n", "y".repeat(1024))));
    }

    #[test]
    fn test_write_ndjson_error_payload() {
        let mut buffer = Vec::new();
        let err_json =
            r#"{"jsonrpc":"2.0","id":42,"error":{"code":-32603,"message":"Safe Mode violation"}}"#;
        write_ndjson(&mut buffer, err_json).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert_eq!(output, format!("{}\n", err_json));
    }
}
