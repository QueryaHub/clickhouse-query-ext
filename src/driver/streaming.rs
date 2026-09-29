//! Incremental parsing of ClickHouse's `FORMAT JSONCompactEachRowWithNamesAndTypes`
//! HTTP response, row by row, as bytes arrive over the network.
//!
//! `db.query`'s previous implementation buffered the entire HTTP response body
//! into a single `String` (`reqwest::Response::text()`) before parsing began,
//! so a large analytical result (tens of megabytes of JSON lines) held both
//! the raw text and the parsed rows in memory simultaneously — a real OOM risk
//! under the 256 MB ClickHouse Sandbox ceiling. This module instead reads the
//! response as a line stream and stops pulling further bytes off the network
//! connection as soon as either the caller's `limit` or a safety byte cap is
//! reached (see issue #49).

use crate::error::DriverError;
use crate::mapper::row_compact::{
    QueryResult, QueryStatistics, parse_and_normalize_row, parse_columns,
};
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::io::StreamReader;

/// Independent of any client-supplied `limit`, stop consuming further row
/// bytes once this many have been read, so an unbounded query (no `limit`
/// sent) still can't grow the result past a safe ceiling.
const MAX_ROW_BYTES: usize = 200 * 1024 * 1024;

/// ClickHouse lines (JSON arrays of column values) are expected to be well
/// under this; it exists only to bound a single corrupt/adversarial line's
/// buffered length instead of growing unbounded.
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Reads a byte stream (a ClickHouse HTTP response body) as a stream of
/// lines and incrementally parses it as `FORMAT JSONCompactEachRowWithNamesAndTypes`
/// output, stopping (without reading the rest of the stream) once `limit`
/// rows have been parsed or the `MAX_ROW_BYTES` safety cap is reached.
pub async fn stream_compact_output<S>(
    byte_stream: S,
    limit: Option<usize>,
) -> Result<QueryResult, DriverError>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    let stream_reader = StreamReader::new(byte_stream);
    let mut lines = FramedRead::new(
        stream_reader,
        LinesCodec::new_with_max_length(MAX_LINE_BYTES),
    );

    let mut bytes_read: usize = 0;

    let Some(names_line) = lines.next().await.transpose().map_err(line_err)? else {
        return Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            statistics: QueryStatistics {
                rows_read: 0,
                bytes_read: 0,
                elapsed_ms: 0,
            },
            query_id: None,
            is_truncated: false,
        });
    };
    bytes_read += names_line.len() + 1;

    let Some(types_line) = lines.next().await.transpose().map_err(line_err)? else {
        return Err(DriverError::Client(
            "Malformed JSONCompactEachRowWithNamesAndTypes output: missing names or types row"
                .to_string(),
        ));
    };
    bytes_read += types_line.len() + 1;

    let columns = parse_columns(&names_line, &types_line)?;

    let mut rows = Vec::with_capacity(limit.unwrap_or(16).min(1024));
    let mut is_truncated = false;
    while let Some(line) = lines.next().await.transpose().map_err(line_err)? {
        bytes_read += line.len() + 1;

        if limit.is_some_and(|limit| rows.len() >= limit) || bytes_read > MAX_ROW_BYTES {
            is_truncated = true;
            break;
        }

        rows.push(parse_and_normalize_row(&line, &columns)?);
    }
    // Drop the frame reader (and the underlying HTTP connection) now instead of
    // reading any remaining body bytes off the network when we stopped early.
    drop(lines);

    let rows_read = rows.len();
    Ok(QueryResult {
        columns,
        rows,
        statistics: QueryStatistics {
            rows_read,
            bytes_read,
            elapsed_ms: 0,
        },
        query_id: None,
        is_truncated,
    })
}

fn line_err(e: tokio_util::codec::LinesCodecError) -> DriverError {
    DriverError::Client(format!("Failed to read ClickHouse response stream: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    /// Builds an in-memory byte stream out of a static string, split into
    /// arbitrary chunks, to exercise `stream_compact_output` without a live
    /// HTTP connection. Splitting mid-line (not just at newlines) verifies
    /// the line codec correctly reassembles frames split across chunks.
    fn chunked_stream(
        body: &'static str,
        chunk_size: usize,
    ) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
        let chunks: Vec<Result<Bytes, std::io::Error>> = body
            .as_bytes()
            .chunks(chunk_size.max(1))
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        stream::iter(chunks)
    }

    #[tokio::test]
    async fn test_stream_compact_output_parses_rows() {
        let body = "[\"id\"]\n[\"UInt64\"]\n[1]\n[2]\n[3]\n";
        let result = stream_compact_output(chunked_stream(body, 1024), None)
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 3);
        assert!(!result.is_truncated);
    }

    #[tokio::test]
    async fn test_stream_compact_output_reassembles_lines_split_across_chunks() {
        let body = "[\"id\"]\n[\"UInt64\"]\n[1]\n[2]\n[3]\n";
        // Force byte-at-a-time delivery so every line boundary falls mid-chunk.
        let result = stream_compact_output(chunked_stream(body, 1), None)
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 3);
    }

    #[tokio::test]
    async fn test_stream_compact_output_enforces_limit() {
        let body = "[\"id\"]\n[\"UInt64\"]\n[1]\n[2]\n[3]\n";
        let result = stream_compact_output(chunked_stream(body, 1024), Some(2))
            .await
            .unwrap();
        assert_eq!(result.rows.len(), 2);
        assert!(result.is_truncated);
    }

    #[tokio::test]
    async fn test_stream_compact_output_empty_body() {
        let result = stream_compact_output(chunked_stream("", 1024), None)
            .await
            .unwrap();
        assert!(result.rows.is_empty());
        assert!(result.columns.is_empty());
    }

    #[tokio::test]
    async fn test_stream_compact_output_missing_types_row() {
        let body = "[\"id\"]\n";
        let err = stream_compact_output(chunked_stream(body, 1024), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing names or types row"));
    }
}
