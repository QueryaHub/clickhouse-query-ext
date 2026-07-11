use crate::error::DriverError;
use serde_json::{Value, json};

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
