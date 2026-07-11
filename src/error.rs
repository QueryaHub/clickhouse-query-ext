use serde_json::Value;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DriverError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization/Deserialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("JSON-RPC Protocol error ({code}): {message}")]
    Rpc {
        code: i32,
        message: String,
        data: Option<Value>,
    },

    #[error("ClickHouse Client error: {0}")]
    Client(String),

    #[error("Connection not found: #{0}")]
    ConnectionNotFound(u64),

    #[error("Safe Mode security violation: {0}")]
    SafeModeViolation(String),
}

impl DriverError {
    pub fn to_rpc_code(&self) -> i32 {
        match self {
            DriverError::Io(_) => -32001,
            DriverError::Serde(_) => -32700,
            DriverError::Rpc { code, .. } => *code,
            DriverError::Client(_) => -32603,
            DriverError::ConnectionNotFound(_) => -32002,
            DriverError::SafeModeViolation(_) => -32603,
        }
    }
}
