use serde::Serialize;

/// Unwraps `Nullable(...)` and `LowCardinality(...)` wrappers recursively and returns `(inner_type_str, is_nullable)`.
pub fn unwrap_type_wrappers(mut ch_type: &str) -> (&str, bool) {
    let mut is_nullable = false;
    loop {
        let trimmed = ch_type.trim();
        if trimmed.starts_with("Nullable(") && trimmed.ends_with(')') {
            is_nullable = true;
            ch_type = &trimmed[9..trimmed.len() - 1];
        } else if trimmed.starts_with("LowCardinality(") && trimmed.ends_with(')') {
            ch_type = &trimmed[15..trimmed.len() - 1];
        } else {
            break;
        }
    }
    (ch_type.trim(), is_nullable)
}

/// Maps an unwrapped (clean) ClickHouse type string to the Querya Standard Schema primitive:
/// `string`, `integer`, `number`, `boolean`, `timestamp`, `json`, or `array`.
fn map_clean_clickhouse_type(inner: &str) -> &'static str {
    if inner.starts_with("Array(") && inner.ends_with(')') {
        return "array";
    }
    if inner.starts_with("Tuple(")
        || inner.starts_with("Map(")
        || inner.starts_with("Nested(")
        || inner == "JSON"
        || inner == "Object"
    {
        return "json";
    }
    if inner == "Bool" || inner == "Boolean" {
        return "boolean";
    }
    if inner == "Date" || inner == "Date32" || inner.starts_with("DateTime") {
        return "timestamp";
    }
    // 64-bit and larger integers MUST map to string to prevent JS / UI 53-bit float precision loss.
    if inner == "Int64"
        || inner == "UInt64"
        || inner == "Int128"
        || inner == "UInt128"
        || inner == "Int256"
        || inner == "UInt256"
    {
        return "string";
    }
    if inner.starts_with("Int") || inner.starts_with("UInt") || inner.starts_with("Interval") {
        return "integer";
    }
    if inner == "Float32" || inner == "Float64" || inner == "BFloat16" {
        return "number";
    }
    // Decimals require exact string representation to avoid floating point inaccuracies.
    if inner.starts_with("Decimal") {
        return "string";
    }
    // Default fallback: String, FixedString, UUID, IPv4, IPv6, Enum8, Enum16, etc.
    "string"
}

/// Maps any raw ClickHouse type (including wrappers) to the Querya Standard Schema primitive string.
pub fn map_clickhouse_type(ch_type: &str) -> &'static str {
    let (inner, _) = unwrap_type_wrappers(ch_type);
    map_clean_clickhouse_type(inner)
}

/// Structured metadata for a query result column conforming to Querya Standard Schema.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSchema {
    pub name: String,
    pub original_type: String,
    pub mapped_type: &'static str,
    pub is_nullable: bool,
}

impl ColumnSchema {
    pub fn new(name: impl Into<String>, original_type: impl Into<String>) -> Self {
        let orig = original_type.into();
        let (inner, is_nullable) = unwrap_type_wrappers(&orig);
        let mapped_type = map_clean_clickhouse_type(inner);
        Self {
            name: name.into(),
            original_type: orig,
            mapped_type,
            is_nullable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unwrap_type_wrappers() {
        assert_eq!(unwrap_type_wrappers("Nullable(String)"), ("String", true));
        assert_eq!(
            unwrap_type_wrappers("LowCardinality(String)"),
            ("String", false)
        );
        assert_eq!(
            unwrap_type_wrappers("Nullable(LowCardinality(Int32))"),
            ("Int32", true)
        );
        assert_eq!(
            unwrap_type_wrappers("LowCardinality(Nullable(FixedString(16)))"),
            ("FixedString(16)", true)
        );
        assert_eq!(unwrap_type_wrappers("UInt32"), ("UInt32", false));
    }

    #[test]
    fn test_map_integers() {
        assert_eq!(map_clickhouse_type("Int8"), "integer");
        assert_eq!(map_clickhouse_type("UInt32"), "integer");
        assert_eq!(map_clickhouse_type("Nullable(Int16)"), "integer");
        assert_eq!(map_clickhouse_type("IntervalDay"), "integer");

        // 64-bit and larger must be strings
        assert_eq!(map_clickhouse_type("Int64"), "string");
        assert_eq!(map_clickhouse_type("UInt64"), "string");
        assert_eq!(map_clickhouse_type("Nullable(Int128)"), "string");
        assert_eq!(map_clickhouse_type("UInt256"), "string");
    }

    #[test]
    fn test_map_numbers_and_decimals() {
        assert_eq!(map_clickhouse_type("Float32"), "number");
        assert_eq!(map_clickhouse_type("Nullable(Float64)"), "number");
        assert_eq!(map_clickhouse_type("Decimal(18, 4)"), "string");
        assert_eq!(map_clickhouse_type("Decimal64(8)"), "string");
    }

    #[test]
    fn test_map_timestamps_and_booleans() {
        assert_eq!(map_clickhouse_type("Date"), "timestamp");
        assert_eq!(map_clickhouse_type("Date32"), "timestamp");
        assert_eq!(map_clickhouse_type("DateTime('UTC')"), "timestamp");
        assert_eq!(
            map_clickhouse_type("DateTime64(3, 'Europe/Moscow')"),
            "timestamp"
        );
        assert_eq!(map_clickhouse_type("Bool"), "boolean");
        assert_eq!(map_clickhouse_type("Nullable(Boolean)"), "boolean");
    }

    #[test]
    fn test_map_complex_and_arrays() {
        assert_eq!(map_clickhouse_type("Array(Int32)"), "array");
        assert_eq!(map_clickhouse_type("Array(Nullable(String))"), "array");
        assert_eq!(map_clickhouse_type("Tuple(String, Int32)"), "json");
        assert_eq!(map_clickhouse_type("Map(String, String)"), "json");
        assert_eq!(map_clickhouse_type("JSON"), "json");
    }

    #[test]
    fn test_column_schema_generation() {
        let col = ColumnSchema::new("user_id", "Nullable(LowCardinality(UInt64))");
        assert_eq!(col.name, "user_id");
        assert_eq!(col.original_type, "Nullable(LowCardinality(UInt64))");
        assert_eq!(col.mapped_type, "string");
        assert!(col.is_nullable);
    }
}
