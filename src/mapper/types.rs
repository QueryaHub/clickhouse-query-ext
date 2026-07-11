pub fn map_clickhouse_type(ch_type: &str) -> &'static str {
    if ch_type.contains("Int64") || ch_type.contains("UInt64") || ch_type.contains("Decimal") {
        "string"
    } else if ch_type.contains("Int") || ch_type.contains("UInt") {
        "integer"
    } else if ch_type.contains("Float") {
        "number"
    } else if ch_type.contains("Date") || ch_type.contains("Time") {
        "timestamp"
    } else {
        "string"
    }
}
