use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;
use crate::error::DriverError;
use crate::utils::secret_guard::ConnectionSecretsPool;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HandshakeParams {
    pub querya_version: Option<String>,
    pub plugin_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectCredentialsParams {
    pub connection_id: u64,
    pub password: Option<String>,
    pub jwt_token: Option<String>,
}

/// Handler for `system.handshake`.
/// Must respond in `< 3 seconds` after process startup.
/// Validates basic protocol parameters and returns supported `capabilities`.
pub async fn handle_handshake(params: Option<Value>) -> Result<Value, DriverError> {
    if let Some(val) = params {
        if let Ok(handshake) = serde_json::from_value::<HandshakeParams>(val) {
            info!(
                "Received system.handshake from Querya host v{} for plugin {}",
                handshake.querya_version.as_deref().unwrap_or("unknown"),
                handshake.plugin_id.as_deref().unwrap_or("unknown")
            );
        }
    } else {
        info!("Received system.handshake without params");
    }

    Ok(json!({
        "ok": true,
        "protocolVersion": 1,
        "driverVersion": "1.0.0-rust",
        "capabilities": [
            "db.connect",
            "db.disconnect",
            "db.query",
            "db.execute",
            "db.cancelQuery",
            "db.getSchemaTree",
            "db.expandTreeNode",
            "db.getConnectionFormSchema",
            "sdui.contextActions"
        ]
    }))
}

/// Handler for `system.ping`.
/// Heartbeat monitor (`SandboxWatchdog`) checks this every 30s. Must reply `< 5ms`.
pub async fn handle_ping(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!("pong"))
}

/// Handler for `system.injectCredentials`.
/// Securely stores password and/or JWT token into `ConnectionSecretsPool` (`SecretString`).
pub async fn handle_inject_credentials(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: system.injectCredentials requires connectionId and credentials".to_string(),
        data: None,
    })?;

    let p: InjectCredentialsParams = serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
        code: -32602,
        message: format!("Invalid injectCredentials params structure: {}", e),
        data: None,
    })?;

    ConnectionSecretsPool::global().inject(
        p.connection_id,
        p.password,
        p.jwt_token,
    );

    info!("Credentials securely injected into zero-trust pool for connectionId={}", p.connection_id);
    Ok(json!({ "ok": true }))
}

/// Handler for `system.shutdown`.
/// Graceful teardown signal from Querya Host: clears all memory secrets and zeroizes RAM.
pub async fn handle_shutdown(_params: Option<Value>) -> Result<Value, DriverError> {
    info!("system.shutdown requested by host. Wiping all in-memory connection secrets...");
    ConnectionSecretsPool::global().clear_all();
    Ok(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handle_handshake() {
        let params = json!({
            "queryaVersion": "2.0.0",
            "pluginId": "queryahub.clickhouse-driver"
        });
        let res = handle_handshake(Some(params)).await.unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["protocolVersion"], 1);
        assert_eq!(res["driverVersion"], "1.0.0-rust");
        let caps = res["capabilities"].as_array().unwrap();
        assert!(caps.contains(&json!("db.connect")));
        assert!(caps.contains(&json!("db.query")));
        assert!(caps.contains(&json!("db.cancelQuery")));
        assert!(caps.contains(&json!("sdui.contextActions")));
    }

    #[tokio::test]
    async fn test_handle_ping() {
        let res = handle_ping(None).await.unwrap();
        assert_eq!(res, json!("pong"));
    }

    #[tokio::test]
    async fn test_handle_inject_credentials() {
        let params = json!({
            "connectionId": 999,
            "password": "ClickHouseSecurePassword999",
            "jwtToken": null
        });
        let res = handle_inject_credentials(Some(params)).await.unwrap();
        assert_eq!(res, json!({ "ok": true }));

        let secrets = ConnectionSecretsPool::global().get(999).expect("Should find secrets for id 999");
        assert_eq!(secrets.expose_password(), Some("ClickHouseSecurePassword999"));
        assert_eq!(secrets.expose_jwt_token(), None);

        // Clean up after test
        ConnectionSecretsPool::global().remove(999);
    }

    #[tokio::test]
    async fn test_handle_shutdown_wipes_pool() {
        ConnectionSecretsPool::global().inject(888, Some("pass".to_string()), None);
        assert!(ConnectionSecretsPool::global().get(888).is_some());

        let res = handle_shutdown(None).await.unwrap();
        assert_eq!(res, json!({ "ok": true }));
        assert!(ConnectionSecretsPool::global().get(888).is_none());
    }
}
