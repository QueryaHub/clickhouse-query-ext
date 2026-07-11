use crate::error::DriverError;
use crate::mapper::types::ColumnSchema;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryStatistics {
    pub rows_read: usize,
    pub bytes_read: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<ColumnSchema>,
    pub rows: Vec<Vec<Value>>,
    pub statistics: QueryStatistics,
}

/// Parses the output of ClickHouse `FORMAT JSONCompactEachRowWithNamesAndTypes`.
/// Line 1: JSON array of column names `["id", "amount"]`
/// Line 2: JSON array of ClickHouse data types `["UInt64", "Decimal(18, 4)"]`
/// Lines 3+: JSON arrays of row values `[18446744073709551615, 123.4500]`
/// Automatically normalizes row values according to `ColumnSchema::mapped_type` (e.g. converting 64-bit numbers and Decimals into JSON strings to prevent 53-bit float overflow in JS/Flutter).
pub fn parse_compact_output(
    output_text: &str,
    elapsed_ms: u64,
) -> Result<QueryResult, DriverError> {
    let lines: Vec<&str> = output_text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.is_empty() {
        return Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            statistics: QueryStatistics {
                rows_read: 0,
                bytes_read: output_text.len(),
                elapsed_ms,
            },
        });
    }

    if lines.len() < 2 {
        return Err(DriverError::Client(
            "Malformed JSONCompactEachRowWithNamesAndTypes output: missing names or types row"
                .to_string(),
        ));
    }

    let names: Vec<String> = serde_json::from_str(lines[0]).map_err(|e| {
        DriverError::Client(format!(
            "Failed to parse column names from ClickHouse output: {}",
            e
        ))
    })?;
    let types: Vec<String> = serde_json::from_str(lines[1]).map_err(|e| {
        DriverError::Client(format!(
            "Failed to parse column types from ClickHouse output: {}",
            e
        ))
    })?;

    if names.len() != types.len() {
        return Err(DriverError::Client(format!(
            "Column names count ({}) does not match types count ({})",
            names.len(),
            types.len()
        )));
    }

    let mut columns = Vec::with_capacity(names.len());
    for (name, ch_type) in names.into_iter().zip(types) {
        columns.push(ColumnSchema::new(name, ch_type));
    }

    let mut rows = Vec::with_capacity(lines.len().saturating_sub(2));
    for line in &lines[2..] {
        let mut raw_row: Vec<Value> = serde_json::from_str(line).map_err(|e| {
            DriverError::Client(format!(
                "Failed to parse data row JSON array from ClickHouse: {}",
                e
            ))
        })?;

        // Normalize values according to Querya schema mapped_type
        for (i, col) in columns.iter().enumerate() {
            if let Some(val) = raw_row.get_mut(i) {
                if val.is_null() {
                    continue;
                }
                match col.mapped_type {
                    "string" => {
                        // 64-bit/large integers and Decimals may arrive as JSON numbers from ClickHouse
                        if val.is_number() {
                            *val = Value::String(val.to_string());
                        }
                    }
                    "integer" => {
                        if let Some(s) = val.as_str()
                            && let Ok(n) = s.parse::<i64>()
                        {
                            *val = Value::Number(serde_json::Number::from(n));
                        }
                    }
                    _ => {}
                }
            }
        }
        rows.push(raw_row);
    }

    let rows_read = rows.len();
    Ok(QueryResult {
        columns,
        rows,
        statistics: QueryStatistics {
            rows_read,
            bytes_read: output_text.len(),
            elapsed_ms,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_compact_output_with_bigint_and_decimal() {
        let raw_output = r#"["id", "name", "amount", "is_active"]
["UInt64", "Nullable(String)", "Decimal(18, 4)", "Bool"]
[18446744073709551615, "Alice", 1234567.8901, true]
[102, null, 0.0000, false]"#;

        let res = parse_compact_output(raw_output, 15).unwrap();
        assert_eq!(res.columns.len(), 4);
        assert_eq!(res.columns[0].mapped_type, "string");
        assert_eq!(res.columns[1].mapped_type, "string");
        assert!(res.columns[1].is_nullable);
        assert_eq!(res.columns[2].mapped_type, "string");
        assert_eq!(res.columns[3].mapped_type, "boolean");

        assert_eq!(res.rows.len(), 2);
        // UInt64 must be string to protect 53-bit JS precision
        assert_eq!(res.rows[0][0], json!("18446744073709551615"));
        assert_eq!(res.rows[0][1], json!("Alice"));
        assert_eq!(res.rows[0][2], json!("1234567.8901"));
        assert_eq!(res.rows[0][3], json!(true));

        assert_eq!(res.rows[1][1], Value::Null);
        assert_eq!(res.statistics.rows_read, 2);
        assert_eq!(res.statistics.elapsed_ms, 15);
    }

    #[test]
    fn test_parse_compact_output_empty() {
        let res = parse_compact_output("", 5).unwrap();
        assert!(res.columns.is_empty());
        assert!(res.rows.is_empty());
        assert_eq!(res.statistics.rows_read, 0);
    }

    #[test]
    fn test_parse_compact_output_complex_types() {
        let raw_output = r#"["arr", "tup", "dt", "big_arr"]
["Array(Int32)", "Tuple(Int32, String)", "DateTime64(3)", "Array(UInt64)"]
[[10, 20, 30], [100, "foo"], "2026-07-11 12:34:56.789", [18446744073709551615, 42]]"#;

        let res = parse_compact_output(raw_output, 8).unwrap();
        assert_eq!(res.columns.len(), 4);
        assert_eq!(res.columns[0].mapped_type, "array");
        assert_eq!(res.columns[1].mapped_type, "json");
        assert_eq!(res.columns[2].mapped_type, "timestamp");
        assert_eq!(res.columns[3].mapped_type, "array");

        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0][0], json!([10, 20, 30]));
        assert_eq!(res.rows[0][1], json!([100, "foo"]));
        assert_eq!(res.rows[0][2], json!("2026-07-11 12:34:56.789"));
        // Check Array(UInt64) values
        assert_eq!(res.rows[0][3], json!([18446744073709551615u64, 42]));
    }
}
