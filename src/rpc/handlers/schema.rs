use crate::driver::pool::ConnectionPool;
use crate::error::DriverError;
use crate::sdui::tree::*;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

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

    client.post_sql(sql, |_| {}).await
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetServerStatsParams {
    pub connection_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetObjectMetadataParams {
    pub connection_id: u64,
    pub node_id: String,
    pub node_type: String,
}

/// Handler for `db.getCapabilities`. Returns capability feature flags reported by the driver.
pub async fn handle_get_capabilities(_params: Option<Value>) -> Result<Value, DriverError> {
    Ok(json!({
        "supportsTransactions": false,
        "supportsCancel": true,
        "supportsDDLInspection": true,
        "supportsPrivileges": false,
        "hasServerStats": true
    }))
}

/// Handler for `db.getServerStats`. Returns server version, uptime, and database sizes.
pub async fn handle_get_server_stats(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.getServerStats requires connectionId".to_string(),
        data: None,
    })?;

    let p: GetServerStatsParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed getServerStats parameters: {}", e),
            data: None,
        })?;

    let client = ConnectionPool::global()
        .get(p.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(p.connection_id))?;

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        return Ok(json!({
            "serverVersion": "ClickHouse 24.3 (Mock)",
            "uptimeSeconds": 3600,
            "activeConnections": 5,
            "activeQueries": 2,
            "memoryUsageBytes": 134217728,
            "databaseSizes": {
                "default": 10485760,
                "system": 2097152,
                "analytics": 524288000
            },
            "extraMetrics": {
                "queriesPerSecond": 14.5
            }
        }));
    }

    let version_text = client
        .post_sql("SELECT version() FORMAT JSONCompactEachRow", |_| {})
        .await
        .unwrap_or_else(|_| r#"["ClickHouse unknown"]"#.to_string());
    let uptime_text = client
        .post_sql("SELECT uptime() FORMAT JSONCompactEachRow", |_| {})
        .await
        .unwrap_or_else(|_| r#"[0]"#.to_string());

    let mut version_str = "ClickHouse".to_string();
    if let Ok(parsed) = crate::mapper::row_compact::parse_compact_output(&version_text, 0) {
        if let Some(row) = parsed.rows.first() {
            if let Some(v) = row.first().and_then(|x| x.as_str()) {
                version_str = format!("ClickHouse {}", v);
            }
        }
    }

    let mut uptime_sec = 0;
    if let Ok(parsed) = crate::mapper::row_compact::parse_compact_output(&uptime_text, 0) {
        if let Some(row) = parsed.rows.first() {
            if let Some(v) = row.first().and_then(|x| x.as_u64()) {
                uptime_sec = v;
            }
        }
    }

    let db_sizes_text = client
        .post_sql(
            "SELECT database, sum(total_bytes) FROM system.tables GROUP BY database FORMAT JSONCompactEachRowWithNamesAndTypes",
            |_| {},
        )
        .await
        .unwrap_or_default();
    let mut db_sizes = serde_json::Map::new();
    if let Ok(parsed) = crate::mapper::row_compact::parse_compact_output(&db_sizes_text, 0) {
        for row in parsed.rows {
            if let (Some(db), Some(size)) = (
                row.first().and_then(|x| x.as_str()),
                row.get(1).and_then(|x| x.as_u64()),
            ) {
                db_sizes.insert(db.to_string(), json!(size));
            }
        }
    }

    Ok(json!({
        "serverVersion": version_str,
        "uptimeSeconds": uptime_sec,
        "activeConnections": 1,
        "activeQueries": 1,
        "memoryUsageBytes": 0,
        "databaseSizes": db_sizes,
        "extraMetrics": {}
    }))
}

/// Handler for `db.getObjectMetadata`. Returns table/view DDL and column list.
pub async fn handle_get_object_metadata(params: Option<Value>) -> Result<Value, DriverError> {
    let params_val = params.ok_or_else(|| DriverError::Rpc {
        code: -32602,
        message: "Invalid params: db.getObjectMetadata requires connectionId, nodeId, and nodeType"
            .to_string(),
        data: None,
    })?;

    let p: GetObjectMetadataParams =
        serde_json::from_value(params_val).map_err(|e| DriverError::Rpc {
            code: -32602,
            message: format!("Malformed getObjectMetadata parameters: {}", e),
            data: None,
        })?;

    let client = ConnectionPool::global()
        .get(p.connection_id)
        .ok_or_else(|| DriverError::ConnectionNotFound(p.connection_id))?;

    let parts: Vec<&str> = p.node_id.split('.').collect();
    let (db_name, tbl_name) =
        if parts.len() >= 3 && (parts[0] == "table" || parts[0] == "view") {
            (parts[1], parts[2])
        } else if parts.len() >= 2 {
            (parts[0], parts[1])
        } else {
            ("default", p.node_id.as_str())
        };

    if client.base_url.starts_with("mock://") || client.base_url.starts_with("test://") {
        return Ok(json!({
            "nodeId": p.node_id,
            "nodeType": p.node_type,
            "ddl": format!("CREATE TABLE {}.{} (\n  id UInt64,\n  created_at DateTime\n) ENGINE = MergeTree ORDER BY id", db_name, tbl_name),
            "columns": [
                { "name": "id", "dataType": "UInt64", "isNullable": false, "comment": "Primary ID" },
                { "name": "created_at", "dataType": "DateTime", "isNullable": false, "comment": "Creation timestamp" }
            ],
            "properties": {
                "engine": "MergeTree"
            }
        }));
    }

    let ddl_sql = format!(
        "SHOW CREATE TABLE `{}`.`{}` FORMAT JSONCompactEachRow",
        db_name, tbl_name
    );
    let mut ddl_str = String::new();
    if let Ok(text) = client.post_sql(&ddl_sql, |_| {}).await {
        if let Ok(parsed) = crate::mapper::row_compact::parse_compact_output(&text, 0) {
            if let Some(row) = parsed.rows.first() {
                if let Some(v) = row.first().and_then(|x| x.as_str()) {
                    ddl_str = v.to_string();
                }
            }
        }
    }

    let cols_sql = format!(
        "SELECT name, type, comment FROM system.columns WHERE database = '{}' AND table = '{}' ORDER BY position FORMAT JSONCompactEachRowWithNamesAndTypes",
        db_name, tbl_name
    );
    let mut columns = Vec::new();
    if let Ok(text) = client.post_sql(&cols_sql, |_| {}).await {
        if let Ok(parsed) = crate::mapper::row_compact::parse_compact_output(&text, 0) {
            for row in parsed.rows {
                let name = row.first().and_then(|x| x.as_str()).unwrap_or("unknown");
                let col_type = row.get(1).and_then(|x| x.as_str()).unwrap_or("String");
                let comment = row.get(2).and_then(|x| x.as_str()).unwrap_or("");
                let is_nullable = col_type.starts_with("Nullable(");
                columns.push(json!({
                    "name": name,
                    "dataType": col_type,
                    "isNullable": is_nullable,
                    "comment": comment
                }));
            }
        }
    }

    Ok(json!({
        "nodeId": p.node_id,
        "nodeType": p.node_type,
        "ddl": ddl_str,
        "columns": columns,
        "properties": {}
    }))
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

    #[tokio::test]
    async fn test_handle_get_capabilities() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let res = handle_get_capabilities(None).await.unwrap();
        assert_eq!(res["supportsCancel"], true);
        assert_eq!(res["hasServerStats"], true);
    }

    #[tokio::test]
    async fn test_handle_get_server_stats_mock() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 404,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let res = handle_get_server_stats(Some(json!({ "connectionId": 404 })))
            .await
            .unwrap();
        assert_eq!(res["serverVersion"], "ClickHouse 24.3 (Mock)");
        assert_eq!(res["uptimeSeconds"], 3600);

        ConnectionPool::global().remove(404);
    }

    #[tokio::test]
    async fn test_handle_get_object_metadata_mock() {
        let _guard = crate::utils::test_lock::GLOBAL_TEST_LOCK.lock().await;
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 405,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        })
        .unwrap();
        ConnectionPool::global().insert(client);

        let res = handle_get_object_metadata(Some(json!({
            "connectionId": 405,
            "nodeId": "table.analytics.events",
            "nodeType": "table"
        })))
        .await
        .unwrap();
        assert_eq!(res["nodeId"], "table.analytics.events");
        assert!(res["ddl"].as_str().unwrap().contains("CREATE TABLE"));
        assert_eq!(res["columns"].as_array().unwrap().len(), 2);

        ConnectionPool::global().remove(405);
    }
}
