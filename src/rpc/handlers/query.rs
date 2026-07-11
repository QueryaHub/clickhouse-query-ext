use crate::driver::pool::ConnectionPool;
use crate::error::DriverError;
use crate::mapper::row_compact::parse_compact_output;
use crate::utils::secret_guard::ConnectionSecretsPool;
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

fn strip_sql_comments_and_trim(sql: &str) -> String {
    let mut res = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_single_comment = false;
    let mut in_multi_comment = false;
    let mut in_string = false;
    let mut string_quote = ' ';

    while let Some(c) = chars.next() {
        if in_single_comment {
            if c == '\n' {
                in_single_comment = false;
                res.push(' ');
            }
        } else if in_multi_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_multi_comment = false;
                res.push(' ');
            }
        } else if in_string {
            res.push(c);
            if c == string_quote {
                in_string = false;
            }
        } else if c == '-' && chars.peek() == Some(&'-') {
            chars.next();
            in_single_comment = true;
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_multi_comment = true;
        } else if c == '\'' || c == '`' || c == '"' {
            in_string = true;
            string_quote = c;
            res.push(c);
        } else {
            res.push(c);
        }
    }
    res.trim().to_uppercase()
}

/// Pre-checks AST/SQL syntax in Safe Mode (`readonly = true`) before network roundtrip.
fn enforce_safe_mode_precheck(sql: &str) -> Result<(), DriverError> {
    let upper = strip_sql_comments_and_trim(sql);
    let tokens: Vec<&str> = upper.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(());
    }

    let first = tokens[0];
    let second = tokens.get(1).copied().unwrap_or("");
    let third = tokens.get(2).copied().unwrap_or("");

    let is_dangerous = match first {
        "DROP" => {
            second == "DATABASE" || second == "TABLE" || second == "VIEW" || second == "DICTIONARY"
        }
        "TRUNCATE" => second == "TABLE",
        "ALTER" => {
            second == "TABLE"
                && tokens.iter().any(|&t| {
                    t == "DROP"
                        || t == "DELETE"
                        || t == "UPDATE"
                        || t == "MODIFY"
                        || t == "REPLACE"
                        || t == "CLEAR"
                        || t == "FREEZE"
                        || t == "ATTACH"
                        || t == "DETACH"
                })
        }
        "INSERT" => second == "INTO" || third == "INTO",
        "DELETE" => second == "FROM" || third == "FROM",
        "UPDATE" => true,
        "CREATE" => {
            second == "DATABASE" || second == "TABLE" || second == "VIEW" || second == "DICTIONARY"
        }
        "RENAME" => second == "TABLE" || second == "DATABASE",
        "ATTACH" | "DETACH" => second == "TABLE" || second == "PARTITION",
        _ => {
            upper.contains("DROP DATABASE")
                || upper.contains("TRUNCATE TABLE")
                || upper.contains("DROP TABLE")
                || (upper.contains("ALTER TABLE") && upper.contains("DROP"))
        }
    };

    if is_dangerous {
        return Err(DriverError::SafeModeViolation(
            "Operation blocked by Safe Mode: write or destructive queries are forbidden in analytical read-only mode".to_string(),
        ));
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
    let is_tabular_query = upper_sql.starts_with("SELECT")
        || upper_sql.starts_with("SHOW")
        || upper_sql.starts_with("DESCRIBE")
        || upper_sql.starts_with("EXPLAIN");

    let sql_to_run = if is_tabular_query && !upper_sql.contains("FORMAT ") {
        format!(
            "{}\nFORMAT JSONCompactEachRowWithNamesAndTypes",
            trimmed_sql
        )
    } else {
        trimmed_sql.to_string()
    };

    let actual_query_id = match &query_params.query_id {
        Some(qid) if !qid.is_empty() => qid.clone(),
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
            let mut parsed_val = serde_json::to_value(parse_compact_output(
                mock_output,
                start_time.elapsed().as_millis() as u64,
            )?)?;
            if let Some(obj) = parsed_val.as_object_mut() {
                obj.insert("queryId".to_string(), json!(actual_query_id));
            }
            return Ok(parsed_val);
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
    let text = client
        .post_sql(&sql_to_run, |url| {
            url.query_pairs_mut()
                .append_pair("query_id", &actual_query_id_for_url);
        })
        .await?;
    let elapsed = start_time.elapsed().as_millis() as u64;

    if is_tabular_query {
        let mut parsed_val = serde_json::to_value(parse_compact_output(&text, elapsed)?)?;
        if let Some(obj) = parsed_val.as_object_mut() {
            obj.insert("queryId".to_string(), json!(actual_query_id));
        }
        Ok(parsed_val)
    } else {
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

    info!(
        "Cancelling queryId={} on connectionId={}",
        cancel_params.query_id, cancel_params.connection_id
    );

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        return Ok(json!({ "ok": true }));
    }

    let sync_kw = if cancel_params.sync { "SYNC" } else { "ASYNC" };
    let mut url = Url::parse(&client.base_url)?;
    url.query_pairs_mut()
        .append_pair("database", &client.database)
        .append_pair(
            "query",
            &format!(
                "KILL QUERY WHERE query_id = '{}' {}",
                cancel_params.query_id, sync_kw
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

    info!(
        "Killing mutationId={} on connectionId={}",
        kill_params.mutation_id, kill_params.connection_id
    );

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        return Ok(json!({ "ok": true }));
    }

    let sync_kw = if kill_params.sync { "SYNC" } else { "ASYNC" };
    let mut url = Url::parse(&client.base_url)?;
    url.query_pairs_mut()
        .append_pair("database", &client.database)
        .append_pair(
            "query",
            &format!(
                "KILL MUTATION WHERE mutation_id = '{}' {}",
                kill_params.mutation_id, sync_kw
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
}
