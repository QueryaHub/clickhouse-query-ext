//! ClickHouse SQL identifier and string literal escaping helpers.
//! Prevents SQL injection (CWE-89) in schema introspection queries and SDUI context actions.

/// Escapes a string for safe inclusion inside a ClickHouse SQL string literal (`'...'`).
///
/// Doubles all backslashes (`\`) and single quotes (`'`), ensuring malicious input cannot
/// break out of the string literal boundary.
pub fn escape_sql_string_literal(val: &str) -> String {
    val.replace('\\', "\\\\").replace('\'', "''")
}

/// Quotes and escapes a ClickHouse SQL identifier (database, table, partition, or column name).
///
/// Wraps the identifier in backticks (`` `...` ``), escaping any embedded backticks (``` ` ```)
/// by doubling them (`` `` ``) and backslashes by doubling them (`\\`).
pub fn quote_identifier(val: &str) -> String {
    let escaped = val.replace('\\', "\\\\").replace('`', "``");
    format!("`{}`", escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_sql_string_literal_benign() {
        assert_eq!(escape_sql_string_literal("analytics"), "analytics");
        assert_eq!(escape_sql_string_literal("events_2026"), "events_2026");
    }

    #[test]
    fn test_escape_sql_string_literal_injection() {
        assert_eq!(
            escape_sql_string_literal("test' OR 1=1 --"),
            "test'' OR 1=1 --"
        );
        assert_eq!(
            escape_sql_string_literal(r"test\' OR 'a'='a"),
            r"test\\'' OR ''a''=''a"
        );
        assert_eq!(escape_sql_string_literal("O'Reilly"), "O''Reilly");
    }

    #[test]
    fn test_quote_identifier_benign() {
        assert_eq!(quote_identifier("default"), "`default`");
        assert_eq!(quote_identifier("system.tables"), "`system.tables`");
    }

    #[test]
    fn test_quote_identifier_injection() {
        assert_eq!(quote_identifier("table`name"), "`table``name`");
        assert_eq!(
            quote_identifier("db`; DROP TABLE students; --"),
            "`db``; DROP TABLE students; --`"
        );
    }
}
