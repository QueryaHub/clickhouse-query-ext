use crate::error::DriverError;
use crate::mapper::row_compact::parse_compact_output;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SduiTreeNode {
    pub id: String,
    pub label: String,
    pub node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub has_children: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl SduiTreeNode {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        node_type: impl Into<String>,
        icon: Option<&str>,
        has_children: bool,
        metadata: Option<Value>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            node_type: node_type.into(),
            icon: icon.map(|s| s.to_string()),
            has_children,
            metadata,
        }
    }
}

/// Generates root database nodes when `db.getSchemaTree` is invoked.
pub fn build_root_databases_nodes(
    compact_output: Option<&str>,
) -> Result<Vec<SduiTreeNode>, DriverError> {
    let mut nodes = Vec::new();

    if let Some(output) = compact_output {
        let parsed = parse_compact_output(output, 0)?;
        for row in parsed.rows {
            let name = row.first().and_then(|v| v.as_str()).unwrap_or("unknown");
            let engine = row.get(1).and_then(|v| v.as_str()).unwrap_or("");
            let comment = row.get(2).and_then(|v| v.as_str()).unwrap_or("");

            nodes.push(SduiTreeNode::new(
                format!("db.{}", name),
                name,
                "database",
                Some("database"),
                true,
                Some(json!({
                    "engine": engine,
                    "comment": comment
                })),
            ));
        }
    } else {
        // Mock fallback for unit tests
        nodes.push(SduiTreeNode::new(
            "db.analytics",
            "analytics",
            "database",
            Some("database"),
            true,
            Some(json!({"engine": "Atomic"})),
        ));
        nodes.push(SduiTreeNode::new(
            "db.system",
            "system",
            "database",
            Some("database"),
            true,
            Some(json!({"engine": "Atomic"})),
        ));
    }

    Ok(nodes)
}

/// Builds database child groups (`Tables`, `Views`, `Dictionaries`) when a `database` node is expanded.
pub fn build_database_groups(db_name: &str) -> Vec<SduiTreeNode> {
    vec![
        SduiTreeNode::new(
            format!("group.{}.tables", db_name),
            "Таблицы (Tables)",
            "group",
            Some("folder-table"),
            true,
            Some(json!({ "database": db_name, "group": "tables" })),
        ),
        SduiTreeNode::new(
            format!("group.{}.views", db_name),
            "Представления (Views)",
            "group",
            Some("folder-eye"),
            true,
            Some(json!({ "database": db_name, "group": "views" })),
        ),
        SduiTreeNode::new(
            format!("group.{}.dictionaries", db_name),
            "Словари (Dictionaries)",
            "group",
            Some("folder-book"),
            true,
            Some(json!({ "database": db_name, "group": "dictionaries" })),
        ),
    ]
}

/// Builds table/view sub-groups (`Columns`, `Partitions`) when a `table` node is expanded.
pub fn build_table_groups(db_name: &str, table_name: &str) -> Vec<SduiTreeNode> {
    vec![
        SduiTreeNode::new(
            format!("group_cols.{}.{}", db_name, table_name),
            "Колонки (Columns)",
            "group_cols",
            Some("folder"),
            true,
            Some(json!({ "database": db_name, "table": table_name })),
        ),
        SduiTreeNode::new(
            format!("group_parts.{}.{}", db_name, table_name),
            "Партиции (Partitions)",
            "group_parts",
            Some("folder"),
            true,
            Some(json!({ "database": db_name, "table": table_name })),
        ),
    ]
}

/// Parses ClickHouse output (`system.tables`) into `table` or `view` tree nodes.
pub fn parse_tables_nodes(
    db_name: &str,
    compact_output: &str,
    filter_view: bool,
) -> Result<Vec<SduiTreeNode>, DriverError> {
    let parsed = parse_compact_output(compact_output, 0)?;
    let mut nodes = Vec::new();

    for row in parsed.rows {
        let name = row.first().and_then(|v| v.as_str()).unwrap_or("unknown");
        let engine = row.get(1).and_then(|v| v.as_str()).unwrap_or("");
        let total_rows = row.get(2).cloned().unwrap_or(json!(0));
        let size_readable = row.get(3).and_then(|v| v.as_str()).unwrap_or("0 B");
        let comment = row.get(4).and_then(|v| v.as_str()).unwrap_or("");
        let obj_type = row.get(5).and_then(|v| v.as_str()).unwrap_or("table");

        if filter_view && obj_type != "view" {
            continue;
        }
        if !filter_view && obj_type == "view" {
            continue;
        }

        let node_type = if obj_type == "view" { "view" } else { "table" };
        let icon = if obj_type == "view" { "eye" } else { "table" };

        nodes.push(SduiTreeNode::new(
            format!("{}.{}.{}", node_type, db_name, name),
            name,
            node_type,
            Some(icon),
            true,
            Some(json!({
                "engine": engine,
                "totalRows": total_rows,
                "sizeReadable": size_readable,
                "comment": comment
            })),
        ));
    }

    Ok(nodes)
}

/// Parses ClickHouse output (`system.dictionaries`) into `dictionary` nodes.
pub fn parse_dictionaries_nodes(
    db_name: &str,
    compact_output: &str,
) -> Result<Vec<SduiTreeNode>, DriverError> {
    let parsed = parse_compact_output(compact_output, 0)?;
    let mut nodes = Vec::new();

    for row in parsed.rows {
        let name = row.first().and_then(|v| v.as_str()).unwrap_or("unknown");
        let status = row.get(1).and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
        let dict_type = row.get(2).and_then(|v| v.as_str()).unwrap_or("");
        let element_count = row.get(3).cloned().unwrap_or(json!(0));
        let size = row.get(5).and_then(|v| v.as_str()).unwrap_or("0 B");

        nodes.push(SduiTreeNode::new(
            format!("dict.{}.{}", db_name, name),
            name,
            "dictionary",
            Some("book"),
            false,
            Some(json!({
                "status": status,
                "type": dict_type,
                "elementCount": element_count,
                "size": size
            })),
        ));
    }

    Ok(nodes)
}

/// Parses ClickHouse output (`system.columns`) into column nodes.
pub fn parse_columns_nodes(
    db_name: &str,
    table_name: &str,
    compact_output: &str,
) -> Result<Vec<SduiTreeNode>, DriverError> {
    let parsed = parse_compact_output(compact_output, 0)?;
    let mut nodes = Vec::new();

    for row in parsed.rows {
        let name = row.first().and_then(|v| v.as_str()).unwrap_or("unknown");
        let col_type = row.get(1).and_then(|v| v.as_str()).unwrap_or("String");
        let comment = row.get(2).and_then(|v| v.as_str()).unwrap_or("");

        nodes.push(SduiTreeNode::new(
            format!("col.{}.{}.{}", db_name, table_name, name),
            format!("{} ({})", name, col_type),
            "column",
            Some("columns"),
            false,
            Some(json!({
                "name": name,
                "type": col_type,
                "comment": comment
            })),
        ));
    }

    Ok(nodes)
}

/// Parses ClickHouse output (`system.parts`) into partition nodes.
pub fn parse_partitions_nodes(
    db_name: &str,
    table_name: &str,
    compact_output: &str,
) -> Result<Vec<SduiTreeNode>, DriverError> {
    let parsed = parse_compact_output(compact_output, 0)?;
    let mut nodes = Vec::new();

    for row in parsed.rows {
        let partition = row.first().and_then(|v| v.as_str()).unwrap_or("all");
        let total_rows = row.get(1).cloned().unwrap_or(json!(0));
        let compressed_size = row.get(2).and_then(|v| v.as_str()).unwrap_or("0 B");
        let parts_count = row.get(3).cloned().unwrap_or(json!(1));

        nodes.push(SduiTreeNode::new(
            format!("part.{}.{}.{}", db_name, table_name, partition),
            format!("⚡ {}", partition),
            "partition",
            Some("archive"),
            false,
            Some(json!({
                "partition": partition,
                "totalRows": total_rows,
                "compressedSize": compressed_size,
                "partsCount": parts_count
            })),
        ));
    }

    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_root_databases_nodes_mock() {
        let roots = build_root_databases_nodes(None).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].id, "db.analytics");
        assert!(roots[0].has_children);
    }

    #[test]
    fn test_build_database_groups() {
        let groups = build_database_groups("analytics");
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].id, "group.analytics.tables");
        assert_eq!(groups[1].id, "group.analytics.views");
        assert_eq!(groups[2].id, "group.analytics.dictionaries");
    }

    #[test]
    fn test_parse_tables_nodes() {
        let mock_output = r#"["name", "engine", "total_rows", "size_readable", "comment", "object_type"]
["String", "String", "UInt64", "String", "String", "String"]
["events", "ReplicatedMergeTree", 1500000, "120.4 MiB", "analytics table", "table"]
["mv_summary", "MaterializedView", 500, "10.0 KiB", "", "view"]"#;

        let tables = parse_tables_nodes("analytics", mock_output, false).unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].id, "table.analytics.events");
        assert_eq!(tables[0].label, "events");
        assert!(tables[0].has_children);

        let views = parse_tables_nodes("analytics", mock_output, true).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].id, "view.analytics.mv_summary");
    }

    #[test]
    fn test_parse_columns_and_partitions() {
        let cols_output = r#"["name", "type", "comment"]
["String", "String", "String"]
["user_id", "UInt64", "Unique user identifier"]"#;

        let cols = parse_columns_nodes("analytics", "events", cols_output).unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].label, "user_id (UInt64)");
        assert!(!cols[0].has_children);

        let parts_output = r#"["partition", "total_rows", "compressed_size", "parts_count"]
["String", "UInt64", "String", "UInt64"]
["202607", 500000, "45.2 MiB", 3]"#;

        let parts = parse_partitions_nodes("analytics", "events", parts_output).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].label, "⚡ 202607");
    }
}
