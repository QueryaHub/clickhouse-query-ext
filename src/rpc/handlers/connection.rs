use crate::driver::client::{ClickHouseClient, ConnectParams};
use crate::driver::pool::ConnectionPool;
use crate::error::DriverError;
use crate::utils::secret_guard::ConnectionSecretsPool;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisconnectParams {
    pub connection_id: u64,
}

/// Handler for `db.connect`.
/// Builds HTTP client from `connectionString` or host/port, performs health check (`SELECT version()`),
/// and upon success stores the session handle in `ConnectionPool`.
pub async fn handle_connect(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.connect requires connection definition".to_string(),
        data: None,
    })?;

    let connect_params: ConnectParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Invalid connect params: {}", e),
            data: None,
        })?;

    let client = ClickHouseClient::from_params(connect_params)?;
    let connection_id = client.connection_id;

    info!(
        "Verifying connection to ClickHouse for connectionId={}...",
        connection_id
    );
    let server_version = client.ping_connection().await?;

    ConnectionPool::global().insert(client);
    info!(
        "Successfully connected to ClickHouse v{} (connectionId={})",
        server_version, connection_id
    );

    Ok(json!({
        "connected": true,
        "serverVersion": server_version
    }))
}

/// Handler for `db.disconnect`.
/// Removes the `ClickHouseClient` session from the active pool and wipes any credentials in `ConnectionSecretsPool`.
pub async fn handle_disconnect(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.disconnect requires connectionId".to_string(),
        data: None,
    })?;

    let connection_id =
        if let Ok(p) = serde_json::from_value::<DisconnectParams>(params_val.clone()) {
            p.connection_id
        } else if let Some(id) = params_val.as_u64() {
            id
        } else if let Some(obj) = params_val.as_object() {
            obj.get("connectionId")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| DriverError::Rpc {
                    code: -32602,
                    message: "Missing or invalid connectionId in db.disconnect".to_string(),
                    data: None,
                })?
        } else {
            return Err(DriverError::Rpc {
                code: -32602,
                message: "Unsupported db.disconnect params format".to_string(),
                data: None,
            });
        };

    let removed_pool = ConnectionPool::global().remove(connection_id);
    let removed_secrets = ConnectionSecretsPool::global().remove(connection_id);

    info!(
        "Disconnected connectionId={} (pool_removed={}, secrets_wiped={})",
        connection_id, removed_pool, removed_secrets
    );

    Ok(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handle_connect_and_disconnect() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let connect_json = json!({
            "connectionId": 777,
            "connectionString": "mock://localhost:8123/default?readonly=1"
        });

        let res = handle_connect(Some(connect_json)).await.unwrap();
        assert!(res["connected"].as_bool().unwrap_or(false));
        assert_eq!(res["serverVersion"], "mock-clickhouse-23.8.1.1");
        assert!(ConnectionPool::global().get(777).is_some());

        let disconnect_json = json!({ "connectionId": 777 });
        let res_dis = handle_disconnect(Some(disconnect_json)).await.unwrap();
        assert!(res_dis["ok"].as_bool().unwrap_or(false));
        assert!(ConnectionPool::global().get(777).is_none());
    }
}
