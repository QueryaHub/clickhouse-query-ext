use serde_json::Value;

pub fn get_connection_form_schema() -> Value {
    let schema_str = include_str!("../../assets/connection_form.json");
    serde_json::from_str(schema_str).unwrap_or(serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_connection_form_schema() {
        let schema = get_connection_form_schema();
        assert_eq!(schema["type"], "form");
        assert_eq!(schema["id"], "clickhouse_connection_form");
        assert!(schema["fields"].is_array());
        let fields = schema["fields"].as_array().unwrap();
        assert!(!fields.is_empty());

        let host_field = fields.iter().find(|f| f["key"] == "host").unwrap();
        assert_eq!(host_field["defaultValue"], "localhost");

        let safe_mode_field = fields.iter().find(|f| f["key"] == "safe_mode").unwrap();
        assert_eq!(safe_mode_field["type"], "boolean");
        assert_eq!(safe_mode_field["defaultValue"], true);
    }
}
