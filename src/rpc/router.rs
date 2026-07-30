use crate::error::DriverError;
use crate::rpc::handlers::{connection, query, schema, system};
use serde_json::Value;

pub async fn dispatch(method: &str, params: Option<Value>) -> Result<Value, DriverError> {
    match method {
        "system.handshake" => system::handle_handshake(params).await,
        "system.ping" => system::handle_ping(params).await,
        "system.shutdown" => system::handle_shutdown(params).await,
        "system.injectCredentials" => system::handle_inject_credentials(params).await,
        "db.connect" => connection::handle_connect(params).await,
        "db.disconnect" => connection::handle_disconnect(params).await,
        "db.query" | "db.execute" => query::handle_query(params).await,
        "db.cancelQuery" => query::handle_cancel(params).await,
        "db.killMutation" => query::handle_kill_mutation(params).await,
        "db.getSchemaTree" => schema::handle_get_schema_tree(params).await,
        "db.expandTreeNode" => schema::handle_expand_tree_node(params).await,
        "db.getConnectionFormSchema" => schema::handle_get_connection_form_schema(params).await,
        "sdui.contextActions" => schema::handle_context_actions(params).await,
        "db.getCapabilities" => schema::handle_get_capabilities(params).await,
        "db.getServerStats" => schema::handle_get_server_stats(params).await,
        "db.getObjectMetadata" | "db.getObjectDDL" => {
            schema::handle_get_object_metadata(params).await
        }
        _ => Err(DriverError::Rpc {
            code: -32601,
            message: format!("Method not found: {}", method),
            data: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::client::{ClickHouseClient, ConnectParams};
    use crate::driver::pool::ConnectionPool;
    use serde_json::json;

    #[tokio::test]
    async fn test_dispatch_get_connection_form_schema() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let res = dispatch("db.getConnectionFormSchema", None).await.unwrap();
        assert_eq!(res["id"], "clickhouse_connection_form");
    }

    #[tokio::test]
    async fn test_dispatch_context_actions() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 888,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let params = json!({
            "connectionId": 888,
            "nodeType": "partition",
            "nodeId": "part.analytics.events.202607"
        });

        let res = dispatch("sdui.contextActions", Some(params)).await.unwrap();
        let actions = res["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 6);
        assert_eq!(actions[0]["id"], "partition.drop");

        ConnectionPool::global().remove(888);
    }

    #[tokio::test]
    async fn test_dispatch_kill_mutation() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 889,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let params = json!({
            "connectionId": 889,
            "mutationId": "mutation-123"
        });

        let res = dispatch("db.killMutation", Some(params)).await.unwrap();
        assert_eq!(res["ok"], true);

        ConnectionPool::global().remove(889);
    }

    #[tokio::test]
    async fn test_dispatch_system_handshake_and_ping() {
        let handshake_res = dispatch("system.handshake", None).await.unwrap();
        assert_eq!(handshake_res["protocolVersion"], 1);
        assert!(handshake_res["capabilities"].is_array());

        let ping_res = dispatch("system.ping", None).await.unwrap();
        assert_eq!(ping_res, json!("pong"));
    }

    #[tokio::test]
    async fn test_dispatch_unknown_method_error() {
        let err = dispatch("unknown.rpc.method", None).await.unwrap_err();
        assert_eq!(err.to_rpc_code(), -32601);
        assert!(err.to_string().contains("Method not found"));
    }
}
