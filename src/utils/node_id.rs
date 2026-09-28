//! Structured encoding/decoding for SDUI tree `nodeId` path segments.
//!
//! `nodeId` values are built by joining a fixed literal prefix (e.g. `"table"`,
//! `"db"`, `"col"`, `"part"`) with dynamic ClickHouse identifiers (database,
//! table, column and partition names) using `.` as the segment separator, e.g.
//! `table.analytics.events`. ClickHouse allows `.` inside backtick-quoted
//! identifiers and inside partition expression values, so a dynamic segment
//! can itself legally contain a literal `.`. A naive `nodeId.split('.')` would
//! then misalign every subsequent segment (CWE-20: Improper Input
//! Validation). To keep parsing unambiguous, every dynamic segment is escaped
//! before being joined, and decoded back after splitting.

/// Escapes a single dynamic `nodeId` segment so the `.` separator and the
/// escape character `~` used within it can never be confused with the path
/// separator when the full `nodeId` is later split on `.`.
pub fn encode_id_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '~' => out.push_str("~t"),
            '.' => out.push_str("~d"),
            _ => out.push(ch),
        }
    }
    out
}

/// Reverses `encode_id_segment`, restoring the original identifier.
/// A dangling `~` not followed by a recognized escape code is passed through
/// literally, since it cannot have been produced by `encode_id_segment`.
pub fn decode_id_segment(encoded: &str) -> String {
    let mut out = String::with_capacity(encoded.len());
    let mut chars = encoded.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.peek() {
                Some('d') => {
                    out.push('.');
                    chars.next();
                }
                Some('t') => {
                    out.push('~');
                    chars.next();
                }
                _ => out.push('~'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Splits a full `nodeId` on `.` and decodes each resulting segment.
/// The first segment (the fixed type prefix, e.g. `"table"`) is always a
/// literal constant chosen by the driver, never dynamic data, so decoding it
/// is a harmless no-op.
pub fn split_node_id(node_id: &str) -> Vec<String> {
    node_id.split('.').map(decode_id_segment).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_plain_identifier() {
        let encoded = encode_id_segment("analytics");
        assert_eq!(encoded, "analytics");
        assert_eq!(decode_id_segment(&encoded), "analytics");
    }

    #[test]
    fn test_roundtrip_dotted_identifier() {
        let encoded = encode_id_segment("my.table");
        assert_eq!(encoded, "my~dtable");
        assert_eq!(decode_id_segment(&encoded), "my.table");
    }

    #[test]
    fn test_roundtrip_tilde_and_dot_mixed() {
        let raw = "weird~name.with.dots~and~tildes";
        let encoded = encode_id_segment(raw);
        assert_eq!(decode_id_segment(&encoded), raw);
    }

    #[test]
    fn test_split_node_id_preserves_dotted_segments() {
        let node_id = format!(
            "table.{}.{}",
            encode_id_segment("my.db"),
            encode_id_segment("weird.table.name")
        );
        let parts = split_node_id(&node_id);
        assert_eq!(parts, vec!["table", "my.db", "weird.table.name"]);
    }

    #[test]
    fn test_split_node_id_plain_backward_compatible() {
        let parts = split_node_id("table.analytics.events");
        assert_eq!(parts, vec!["table", "analytics", "events"]);
    }

    #[test]
    fn test_dangling_tilde_passthrough() {
        assert_eq!(decode_id_segment("foo~"), "foo~");
        assert_eq!(decode_id_segment("foo~x"), "foo~x");
    }
}
