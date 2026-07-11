use serde_json::{json, Value};
use crate::error::DriverError;

pub async fn handle_handshake(_params: Option<Value>) -> Result<Value, DriverError> {
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

pub async fn handle_ping(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!("pong"))
}

pub async fn handle_shutdown(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!({ "ok": true }))
}
