use crate::driver::pool::ConnectionPool;
use crate::error::DriverError;
use crate::sdui::tree::*;
use crate::utils::secret_guard::ConnectionSecretsPool;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;
use url::Url;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSchemaTreeParams {
    pub connection_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpandTreeNodeParams {
    pub connection_id: u64,
    pub node_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextActionsParams {
    pub connection_id: u64,
    pub node_type: String,
    pub node_id: String,
}

/// Helper to execute an internal introspection query against ClickHouse using HTTP client and credentials pool.
async fn run_introspection_query(connection_id: u64, sql: &str) -> Result<String, DriverError> {
    let client = ConnectionPool::global()
        .get(connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(connection_id))?;

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        return Ok(String::new());
    }

    let mut url = Url::parse(&client.base_url)?;
    url.query_pairs_mut()
        .append_pair("database", &client.database);
    if client.readonly {
        url.query_pairs_mut()
            .append_pair("readonly", "1")
            .append_pair("max_execution_time", "300")
            .append_pair("max_memory_usage", "10000000000");
    }

    let mut req = client.http_client.post(url).body(sql.to_string());
    if let Some(secrets) = ConnectionSecretsPool::global().get(client.connection_id) {
        if let Some(jwt) = secrets.expose_jwt_token() {
            req = req.header("Authorization", format!("Bearer {}", jwt));
        } else if let Some(pass) = secrets.expose_password() {
            req = req
                .header("X-ClickHouse-User", &client.user)
                .header("X-ClickHouse-Key", pass);
        }
    } else {
        req = req.header("X-ClickHouse-User", &client.user);
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(DriverError::Client(format!(
            "Introspection query error {}: {}",
            status, text
        )));
    }

    Ok(resp.text().await?)
}

/// Handler for `db.getSchemaTree`. Returns root database nodes (`SYSTEM.databases`).
pub async fn handle_get_schema_tree(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.getSchemaTree requires connectionId".to_string(),
        data: None,
    })?;

    let p: GetSchemaTreeParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed getSchemaTree parameters: {}", e),
            data: None,
        })?;

    let client = ConnectionPool::global()
        .get(p.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(p.connection_id))?;

    info!(
        "Generating Schema Tree roots for connectionId={}",
        p.connection_id
    );

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        let nodes = build_root_databases_nodes(None)?;
        return Ok(json!({ "nodes": nodes }));
    }

    let sql = "SELECT name, engine, comment FROM system.databases ORDER BY name FORMAT JSONCompactEachRowWithNamesAndTypes";
    let text = run_introspection_query(p.connection_id, sql).await?;
    let nodes = build_root_databases_nodes(Some(&text))?;
    Ok(json!({ "nodes": nodes }))
}

/// Handler for `db.expandTreeNode`. Returns child nodes (`Tables`, `Views`, `Columns`, `Partitions`).
pub async fn handle_expand_tree_node(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.expandTreeNode requires connectionId and nodeId".to_string(),
        data: None,
    })?;

    let p: ExpandTreeNodeParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed expandTreeNode parameters: {}", e),
            data: None,
        })?;

    let client = ConnectionPool::global()
        .get(p.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(p.connection_id))?;

    info!(
        "Expanding node '{}' for connectionId={}",
        p.node_id, p.connection_id
    );

    let parts: Vec<&str> = p.node_id.split('.').collect();
    if parts.is_empty() {
        return Err(DriverError::Client(format!(
            "Invalid nodeId format: '{}'",
            p.node_id
        )));
    }

    let prefix = parts[0];

    // 1. Expand database -> Groups (Tables, Views, Dictionaries)
    if prefix == "db" && parts.len() >= 2 {
        let db_name = parts[1];
        let groups = build_database_groups(db_name);
        return Ok(json!({ "nodes": groups }));
    }

    // 2. Expand Table or View -> Groups (Columns, Partitions)
    if (prefix == "table" || prefix == "view") && parts.len() >= 3 {
        let db_name = parts[1];
        let table_name = parts[2];
        let groups = build_table_groups(db_name, table_name);
        return Ok(json!({ "nodes": groups }));
    }

    // Check mock mode for detailed lists
    let is_mock = client.base_url.starts_with("mock://") || client.base_url.starts_with("test://");

    // 3. Expand Group -> Tables / Views / Dictionaries
    if prefix == "group" && parts.len() >= 3 {
        let db_name = parts[1];
        let group_type = parts[2];

        if group_type == "tables" || group_type == "views" {
            let filter_view = group_type == "views";
            let sql = format!(
                "SELECT t.name AS name, t.engine AS engine, t.total_rows AS total_rows, formatReadableSize(t.total_bytes) AS size_readable, t.comment AS comment, multiIf(t.engine LIKE '%View%', 'view', t.engine LIKE '%Dictionary%', 'dictionary', 'table') AS object_type FROM system.tables t WHERE database = '{}' ORDER BY name FORMAT JSONCompactEachRowWithNamesAndTypes",
                db_name
            );

            let text = if is_mock {
                r#"["name", "engine", "total_rows", "size_readable", "comment", "object_type"]
["String", "String", "UInt64", "String", "String", "String"]
["events", "ReplicatedMergeTree", 1500000, "120.4 MiB", "analytics table", "table"]
["mv_summary", "MaterializedView", 500, "10.0 KiB", "", "view"]"#
                    .to_string()
            } else {
                run_introspection_query(p.connection_id, &sql).await?
            };

            let nodes = parse_tables_nodes(db_name, &text, filter_view)?;
            return Ok(json!({ "nodes": nodes }));
        } else if group_type == "dictionaries" {
            let sql = format!(
                "SELECT name, status, type, element_count, load_factor, formatReadableSize(bytes_allocated) AS size FROM system.dictionaries WHERE database = '{}' FORMAT JSONCompactEachRowWithNamesAndTypes",
                db_name
            );

            let text = if is_mock {
                r#"["name", "status", "type", "element_count", "load_factor", "size"]
["String", "String", "String", "UInt64", "Float64", "String"]
["dict_users", "LOADED", "Hashed", 10000, 0.99, "1.2 MiB"]"#
                    .to_string()
            } else {
                run_introspection_query(p.connection_id, &sql).await?
            };

            let nodes = parse_dictionaries_nodes(db_name, &text)?;
            return Ok(json!({ "nodes": nodes }));
        }
    }

    // 4. Expand group_cols -> Columns
    if prefix == "group_cols" && parts.len() >= 3 {
        let db_name = parts[1];
        let table_name = parts[2];
        let sql = format!(
            "SELECT name, type, comment FROM system.columns WHERE database = '{}' AND table = '{}' ORDER BY position FORMAT JSONCompactEachRowWithNamesAndTypes",
            db_name, table_name
        );

        let text = if is_mock {
            r#"["name", "type", "comment"]
["String", "String", "String"]
["user_id", "UInt64", "Unique user identifier"]
["event_name", "String", "Name of action"]"#
                .to_string()
        } else {
            run_introspection_query(p.connection_id, &sql).await?
        };

        let nodes = parse_columns_nodes(db_name, table_name, &text)?;
        return Ok(json!({ "nodes": nodes }));
    }

    // 5. Expand group_parts -> Partitions
    if prefix == "group_parts" && parts.len() >= 3 {
        let db_name = parts[1];
        let table_name = parts[2];
        let sql = format!(
            "SELECT partition, sum(rows) AS total_rows, formatReadableSize(sum(data_compressed_bytes)) AS compressed_size, count() AS parts_count FROM system.parts WHERE database = '{}' AND table = '{}' AND active = 1 GROUP BY partition ORDER BY partition DESC FORMAT JSONCompactEachRowWithNamesAndTypes",
            db_name, table_name
        );

        let text = if is_mock {
            r#"["partition", "total_rows", "compressed_size", "parts_count"]
["String", "UInt64", "String", "UInt64"]
["202607", 500000, "45.2 MiB", 3]
["202606", 1000000, "75.2 MiB", 5]"#
                .to_string()
        } else {
            run_introspection_query(p.connection_id, &sql).await?
        };

        let nodes = parse_partitions_nodes(db_name, table_name, &text)?;
        return Ok(json!({ "nodes": nodes }));
    }

    Err(DriverError::Client(format!(
        "Unsupported or unknown nodeId for expansion: '{}'",
        p.node_id
    )))
}

/// Handler for `db.getConnectionFormSchema`. Returns SDUI form schema (`connection_form.json`).
pub async fn handle_get_connection_form_schema(
    _params: Option<Value>,
) -> Result<Value, DriverError> {
    info!("Serving SDUI connection form schema");
    Ok(crate::sdui::form::get_connection_form_schema())
}

/// Handler for `sdui.contextActions`. Returns context menu actions (`table`, `partition`, `database`, `view`).
pub async fn handle_context_actions(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: sdui.contextActions requires connectionId, nodeType and nodeId"
            .to_string(),
        data: None,
    })?;

    let p: ContextActionsParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed sdui.contextActions parameters: {}", e),
            data: None,
        })?;

    // Verify connection exists in pool
    let _client = ConnectionPool::global()
        .get(p.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(p.connection_id))?;

    info!(
        "Generating context actions for connectionId={}, nodeType='{}', nodeId='{}'",
        p.connection_id, p.node_type, p.node_id
    );

    let actions = crate::sdui::actions::get_context_actions_for_node(&p.node_type, &p.node_id)?;
    Ok(json!({ "actions": actions }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::client::{ClickHouseClient, ConnectParams};

    #[tokio::test]
    async fn test_handle_get_schema_tree_mock() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 401,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let res = handle_get_schema_tree(Some(json!({ "connectionId": 401 })))
            .await
            .unwrap();
        let nodes = res["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["id"], "db.analytics");
        assert_eq!(nodes[1]["id"], "db.system");

        ConnectionPool::global().remove(401);
    }

    #[tokio::test]
    async fn test_handle_expand_tree_node_hierarchy() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 402,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        // 1. Expand database -> groups
        let res_db = handle_expand_tree_node(Some(
            json!({ "connectionId": 402, "nodeId": "db.analytics" }),
        ))
        .await
        .unwrap();
        assert_eq!(res_db["nodes"].as_array().unwrap().len(), 3);

        // 2. Expand group.analytics.tables -> tables list
        let res_tbl = handle_expand_tree_node(Some(
            json!({ "connectionId": 402, "nodeId": "group.analytics.tables" }),
        ))
        .await
        .unwrap();
        assert_eq!(res_tbl["nodes"][0]["id"], "table.analytics.events");

        // 3. Expand table.analytics.events -> cols & parts groups
        let res_sub = handle_expand_tree_node(Some(
            json!({ "connectionId": 402, "nodeId": "table.analytics.events" }),
        ))
        .await
        .unwrap();
        assert_eq!(res_sub["nodes"].as_array().unwrap().len(), 2);

        // 4. Expand group_cols -> columns list
        let res_cols = handle_expand_tree_node(Some(
            json!({ "connectionId": 402, "nodeId": "group_cols.analytics.events" }),
        ))
        .await
        .unwrap();
        assert_eq!(res_cols["nodes"][0]["label"], "user_id (UInt64)");

        // 5. Expand group_parts -> partitions list
        let res_parts = handle_expand_tree_node(Some(
            json!({ "connectionId": 402, "nodeId": "group_parts.analytics.events" }),
        ))
        .await
        .unwrap();
        assert_eq!(res_parts["nodes"][0]["label"], "⚡ 202607");

        ConnectionPool::global().remove(402);
    }

    #[tokio::test]
    async fn test_handle_get_connection_form_schema() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let res = handle_get_connection_form_schema(None).await.unwrap();
        assert_eq!(res["type"], "form");
        assert_eq!(res["id"], "clickhouse_connection_form");
    }

    #[tokio::test]
    async fn test_handle_context_actions() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 403,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let params = json!({
            "connectionId": 403,
            "nodeType": "table",
            "nodeId": "table.analytics.events"
        });

        let res = handle_context_actions(Some(params)).await.unwrap();
        let actions = res["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 8);
        assert_eq!(actions[0]["id"], "table.top_100");

        ConnectionPool::global().remove(403);
    }
}
