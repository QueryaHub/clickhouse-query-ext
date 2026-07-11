use serde_json::{json, Value};
use crate::error::DriverError;

pub async fn handle_connect(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!({ "connected": true }))
}

pub async fn handle_disconnect(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!({ "ok": true }))
}
