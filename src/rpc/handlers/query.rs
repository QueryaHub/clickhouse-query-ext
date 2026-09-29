use crate::driver::pool::ConnectionPool;
use crate::error::DriverError;
use crate::mapper::row_compact::parse_compact_output;
use crate::utils::secret_guard::ConnectionSecretsPool;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tracing::info;
use url::Url;

static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryParams {
    pub connection_id: u64,
    pub sql: String,
    pub query_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelParams {
    pub connection_id: u64,
    pub query_id: String,
    #[serde(default = "default_true")]
    pub sync: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillMutationParams {
    pub connection_id: u64,
    pub mutation_id: String,
    #[serde(default = "default_true")]
    pub sync: bool,
}

fn default_true() -> bool {
    true
}

fn generate_query_id(connection_id: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("querya-job-{}-{}-{}", connection_id, now, seq)
}

/// Validates that an identifier token (queryId or mutationId) contains only safe characters.
pub fn validate_query_or_mutation_id(id: &str, field_name: &str) -> Result<(), DriverError> {
    if id.is_empty() || id.len() > 256 {
        return Err(DriverError::Client(format!(
            "Invalid {} length: must be between 1 and 256 characters",
            field_name
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(DriverError::Client(format!(
            "Invalid {} format: '{}' contains disallowed characters (allowed: [a-zA-Z0-9_.-])",
            field_name, id
        )));
    }
    Ok(())
}

/// Zero-allocation, lazy SQL token scanner used by the Safe Mode precheck and
/// query classification.
///
/// Yields whitespace-separated tokens as slices of the original text, skipping
/// `-- line` and `/* block */` comments. Quoted literals (`'...'`, `"..."`,
/// with `\\` and doubled-quote escapes) are kept inside a single token, so their
/// contents can never be mistaken for keywords or comment markers. Scanning
/// stops as soon as the caller stops pulling tokens.
struct SqlTokens<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> SqlTokens<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn char_at(&self, i: usize) -> char {
        self.src[i..].chars().next().unwrap_or('\0')
    }
}

impl<'a> Iterator for SqlTokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let b = self.src.as_bytes();
        let n = b.len();
        let mut i = self.pos;

        // Skip whitespace and comments before the token.
        loop {
            if i >= n {
                self.pos = n;
                return None;
            }
            if b[i] == b'-' && b.get(i + 1) == Some(&b'-') {
                i = self.src[i..].find('\n').map_or(n, |off| i + off);
            } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                i = self.src[i + 2..]
                    .find("*/")
                    .map_or(n, |off| i + 2 + off + 2);
            } else {
                let c = self.char_at(i);
                if !c.is_whitespace() {
                    break;
                }
                i += c.len_utf8();
            }
        }

        let start = i;
        while i < n {
            match b[i] {
                b'-' if b.get(i + 1) == Some(&b'-') => break,
                b'/' if b.get(i + 1) == Some(&b'*') => break,
                q @ (b'\'' | b'"') => {
                    i += 1;
                    while i < n {
                        if b[i] == b'\\' {
                            i += 1;
                            if i < n {
                                i += self.char_at(i).len_utf8();
                            }
                        } else if b[i] == q {
                            i += 1;
                            if b.get(i) == Some(&q) {
                                i += 1;
                            } else {
                                break;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }
                _ => {
                    let c = self.char_at(i);
                    if c.is_whitespace() {
                        break;
                    }
                    i += c.len_utf8();
                }
            }
        }

        let i = i.min(n);
        self.pos = i;
        Some(&self.src[start..i])
    }
}

/// Case-insensitive (ASCII) check that the first SQL token starts with `prefix`.
fn first_token_starts_with(sql: &str, prefix: &str) -> bool {
    SqlTokens::new(sql).next().is_some_and(|t| {
        t.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

const KW_DROP: u16 = 1 << 0;
const KW_TRUNCATE: u16 = 1 << 1;
const KW_DELETE: u16 = 1 << 2;
const KW_UPDATE: u16 = 1 << 3;
const KW_ALTER: u16 = 1 << 4;
const KW_TABLE: u16 = 1 << 5;
const KW_DATABASE: u16 = 1 << 6;
const KW_INSERT: u16 = 1 << 7;
const KW_INTO: u16 = 1 << 8;
const KW_CREATE: u16 = 1 << 9;
const KW_VALUES: u16 = 1 << 10;
const KW_SELECT: u16 = 1 << 11;
/// A mutating `ALTER TABLE` action keyword (parentheses ignored).
const KW_ALTER_ACTION: u16 = 1 << 12;

/// Single pass over all tokens, recording which dangerous keywords appear.
fn scan_keywords(sql: &str) -> u16 {
    const EXACT: [(&str, u16); 12] = [
        ("DROP", KW_DROP),
        ("TRUNCATE", KW_TRUNCATE),
        ("DELETE", KW_DELETE),
        ("UPDATE", KW_UPDATE),
        ("ALTER", KW_ALTER),
        ("TABLE", KW_TABLE),
        ("DATABASE", KW_DATABASE),
        ("INSERT", KW_INSERT),
        ("INTO", KW_INTO),
        ("CREATE", KW_CREATE),
        ("VALUES", KW_VALUES),
        ("SELECT", KW_SELECT),
    ];
    const ALTER_ACTIONS: [&str; 9] = [
        "DROP", "DELETE", "UPDATE", "MODIFY", "REPLACE", "CLEAR", "FREEZE", "ATTACH", "DETACH",
    ];

    let mut flags = 0u16;
    for token in SqlTokens::new(sql) {
        for (kw, bit) in EXACT {
            if token.eq_ignore_ascii_case(kw) {
                flags |= bit;
            }
        }
        let clean = token.trim_matches(|c| c == '(' || c == ')');
        if ALTER_ACTIONS.iter().any(|a| clean.eq_ignore_ascii_case(a)) {
            flags |= KW_ALTER_ACTION;
        }
    }
    flags
}

/// Splits SQL text into individual statements separated by semicolon (`;`),
/// taking care not to split inside single/double quotes, backticks, or comments.
pub fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_single_comment = false;
    let mut in_multi_comment = false;
    let mut in_string = false;
    let mut string_quote = ' ';

    while let Some(c) = chars.next() {
        if in_single_comment {
            current.push(c);
            if c == '\n' {
                in_single_comment = false;
            }
        } else if in_multi_comment {
            current.push(c);
            if c == '*' && chars.peek() == Some(&'/') {
                current.push(chars.next().unwrap());
                in_multi_comment = false;
            }
        } else if in_string {
            current.push(c);
            if c == '\\' {
                if let Some(next_c) = chars.next() {
                    current.push(next_c);
                }
            } else if c == string_quote {
                if chars.peek() == Some(&string_quote) {
                    current.push(chars.next().unwrap());
                } else {
                    in_string = false;
                }
            }
        } else if c == '-' && chars.peek() == Some(&'-') {
            current.push(c);
            current.push(chars.next().unwrap());
            in_single_comment = true;
        } else if c == '/' && chars.peek() == Some(&'*') {
            current.push(c);
            current.push(chars.next().unwrap());
            in_multi_comment = true;
        } else if c == '\'' || c == '`' || c == '"' {
            in_string = true;
            string_quote = c;
            current.push(c);
        } else if c == ';' {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                statements.push(trimmed.to_string());
            }
            current.clear();
        } else {
            current.push(c);
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }

    statements
}

/// Checks an individual SQL statement for dangerous/destructive operations in Safe Mode.
fn check_single_statement_for_safe_mode(statement_sql: &str) -> Result<(), DriverError> {
    let mut tokens = SqlTokens::new(statement_sql);
    let Some(first) = tokens.next() else {
        return Ok(());
    };
    let first = first.trim_start_matches('(');
    let is_first = |kw: &str| first.eq_ignore_ascii_case(kw);

    // Fast path: read-only leading commands need no further scanning.
    const READ_ONLY: [&str; 8] = [
        "SELECT", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "EXISTS", "CHECK", "WITH",
    ];
    if READ_ONLY.iter().any(|kw| is_first(kw)) {
        return Ok(());
    }
    if is_first("UPDATE") {
        return Err(safe_mode_violation());
    }

    let second = tokens.next().unwrap_or("").trim_start_matches('(');
    let third = tokens.next().unwrap_or("").trim_start_matches('(');
    let second_is = |kw: &str| second.eq_ignore_ascii_case(kw);
    let third_is = |kw: &str| third.eq_ignore_ascii_case(kw);
    let is_object_kind = || {
        second_is("DATABASE") || second_is("TABLE") || second_is("VIEW") || second_is("DICTIONARY")
    };

    let is_dangerous = if is_first("DROP") || is_first("CREATE") {
        is_object_kind()
    } else if is_first("TRUNCATE") {
        second_is("TABLE")
    } else if is_first("ALTER") {
        second_is("TABLE") && scan_keywords(statement_sql) & KW_ALTER_ACTION != 0
    } else if is_first("INSERT") {
        second_is("INTO")
            || third_is("INTO")
            || scan_keywords(statement_sql) & (KW_VALUES | KW_SELECT) != 0
    } else if is_first("DELETE") {
        second_is("FROM") || third_is("FROM")
    } else if is_first("RENAME") {
        second_is("TABLE") || second_is("DATABASE")
    } else if is_first("ATTACH") || is_first("DETACH") {
        second_is("TABLE") || second_is("PARTITION")
    } else {
        let f = scan_keywords(statement_sql);
        f & (KW_DROP | KW_TRUNCATE | KW_DELETE | KW_UPDATE) != 0
            || (f & KW_ALTER != 0 && f & KW_TABLE != 0)
            || (f & KW_INSERT != 0 && f & KW_INTO != 0)
            || (f & KW_CREATE != 0 && f & (KW_TABLE | KW_DATABASE) != 0)
    };

    if is_dangerous {
        return Err(safe_mode_violation());
    }
    Ok(())
}

fn safe_mode_violation() -> DriverError {
    DriverError::SafeModeViolation(
        "Operation blocked by Safe Mode: write or destructive queries are forbidden in analytical read-only mode".to_string(),
    )
}

/// Pre-checks AST/SQL syntax in Safe Mode (`readonly = true`) before network roundtrip,
/// evaluating all statements in multi-statement queries.
fn enforce_safe_mode_precheck(sql: &str) -> Result<(), DriverError> {
    let statements = split_sql_statements(sql);
    if statements.is_empty() {
        return check_single_statement_for_safe_mode(sql);
    }
    for stmt in &statements {
        check_single_statement_for_safe_mode(stmt)?;
    }
    Ok(())
}

/// Handler for `db.query` and `db.execute`.
/// Enforces Safe Mode AST pre-checks, injects `FORMAT JSONCompactEachRowWithNamesAndTypes` when needed,
/// streams results from ClickHouse via HTTP POST, and normalizes output types using `row_compact`.
pub async fn handle_query(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.query requires connectionId and sql".to_string(),
        data: None,
    })?;

    let query_params: QueryParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed query parameters: {}", e),
            data: None,
        })?;

    let client = ConnectionPool::global()
        .get(query_params.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(query_params.connection_id))?;

    // 1. Safe Mode check
    if client.readonly {
        enforce_safe_mode_precheck(&query_params.sql)?;
    }

    let trimmed_sql = query_params.sql.trim();
    let upper_sql = trimmed_sql.to_uppercase();
    // Classify on the comment-stripped statement so a leading `-- comment` or
    // `/* comment */` doesn't hide the real starting keyword, and recognize
    // `WITH ...` CTE queries as tabular too.
    let is_tabular_query = ["SELECT", "SHOW", "DESCRIBE", "EXPLAIN", "WITH"]
        .iter()
        .any(|kw| first_token_starts_with(trimmed_sql, kw));
    let has_format_clause = SqlTokens::new(trimmed_sql).any(|t| t.eq_ignore_ascii_case("FORMAT"));

    let sql_to_run = if is_tabular_query && !has_format_clause {
        // FORMAT must precede the statement-terminating `;` in ClickHouse's
        // grammar, so strip any trailing semicolon before appending it.
        let sql_no_trailing_semicolon =
            trimmed_sql.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
        format!(
            "{}\nFORMAT JSONCompactEachRowWithNamesAndTypes",
            sql_no_trailing_semicolon
        )
    } else {
        trimmed_sql.to_string()
    };

    let actual_query_id = match &query_params.query_id {
        Some(qid) if !qid.is_empty() => {
            validate_query_or_mutation_id(qid, "queryId")?;
            qid.clone()
        }
        _ => generate_query_id(query_params.connection_id),
    };

    info!(
        "Executing SQL on connectionId={} (query_id='{}', readonly={}): {}...",
        query_params.connection_id,
        actual_query_id,
        client.readonly,
        trimmed_sql.lines().next().unwrap_or("")
    );

    let start_time = Instant::now();

    // 2. Mock handler for unit tests
    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        if is_tabular_query {
            let mock_output = r#"["id", "event_name", "user_id"]
["UInt64", "String", "Nullable(UInt64)"]
[18446744073709551615, "page_view", 42]
[100, "click", null]"#;
            let mut result = parse_compact_output(
                mock_output,
                start_time.elapsed().as_millis() as u64,
                query_params.limit,
            )?;
            result.query_id = Some(actual_query_id);
            return Ok(serde_json::to_value(result)?);
        } else {
            return Ok(build_non_tabular_result(
                &upper_sql,
                start_time.elapsed().as_millis() as u64,
                0,
                &actual_query_id,
            ));
        }
    }

    // 3. Real ClickHouse HTTP request
    let actual_query_id_for_url = actual_query_id.clone();
    if is_tabular_query {
        // Stream and parse the response row-by-row instead of buffering the
        // whole body into a String first, bounding peak memory for large
        // analytical result sets (issue #49).
        let response = client
            .post_sql_response(&sql_to_run, |url| {
                url.query_pairs_mut()
                    .append_pair("query_id", &actual_query_id_for_url);
            })
            .await?;
        let byte_stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(std::io::Error::other));
        let mut result =
            crate::driver::streaming::stream_compact_output(byte_stream, query_params.limit)
                .await?;
        result.statistics.elapsed_ms = start_time.elapsed().as_millis() as u64;
        result.query_id = Some(actual_query_id);
        Ok(serde_json::to_value(result)?)
    } else {
        let text = client
            .post_sql(&sql_to_run, |url| {
                url.query_pairs_mut()
                    .append_pair("query_id", &actual_query_id_for_url);
            })
            .await?;
        let elapsed = start_time.elapsed().as_millis() as u64;
        Ok(build_non_tabular_result(
            &upper_sql,
            elapsed,
            text.len(),
            &actual_query_id,
        ))
    }
}

fn build_non_tabular_result(
    upper_sql: &str,
    elapsed: u64,
    bytes_read: usize,
    query_id: &str,
) -> Value {
    let operation = if upper_sql.starts_with("OPTIMIZE TABLE") {
        "optimize"
    } else if upper_sql.starts_with("ALTER ") {
        "alter"
    } else if upper_sql.starts_with("INSERT ") {
        "insert"
    } else if upper_sql.starts_with("KILL ") {
        "kill"
    } else {
        "execute"
    };

    let status_msg = if operation == "optimize" {
        if upper_sql.contains("DEDUPLICATE") {
            format!(
                "Table deduplication completed successfully in {}ms",
                elapsed
            )
        } else {
            format!(
                "Table optimization (FINAL) completed successfully in {}ms",
                elapsed
            )
        }
    } else if operation == "alter" && upper_sql.contains("PARTITION") {
        if upper_sql.contains("FREEZE PARTITION") {
            format!(
                "Partition frozen successfully in {}ms (backup created in /shadow/)",
                elapsed
            )
        } else if upper_sql.contains("DROP PARTITION") {
            format!("Partition dropped successfully in {}ms", elapsed)
        } else if upper_sql.contains("DETACH PARTITION") {
            format!("Partition detached successfully in {}ms", elapsed)
        } else if upper_sql.contains("ATTACH PARTITION") {
            format!("Partition attached successfully in {}ms", elapsed)
        } else {
            format!(
                "Partition operation completed successfully in {}ms",
                elapsed
            )
        }
    } else if operation == "kill" {
        if upper_sql.contains("MUTATION") {
            format!("Mutation(s) killed successfully in {}ms", elapsed)
        } else {
            format!("Query/process(es) killed successfully in {}ms", elapsed)
        }
    } else {
        format!("Command completed successfully in {}ms", elapsed)
    };

    json!({
        "queryId": query_id,
        "status": "completed",
        "operation": operation,
        "message": status_msg,
        "columns": [],
        "rows": [],
        "statistics": {
            "rowsRead": 0,
            "bytesRead": bytes_read,
            "elapsedMs": elapsed
        }
    })
}

/// Handler for `db.cancelQuery`.
/// Sends `KILL QUERY WHERE query_id = '...' ASYNC` to abort running queries without dropping the connection.
pub async fn handle_cancel(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.cancelQuery requires connectionId and queryId".to_string(),
        data: None,
    })?;

    let cancel_params: CancelParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed cancelQuery parameters: {}", e),
            data: None,
        })?;

    let client = ConnectionPool::global()
        .get(cancel_params.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(cancel_params.connection_id))?;

    validate_query_or_mutation_id(&cancel_params.query_id, "queryId")?;

    info!(
        "Cancelling queryId={} on connectionId={}",
        cancel_params.query_id, cancel_params.connection_id
    );

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        return Ok(json!({ "ok": true }));
    }

    let escaped_query_id =
        crate::utils::sql_escape::escape_sql_string_literal(&cancel_params.query_id);
    let sync_kw = if cancel_params.sync { "SYNC" } else { "ASYNC" };
    let mut url = Url::parse(&client.base_url)?;
    url.query_pairs_mut()
        .append_pair("database", &client.database)
        .append_pair(
            "query",
            &format!(
                "KILL QUERY WHERE query_id = '{}' {}",
                escaped_query_id, sync_kw
            ),
        );

    let mut req = client.http_client.post(url);
    if let Some(secrets) = ConnectionSecretsPool::global().get(client.connection_id) {
        if let Some(jwt) = secrets.expose_jwt_token() {
            req = req.header("Authorization", format!("Bearer {}", jwt));
        } else if let Some(pass) = secrets.expose_password() {
            req = req
                .header("X-ClickHouse-User", &client.user)
                .header("X-ClickHouse-Key", pass);
        }
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(DriverError::Client(format!(
            "Failed to cancel query: {}",
            text
        )));
    }

    Ok(json!({ "ok": true }))
}

/// Handler for `db.killMutation`.
/// Sends `KILL MUTATION WHERE mutation_id = '...' ASYNC/SYNC` to abort active mutations.
pub async fn handle_kill_mutation(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.killMutation requires connectionId and mutationId".to_string(),
        data: None,
    })?;

    let kill_params: KillMutationParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed killMutation parameters: {}", e),
            data: None,
        })?;

    let client = ConnectionPool::global()
        .get(kill_params.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(kill_params.connection_id))?;

    validate_query_or_mutation_id(&kill_params.mutation_id, "mutationId")?;

    info!(
        "Killing mutationId={} on connectionId={}",
        kill_params.mutation_id, kill_params.connection_id
    );

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        return Ok(json!({ "ok": true }));
    }

    let escaped_mutation_id =
        crate::utils::sql_escape::escape_sql_string_literal(&kill_params.mutation_id);
    let sync_kw = if kill_params.sync { "SYNC" } else { "ASYNC" };
    let mut url = Url::parse(&client.base_url)?;
    url.query_pairs_mut()
        .append_pair("database", &client.database)
        .append_pair(
            "query",
            &format!(
                "KILL MUTATION WHERE mutation_id = '{}' {}",
                escaped_mutation_id, sync_kw
            ),
        );

    let mut req = client.http_client.post(url);
    if let Some(secrets) = ConnectionSecretsPool::global().get(client.connection_id) {
        if let Some(jwt) = secrets.expose_jwt_token() {
            req = req.header("Authorization", format!("Bearer {}", jwt));
        } else if let Some(pass) = secrets.expose_password() {
            req = req
                .header("X-ClickHouse-User", &client.user)
                .header("X-ClickHouse-Key", pass);
        }
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(DriverError::Client(format!(
            "Failed to kill mutation: {}",
            text
        )));
    }

    Ok(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::client::{ClickHouseClient, ConnectParams};

    #[test]
    fn test_safe_mode_precheck_rejections() {
        assert!(enforce_safe_mode_precheck("SELECT * FROM events").is_ok());
        assert!(enforce_safe_mode_precheck("SHOW TABLES").is_ok());
        assert!(enforce_safe_mode_precheck("DESCRIBE TABLE events").is_ok());
        assert!(
            enforce_safe_mode_precheck("-- analytical query\nSELECT count() FROM logs").is_ok()
        );

        let drop_db = enforce_safe_mode_precheck("DROP DATABASE prod").unwrap_err();
        assert_eq!(drop_db.to_rpc_code(), -32603);
        assert!(
            drop_db
                .to_string()
                .contains("Operation blocked by Safe Mode")
        );

        assert!(enforce_safe_mode_precheck("DROP TABLE events").is_err());
        assert!(enforce_safe_mode_precheck("TRUNCATE TABLE logs").is_err());
        assert!(enforce_safe_mode_precheck("ALTER TABLE events DROP COLUMN age").is_err());
        assert!(
            enforce_safe_mode_precheck(
                "/* multiline\n comment */\nALTER TABLE events DROP COLUMN age"
            )
            .is_err()
        );
        assert!(enforce_safe_mode_precheck("-- comment\nDROP TABLE logs").is_err());
        assert!(enforce_safe_mode_precheck("INSERT INTO events VALUES (1, 'test')").is_err());
        assert!(enforce_safe_mode_precheck("DELETE FROM events WHERE id = 1").is_err());
        assert!(enforce_safe_mode_precheck("CREATE TABLE new_tbl (id Int32)").is_err());
    }

    #[test]
    fn test_split_sql_statements() {
        assert_eq!(
            split_sql_statements("SELECT 1; SELECT 2"),
            vec!["SELECT 1", "SELECT 2"]
        );
        assert_eq!(
            split_sql_statements("SELECT 'hello; world'; SELECT 2;"),
            vec!["SELECT 'hello; world'", "SELECT 2"]
        );
        assert_eq!(
            split_sql_statements("SELECT `col;name` FROM t; SELECT 3"),
            vec!["SELECT `col;name` FROM t", "SELECT 3"]
        );
        assert_eq!(
            split_sql_statements("SELECT 1 -- ; comment\n; SELECT 2"),
            vec!["SELECT 1 -- ; comment", "SELECT 2"]
        );
        assert_eq!(
            split_sql_statements("SELECT 1 /* ; block comment */ ; SELECT 2"),
            vec!["SELECT 1 /* ; block comment */", "SELECT 2"]
        );
        assert_eq!(split_sql_statements(";; ;"), Vec::<String>::new());
    }

    #[test]
    fn test_split_sql_statements_handles_escaped_and_doubled_quotes() {
        // Regression for issue #59: a backslash-escaped quote or a doubled quote
        // inside a string literal must not be treated as the string's closing quote.
        assert_eq!(
            split_sql_statements("SELECT 'Customer\\'s notes'; SELECT 2"),
            vec!["SELECT 'Customer\\'s notes'", "SELECT 2"]
        );
        assert_eq!(
            split_sql_statements("SELECT 'Don''t drop; table'; SELECT 2"),
            vec!["SELECT 'Don''t drop; table'", "SELECT 2"]
        );
    }

    #[test]
    fn test_sql_tokens_handles_escaped_and_doubled_quotes() {
        // Regression for issue #59: an escaped quote (`\'`) must not prematurely
        // close a string literal and expose a following `--` as a real comment.
        let escaped: Vec<&str> =
            SqlTokens::new("SELECT 'Customer\\'s notes -- internal' FROM feedback").collect();
        assert_eq!(escaped.first(), Some(&"SELECT"));
        assert_eq!(escaped.last(), Some(&"feedback"));
        assert!(escaped.contains(&"FROM"));

        // A doubled quote (`''`, the SQL-standard escape) must not close the
        // string either, so the literal's content never leaks out as keywords.
        let doubled: Vec<&str> = SqlTokens::new("SELECT 'Don''t drop table' FROM logs").collect();
        assert_eq!(doubled.first(), Some(&"SELECT"));
        assert_eq!(doubled.last(), Some(&"logs"));
        assert!(!doubled.iter().any(|t| t.eq_ignore_ascii_case("DROP")));
    }

    #[test]
    fn test_safe_mode_precheck_is_case_insensitive_and_comment_aware() {
        assert!(enforce_safe_mode_precheck("-- hi\n/* x */ drop table t").is_err());
        assert!(enforce_safe_mode_precheck("Alter Table t Delete where 1").is_err());
        assert!(enforce_safe_mode_precheck("insert into t values (1)").is_err());
        assert!(enforce_safe_mode_precheck("select 'drop table t' -- drop table x").is_ok());
        assert!(enforce_safe_mode_precheck("  \n  ").is_ok());
    }

    #[test]
    fn test_sql_tokens_skips_comments_and_whitespace() {
        let tokens: Vec<&str> =
            SqlTokens::new("  -- lead\n/* block */ WITH/**/x AS (SELECT 1) -- tail").collect();
        assert_eq!(tokens, ["WITH", "x", "AS", "(SELECT", "1)"]);
        assert!(SqlTokens::new("-- only comment").next().is_none());
        assert!(SqlTokens::new("/* unterminated").next().is_none());
        assert!(first_token_starts_with("  /* c */ select 1", "SELECT"));
        assert!(!first_token_starts_with("(select 1)", "SELECT"));
    }

    #[test]
    fn test_safe_mode_multi_statement_bypass_prevention() {
        // Multi-statement bypass attempts from Issue #54
        assert!(
            enforce_safe_mode_precheck(
                "SELECT 1; INSERT INTO telemetry VALUES ('compromised', now());"
            )
            .is_err()
        );
        assert!(enforce_safe_mode_precheck("SELECT 1; DROP TABLE events;").is_err());
        assert!(enforce_safe_mode_precheck("SELECT 1; TRUNCATE TABLE events;").is_err());
        assert!(
            enforce_safe_mode_precheck("SELECT 1; ALTER TABLE events DROP COLUMN user_id;")
                .is_err()
        );
        assert!(enforce_safe_mode_precheck("SELECT 1; DELETE FROM events WHERE 1=1;").is_err());
        assert!(enforce_safe_mode_precheck("SELECT 1; UPDATE events SET id = 2;").is_err());
        assert!(enforce_safe_mode_precheck("SELECT 1; CREATE TABLE new_tbl (id Int32);").is_err());

        // Benign multi-statement queries
        assert!(enforce_safe_mode_precheck("SELECT 1; SELECT 2; SHOW TABLES;").is_ok());
        assert!(enforce_safe_mode_precheck("SELECT ';'; SELECT 'DROP TABLE in string';").is_ok());

        // Regression for issue #59: an escaped or doubled quote inside a string
        // literal must not desynchronize comment/string tracking for the rest of
        // the query, which would otherwise falsely block a benign query or hide a
        // dangerous statement behind a fake comment.
        assert!(
            enforce_safe_mode_precheck("SELECT 'Customer\\'s notes -- internal' FROM feedback")
                .is_ok()
        );
        assert!(enforce_safe_mode_precheck("SELECT 'Don''t drop table' FROM logs").is_ok());
    }

    #[tokio::test]
    async fn test_handle_query_blocked_by_safe_mode_multi_statement() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 223,
            connection_string: Some("mock://localhost:8123/default?readonly=1".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let query_params = json!({
            "connectionId": 223,
            "sql": "SELECT 1; INSERT INTO telemetry VALUES ('compromised', now());"
        });

        let err = handle_query(Some(query_params)).await.unwrap_err();
        assert!(matches!(err, DriverError::SafeModeViolation(_)));

        ConnectionPool::global().remove(223);
    }

    #[tokio::test]
    async fn test_handle_query_in_mock_mode() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 111,
            connection_string: Some("mock://localhost:8123/default?readonly=1".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let query_params = json!({
            "connectionId": 111,
            "sql": "SELECT id, event_name, user_id FROM events",
            "queryId": "query-mock-1"
        });

        let res = handle_query(Some(query_params)).await.unwrap();
        assert_eq!(res["columns"].as_array().unwrap().len(), 3);
        assert_eq!(res["rows"].as_array().unwrap().len(), 2);
        assert_eq!(res["rows"][0][0], json!("18446744073709551615"));
        assert_eq!(res["rows"][0][1], json!("page_view"));

        ConnectionPool::global().remove(111);
    }

    #[tokio::test]
    async fn test_handle_query_enforces_limit() {
        // Regression for issue #47: a `limit` in the request must actually
        // truncate the parsed rows instead of being silently ignored.
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 113,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let query_params = json!({
            "connectionId": 113,
            "sql": "SELECT id, event_name, user_id FROM events",
            "limit": 1
        });

        let res = handle_query(Some(query_params)).await.unwrap();
        assert_eq!(res["rows"].as_array().unwrap().len(), 1);
        assert_eq!(res["statistics"]["rowsRead"], 1);

        ConnectionPool::global().remove(113);
    }

    #[tokio::test]
    async fn test_handle_query_tabular_detection_edge_cases() {
        // Regression for issue #58: leading comments, CTE `WITH` queries and a
        // trailing `;` must all still be classified as tabular queries.
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 112,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        for sql in [
            "-- Top 10 users\nSELECT id, event_name, user_id FROM events",
            "/* block comment */ SELECT id, event_name, user_id FROM events",
            "WITH x AS (SELECT 1) SELECT id, event_name, user_id FROM events",
            "SELECT id, event_name, user_id FROM events;",
            "SELECT id, event_name, user_id FROM events;   ",
        ] {
            let query_params = json!({ "connectionId": 112, "sql": sql });
            let res = handle_query(Some(query_params)).await.unwrap();
            assert_eq!(
                res["columns"].as_array().unwrap().len(),
                3,
                "expected tabular result for: {}",
                sql
            );
            assert_eq!(res["rows"].as_array().unwrap().len(), 2);
        }

        ConnectionPool::global().remove(112);
    }

    #[test]
    fn test_tabular_query_trailing_semicolon_format_placement() {
        // The FORMAT clause must be appended before any trailing `;`, never after.
        let trimmed_sql = "SELECT 1;";
        assert!(first_token_starts_with(trimmed_sql, "SELECT"));
        let sql_no_trailing_semicolon =
            trimmed_sql.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
        let sql_to_run = format!(
            "{}\nFORMAT JSONCompactEachRowWithNamesAndTypes",
            sql_no_trailing_semicolon
        );
        assert_eq!(
            sql_to_run,
            "SELECT 1\nFORMAT JSONCompactEachRowWithNamesAndTypes"
        );
    }

    #[tokio::test]
    async fn test_handle_query_blocked_by_safe_mode() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 222,
            connection_string: Some("mock://localhost:8123/default?readonly=1".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let query_params = json!({
            "connectionId": 222,
            "sql": "DROP TABLE events"
        });

        let err = handle_query(Some(query_params)).await.unwrap_err();
        assert!(matches!(err, DriverError::SafeModeViolation(_)));

        ConnectionPool::global().remove(222);
    }

    #[tokio::test]
    async fn test_handle_cancel_sync_and_async() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 333,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        // SYNC cancel (default)
        let cancel_params = json!({
            "connectionId": 333,
            "queryId": "query-to-cancel-123"
        });
        let res = handle_cancel(Some(cancel_params)).await.unwrap();
        assert_eq!(res, json!({ "ok": true }));

        // ASYNC cancel
        let cancel_params_async = json!({
            "connectionId": 333,
            "queryId": "query-to-cancel-456",
            "sync": false
        });
        let res_async = handle_cancel(Some(cancel_params_async)).await.unwrap();
        assert_eq!(res_async, json!({ "ok": true }));

        ConnectionPool::global().remove(333);
    }

    #[tokio::test]
    async fn test_handle_query_auto_generates_query_id() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 444,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let query_params = json!({
            "connectionId": 444,
            "sql": "SELECT 1"
        });

        let res = handle_query(Some(query_params)).await.unwrap();
        let qid = res["queryId"].as_str().expect("queryId must be returned");
        assert!(
            qid.starts_with("querya-job-444-"),
            "queryId must start with querya-job-444-, got {}",
            qid
        );

        ConnectionPool::global().remove(444);
    }

    #[tokio::test]
    async fn test_handle_query_optimize_final_and_deduplicate() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 555,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        // OPTIMIZE FINAL
        let query_params = json!({
            "connectionId": 555,
            "sql": "OPTIMIZE TABLE analytics.events FINAL"
        });
        let res = handle_query(Some(query_params)).await.unwrap();
        assert_eq!(res["status"], "completed");
        assert_eq!(res["operation"], "optimize");
        assert!(
            res["message"]
                .as_str()
                .unwrap()
                .contains("Table optimization (FINAL) completed successfully")
        );

        // OPTIMIZE DEDUPLICATE
        let query_params_dedup = json!({
            "connectionId": 555,
            "sql": "OPTIMIZE TABLE analytics.events DEDUPLICATE"
        });
        let res_dedup = handle_query(Some(query_params_dedup)).await.unwrap();
        assert_eq!(res_dedup["status"], "completed");
        assert_eq!(res_dedup["operation"], "optimize");
        assert!(
            res_dedup["message"]
                .as_str()
                .unwrap()
                .contains("Table deduplication completed successfully")
        );

        ConnectionPool::global().remove(555);
    }

    #[tokio::test]
    async fn test_handle_query_partition_lifecycle() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 666,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            readonly: Some(false),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        // FREEZE PARTITION
        let freeze_res = handle_query(Some(json!({
            "connectionId": 666,
            "sql": "ALTER TABLE analytics.events FREEZE PARTITION '202607'"
        })))
        .await
        .unwrap();
        assert_eq!(freeze_res["operation"], "alter");
        assert!(
            freeze_res["message"]
                .as_str()
                .unwrap()
                .contains("Partition frozen successfully")
        );
        assert!(freeze_res["message"].as_str().unwrap().contains("/shadow/"));

        // DROP PARTITION
        let drop_res = handle_query(Some(json!({
            "connectionId": 666,
            "sql": "ALTER TABLE analytics.events DROP PARTITION '202607'"
        })))
        .await
        .unwrap();
        assert!(
            drop_res["message"]
                .as_str()
                .unwrap()
                .contains("Partition dropped successfully")
        );

        // DETACH PARTITION
        let detach_res = handle_query(Some(json!({
            "connectionId": 666,
            "sql": "ALTER TABLE analytics.events DETACH PARTITION '202607'"
        })))
        .await
        .unwrap();
        assert!(
            detach_res["message"]
                .as_str()
                .unwrap()
                .contains("Partition detached successfully")
        );

        // ATTACH PARTITION
        let attach_res = handle_query(Some(json!({
            "connectionId": 666,
            "sql": "ALTER TABLE analytics.events ATTACH PARTITION '202607'"
        })))
        .await
        .unwrap();
        assert!(
            attach_res["message"]
                .as_str()
                .unwrap()
                .contains("Partition attached successfully")
        );

        ConnectionPool::global().remove(666);
    }

    #[tokio::test]
    async fn test_handle_kill_mutation() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 777,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let res = handle_kill_mutation(Some(json!({
            "connectionId": 777,
            "mutationId": "mut_123"
        })))
        .await
        .unwrap();
        assert_eq!(res["ok"], true);

        ConnectionPool::global().remove(777);
    }

    #[tokio::test]
    async fn test_kill_status_messages() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 778,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let mut_kill = handle_query(Some(json!({
            "connectionId": 778,
            "sql": "KILL MUTATION WHERE mutation_id = 'mut_123'"
        })))
        .await
        .unwrap();
        assert_eq!(mut_kill["operation"], "kill");
        assert!(
            mut_kill["message"]
                .as_str()
                .unwrap()
                .contains("Mutation(s) killed successfully")
        );

        let q_kill = handle_query(Some(json!({
            "connectionId": 778,
            "sql": "KILL QUERY WHERE elapsed > 100"
        })))
        .await
        .unwrap();
        assert_eq!(q_kill["operation"], "kill");
        assert!(
            q_kill["message"]
                .as_str()
                .unwrap()
                .contains("Query/process(es) killed successfully")
        );

        ConnectionPool::global().remove(778);
    }

    #[test]
    fn test_validate_query_or_mutation_id() {
        assert!(validate_query_or_mutation_id("q123", "queryId").is_ok());
        assert!(validate_query_or_mutation_id("mutation_123.txt", "mutationId").is_ok());
        assert!(validate_query_or_mutation_id("querya-job-1-12345-67", "queryId").is_ok());

        // Empty
        assert!(validate_query_or_mutation_id("", "queryId").is_err());
        // Too long (>256)
        assert!(validate_query_or_mutation_id(&"a".repeat(257), "queryId").is_err());
        // Disallowed chars (SQL injection vectors)
        assert!(validate_query_or_mutation_id("' OR 1=1 --", "queryId").is_err());
        assert!(validate_query_or_mutation_id("id; DROP TABLE x;", "queryId").is_err());
        assert!(validate_query_or_mutation_id("id`injection", "mutationId").is_err());
        assert!(validate_query_or_mutation_id("id with spaces", "queryId").is_err());
        assert!(validate_query_or_mutation_id("id\nnewline", "queryId").is_err());
    }

    #[tokio::test]
    async fn test_handle_cancel_rejects_malicious_query_id() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 881,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let cancel_params = json!({
            "connectionId": 881,
            "queryId": "' OR 1=1 --"
        });
        let err = handle_cancel(Some(cancel_params)).await.unwrap_err();
        assert!(matches!(err, DriverError::Client(_)));

        ConnectionPool::global().remove(881);
    }

    #[tokio::test]
    async fn test_handle_kill_mutation_rejects_malicious_mutation_id() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 882,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let kill_params = json!({
            "connectionId": 882,
            "mutationId": "' OR 1=1 --"
        });
        let err = handle_kill_mutation(Some(kill_params)).await.unwrap_err();
        assert!(matches!(err, DriverError::Client(_)));

        ConnectionPool::global().remove(882);
    }

    #[tokio::test]
    async fn test_handle_query_rejects_malicious_custom_query_id() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 883,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let query_params = json!({
            "connectionId": 883,
            "sql": "SELECT 1",
            "queryId": "malicious'query"
        });
        let err = handle_query(Some(query_params)).await.unwrap_err();
        assert!(matches!(err, DriverError::Client(_)));

        ConnectionPool::global().remove(883);
    }
}
