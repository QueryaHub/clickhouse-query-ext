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
        _ => Err(DriverError::Rpc {
            code: -32601,
            message: format!("Method not found: {}", method),
            data: None,
        }),
    }
}
