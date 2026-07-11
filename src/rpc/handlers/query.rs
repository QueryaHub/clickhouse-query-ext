use crate::driver::pool::ConnectionPool;
use crate::error::DriverError;
use crate::mapper::row_compact::parse_compact_output;
use crate::utils::secret_guard::ConnectionSecretsPool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Instant;
use tracing::info;
use url::Url;

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
}

/// Pre-checks AST/SQL syntax in Safe Mode (`readonly = true`) before network roundtrip.
fn enforce_safe_mode_precheck(sql: &str) -> Result<(), DriverError> {
    let upper = sql.trim().to_uppercase();
    if upper.contains("DROP DATABASE")
        || upper.contains("TRUNCATE TABLE")
        || upper.contains("DROP TABLE")
        || (upper.contains("ALTER TABLE") && upper.contains("DROP"))
        || upper.starts_with("INSERT INTO")
        || upper.starts_with("DELETE FROM")
        || upper.starts_with("UPDATE ")
        || upper.starts_with("CREATE DATABASE")
        || upper.starts_with("CREATE TABLE")
    {
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

    info!(
        "Executing SQL on connectionId={} (query_id={:?}, readonly={}): {}...",
        query_params.connection_id,
        query_params.query_id,
        client.readonly,
        trimmed_sql.lines().next().unwrap_or("")
    );

    let start_time = Instant::now();

    // 2. Mock handler for unit tests
    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        let mock_output = if is_tabular_query {
            r#"["id", "event_name", "user_id"]
["UInt64", "String", "Nullable(UInt64)"]
[18446744073709551615, "page_view", 42]
[100, "click", null]"#
        } else {
            ""
        };
        let parsed = parse_compact_output(mock_output, start_time.elapsed().as_millis() as u64)?;
        return Ok(serde_json::to_value(parsed)?);
    }

    // 3. Real ClickHouse HTTP request
    let mut url = Url::parse(&client.base_url)?;
    url.query_pairs_mut()
        .append_pair("database", &client.database);
    if client.readonly {
        url.query_pairs_mut().append_pair("readonly", "1");
    }
    if let Some(qid) = &query_params.query_id {
        url.query_pairs_mut().append_pair("query_id", qid);
    }

    let mut req = client.http_client.post(url).body(sql_to_run);

    if let Some(secrets) = ConnectionSecretsPool::global().get(client.connection_id) {
        if let Some(jwt) = secrets.expose_jwt_token() {
            req = req.header("Authorization", format!("Bearer {}", jwt));
        } else if let Some(pass) = secrets.expose_password() {
            req = req
                .header("X-ClickHouse-User", &client.user)
                .header("X-ClickHouse-Key", pass);
        } else {
            req = req.header("X-ClickHouse-User", &client.user);
        }
    } else {
        req = req.header("X-ClickHouse-User", &client.user);
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(DriverError::Client(format!(
            "ClickHouse SQL error {}: {}",
            status, text
        )));
    }

    let text = resp.text().await?;
    let elapsed = start_time.elapsed().as_millis() as u64;

    if is_tabular_query {
        let parsed = parse_compact_output(&text, elapsed)?;
        Ok(serde_json::to_value(parsed)?)
    } else {
        Ok(json!({
            "columns": [],
            "rows": [],
            "statistics": {
                "rowsRead": 0,
                "bytesRead": text.len(),
                "elapsedMs": elapsed
            }
        }))
    }
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

    let mut url = Url::parse(&client.base_url)?;
    url.query_pairs_mut()
        .append_pair("database", &client.database)
        .append_pair(
            "query",
            &format!(
                "KILL QUERY WHERE query_id = '{}' ASYNC",
                cancel_params.query_id
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::client::{ClickHouseClient, ConnectParams};

    #[test]
    fn test_safe_mode_precheck_rejections() {
        assert!(enforce_safe_mode_precheck("SELECT * FROM events").is_ok());
        assert!(enforce_safe_mode_precheck("SHOW TABLES").is_ok());
        assert!(enforce_safe_mode_precheck("DROP TABLE events").is_err());
        assert!(enforce_safe_mode_precheck("TRUNCATE TABLE logs").is_err());
        assert!(enforce_safe_mode_precheck("ALTER TABLE events DROP COLUMN age").is_err());
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
    async fn test_handle_cancel() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 333,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let cancel_params = json!({
            "connectionId": 333,
            "queryId": "query-to-cancel-123"
        });

        let res = handle_cancel(Some(cancel_params)).await.unwrap();
        assert_eq!(res, json!({ "ok": true }));

        ConnectionPool::global().remove(333);
    }
}
