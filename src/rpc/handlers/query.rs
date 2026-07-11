use serde_json::{json, Value};
use crate::error::DriverError;

pub async fn handle_query(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!({
        "columns": [],
        "rows": [],
        "rowsAffected": 0,
        "executionTimeMs": 0.0
    }))
}

pub async fn handle_cancel(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!({ "ok": true }))
}
