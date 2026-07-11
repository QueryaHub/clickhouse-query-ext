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
        "db.getSchemaTree" => schema::handle_get_schema_tree(params).await,
        "db.expandTreeNode" => schema::handle_expand_tree_node(params).await,
        "db.getConnectionFormSchema" => schema::handle_get_connection_form_schema(params).await,
        "sdui.contextActions" => schema::handle_context_actions(params).await,
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
}
