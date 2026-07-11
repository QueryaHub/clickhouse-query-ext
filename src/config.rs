use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub plugin_id: String,
    pub version: String,
    pub max_memory_mb: usize,
    pub safe_mode_default: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            plugin_id: "queryahub.clickhouse-driver".to_string(),
            version: "1.0.0".to_string(),
            max_memory_mb: 256,
            safe_mode_default: true,
        }
    }
}
