use serde_json::Value;

pub fn get_connection_form_schema() -> Value {
    let schema_str = include_str!("../../assets/connection_form.json");
    serde_json::from_str(schema_str).unwrap_or(serde_json::json!({}))
}
