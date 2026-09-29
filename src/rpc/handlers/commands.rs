use crate::error::DriverError;
use crate::rpc::handlers::{query, schema};
use crate::utils::sql_escape::{escape_sql_string_literal, quote_identifier};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

/// Parameters for `commands.execute`, sent by Querya Desktop's Command Palette
/// when the user runs one of this driver's `contributions.commands` entries.
///
/// The Command Palette does not yet forward workspace selection (selected
/// table/partition) to the driver, so `database`/`table`/`partition` are
/// optional here and validated per-command: a command that needs a target
/// returns a clear `-32602` error explaining what's missing instead of
/// silently operating on the wrong object.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandParams {
    pub command_id: String,
    pub connection_id: u64,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub partition: Option<String>,
}

/// Handler for `commands.execute`. Dispatches a Command Palette invocation
/// (`commandId`) to the matching ClickHouse maintenance operation.
pub async fn handle_execute(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: commands.execute requires commandId and connectionId".to_string(),
        data: None,
    })?;

    let p: ExecuteCommandParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed commands.execute parameters: {}", e),
            data: None,
        })?;

    info!(
        "Executing command '{}' on connectionId={}",
        p.command_id, p.connection_id
    );

    match p.command_id.as_str() {
        "clickhouse.serverStats" => {
            schema::handle_get_server_stats(Some(json!({ "connectionId": p.connection_id }))).await
        }
        "clickhouse.optimizeFinal" => {
            let (db, tbl) = require_table_target(&p)?;
            run_sql(
                p.connection_id,
                format!(
                    "OPTIMIZE TABLE {}.{} FINAL",
                    quote_identifier(db),
                    quote_identifier(tbl)
                ),
            )
            .await
        }
        "clickhouse.deduplicate" => {
            let (db, tbl) = require_table_target(&p)?;
            run_sql(
                p.connection_id,
                format!(
                    "OPTIMIZE TABLE {}.{} DEDUPLICATE",
                    quote_identifier(db),
                    quote_identifier(tbl)
                ),
            )
            .await
        }
        "clickhouse.dropPartition" => {
            let (db, tbl) = require_table_target(&p)?;
            let partition = p
                .partition
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| DriverError::Rpc {
                    code: -32602,
                    message:
                        "commandId 'clickhouse.dropPartition' also requires a 'partition' parameter"
                            .to_string(),
                    data: None,
                })?;
            run_sql(
                p.connection_id,
                format!(
                    "ALTER TABLE {}.{} DROP PARTITION '{}'",
                    quote_identifier(db),
                    quote_identifier(tbl),
                    escape_sql_string_literal(partition)
                ),
            )
            .await
        }
        other => Err(DriverError::Rpc {
            code: -32602,
            message: format!("Unknown commandId: '{}'", other),
            data: None,
        }),
    }
}

fn require_table_target(p: &ExecuteCommandParams) -> Result<(&str, &str), DriverError> {
    match (
        p.database.as_deref().filter(|s| !s.is_empty()),
        p.table.as_deref().filter(|s| !s.is_empty()),
    ) {
        (Some(db), Some(tbl)) => Ok((db, tbl)),
        _ => Err(DriverError::Rpc {
            code: -32602,
            message: format!(
                "commandId '{}' requires 'database' and 'table' parameters (select a table first)",
                p.command_id
            ),
            data: None,
        }),
    }
}

async fn run_sql(connection_id: u64, sql: String) -> Result<Value, DriverError> {
    query::handle_query(Some(json!({
        "connectionId": connection_id,
        "sql": sql
    })))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::client::{ClickHouseClient, ConnectParams};
    use crate::driver::pool::ConnectionPool;

    #[tokio::test]
    async fn test_handle_execute_server_stats() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 501,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let res = handle_execute(Some(json!({
            "commandId": "clickhouse.serverStats",
            "connectionId": 501
        })))
        .await
        .unwrap();
        assert_eq!(res["serverVersion"], "ClickHouse 24.3 (Mock)");

        ConnectionPool::global().remove(501);
    }

    #[tokio::test]
    async fn test_handle_execute_optimize_final_and_deduplicate() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 502,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let res = handle_execute(Some(json!({
            "commandId": "clickhouse.optimizeFinal",
            "connectionId": 502,
            "database": "analytics",
            "table": "events"
        })))
        .await
        .unwrap();
        assert_eq!(res["operation"], "optimize");

        let res = handle_execute(Some(json!({
            "commandId": "clickhouse.deduplicate",
            "connectionId": 502,
            "database": "analytics",
            "table": "events"
        })))
        .await
        .unwrap();
        assert_eq!(res["operation"], "optimize");

        ConnectionPool::global().remove(502);
    }

    #[tokio::test]
    async fn test_handle_execute_drop_partition() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 503,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            readonly: Some(false),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let res = handle_execute(Some(json!({
            "commandId": "clickhouse.dropPartition",
            "connectionId": 503,
            "database": "analytics",
            "table": "events",
            "partition": "202607"
        })))
        .await
        .unwrap();
        assert_eq!(res["operation"], "alter");

        ConnectionPool::global().remove(503);
    }

    #[tokio::test]
    async fn test_handle_execute_requires_table_target() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 504,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let err = handle_execute(Some(json!({
            "commandId": "clickhouse.optimizeFinal",
            "connectionId": 504
        })))
        .await
        .unwrap_err();
        assert_eq!(err.to_rpc_code(), -32602);
        assert!(err.to_string().contains("requires 'database' and 'table'"));

        let err = handle_execute(Some(json!({
            "commandId": "clickhouse.dropPartition",
            "connectionId": 504,
            "database": "analytics",
            "table": "events"
        })))
        .await
        .unwrap_err();
        assert!(err.to_string().contains("also requires a 'partition'"));

        ConnectionPool::global().remove(504);
    }

    #[tokio::test]
    async fn test_handle_execute_unknown_command_id() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 505,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let err = handle_execute(Some(json!({
            "commandId": "clickhouse.doesNotExist",
            "connectionId": 505
        })))
        .await
        .unwrap_err();
        assert_eq!(err.to_rpc_code(), -32602);
        assert!(err.to_string().contains("Unknown commandId"));

        ConnectionPool::global().remove(505);
    }

    #[tokio::test]
    async fn test_handle_execute_missing_params() {
        let err = handle_execute(None).await.unwrap_err();
        assert_eq!(err.to_rpc_code(), -32602);
    }
}
