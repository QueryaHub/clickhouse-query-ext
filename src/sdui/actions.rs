use crate::error::DriverError;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SduiContextAction {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    pub requires_confirmation: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub danger: bool,
}

impl SduiContextAction {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        icon: Option<&str>,
        action_type: impl Into<String>,
        sql: Option<String>,
        requires_confirmation: bool,
        danger: bool,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: icon.map(|s| s.to_string()),
            action_type: action_type.into(),
            sql,
            requires_confirmation,
            danger,
        }
    }
}

/// Generates SDUI context menu actions based on `nodeType` and `nodeId`.
pub fn get_context_actions_for_node(
    node_type: &str,
    node_id: &str,
) -> Result<Vec<SduiContextAction>, DriverError> {
    let parts: Vec<&str> = node_id.split('.').collect();

    match node_type {
        "server" | "root_databases" => Ok(vec![
            SduiContextAction::new(
                "server.active_mutations",
                "🔄 All Active Mutations (SYSTEM.mutations)",
                Some("activity"),
                "query",
                Some("SELECT mutation_id, database, table, command, create_time, parts_to_do FROM system.mutations WHERE is_done = 0 ORDER BY create_time ASC".to_string()),
                false,
                false,
            ),
            SduiContextAction::new(
                "server.active_queries",
                "⚡ All Active Queries (SYSTEM.processes)",
                Some("cpu"),
                "query",
                Some("SELECT query_id, user, query, elapsed, formatReadableSize(memory_usage) AS mem FROM system.processes WHERE query NOT LIKE '%system.processes%' ORDER BY elapsed DESC".to_string()),
                false,
                false,
            ),
            SduiContextAction::new(
                "server.kill_long_queries",
                "🛑 Kill Long Running Queries (>60s)",
                Some("x-circle"),
                "execute",
                Some("KILL QUERY WHERE elapsed > 60 AND query NOT LIKE '%KILL QUERY%' ASYNC".to_string()),
                true,
                true,
            ),
        ]),
        "table" => {
            if parts.len() < 3 {
                return Err(DriverError::Client(format!(
                    "Invalid nodeId for table context actions: '{}'",
                    node_id
                )));
            }
            let db_name = parts[1];
            let table_name = parts[2];

            Ok(vec![
                SduiContextAction::new(
                    "table.top_100",
                    "⚡ Top 100 Rows",
                    Some("table"),
                    "query",
                    Some(format!(
                        "SELECT * FROM {}.{} LIMIT 100",
                        db_name, table_name
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "table.show_ddl",
                    "📜 Show DDL (SHOW CREATE TABLE)",
                    Some("code"),
                    "query",
                    Some(format!("SHOW CREATE TABLE {}.{}", db_name, table_name)),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "table.col_stats",
                    "📈 Column Statistics (Быстрый профайлер)",
                    Some("bar-chart"),
                    "modal",
                    None,
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "table.optimize_final",
                    "🔨 Optimize Table (FINAL)",
                    Some("tool"),
                    "execute",
                    Some(format!("OPTIMIZE TABLE {}.{} FINAL", db_name, table_name)),
                    true,
                    false,
                ),
                SduiContextAction::new(
                    "table.deduplicate",
                    "🧹 Deduplicate (DEDUPLICATE)",
                    Some("filter"),
                    "execute",
                    Some(format!(
                        "OPTIMIZE TABLE {}.{} DEDUPLICATE",
                        db_name, table_name
                    )),
                    true,
                    false,
                ),
                SduiContextAction::new(
                    "table.active_mutations",
                    "🔄 Active Mutations for Table",
                    Some("activity"),
                    "query",
                    Some(format!(
                        "SELECT mutation_id, command, create_time, parts_to_do, is_done FROM system.mutations WHERE database = '{}' AND table = '{}' AND is_done = 0",
                        db_name, table_name
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "table.active_queries",
                    "⚡ Active Queries for Table",
                    Some("cpu"),
                    "query",
                    Some(format!(
                        "SELECT query_id, user, query, elapsed, formatReadableSize(memory_usage) AS mem FROM system.processes WHERE current_database = '{}' AND query LIKE '%{}%' AND query NOT LIKE '%system.processes%'",
                        db_name, table_name
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "table.kill_mutations",
                    "🛑 Kill Mutations for Table",
                    Some("x-circle"),
                    "execute",
                    Some(format!(
                        "KILL MUTATION WHERE database = '{}' AND table = '{}'",
                        db_name, table_name
                    )),
                    true,
                    true,
                ),
            ])
        }
        "view" => {
            if parts.len() < 3 {
                return Err(DriverError::Client(format!(
                    "Invalid nodeId for view context actions: '{}'",
                    node_id
                )));
            }
            let db_name = parts[1];
            let view_name = parts[2];

            Ok(vec![
                SduiContextAction::new(
                    "view.top_100",
                    "⚡ Top 100 Rows",
                    Some("eye"),
                    "query",
                    Some(format!("SELECT * FROM {}.{} LIMIT 100", db_name, view_name)),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "view.show_ddl",
                    "📜 Show DDL (SHOW CREATE TABLE)",
                    Some("code"),
                    "query",
                    Some(format!("SHOW CREATE TABLE {}.{}", db_name, view_name)),
                    false,
                    false,
                ),
            ])
        }
        "partition" => {
            if parts.len() < 4 {
                return Err(DriverError::Client(format!(
                    "Invalid nodeId for partition context actions: '{}'",
                    node_id
                )));
            }
            let db_name = parts[1];
            let table_name = parts[2];
            let partition = parts[3];

            Ok(vec![
                SduiContextAction::new(
                    "partition.drop",
                    format!("🗑️ Drop Partition '{}'", partition),
                    Some("trash-2"),
                    "execute",
                    Some(format!(
                        "ALTER TABLE {}.{} DROP PARTITION '{}'",
                        db_name, table_name, partition
                    )),
                    true,
                    true,
                ),
                SduiContextAction::new(
                    "partition.freeze",
                    format!("❄️ Freeze Partition '{}' (Backup)", partition),
                    Some("save"),
                    "execute",
                    Some(format!(
                        "ALTER TABLE {}.{} FREEZE PARTITION '{}'",
                        db_name, table_name, partition
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "partition.detach",
                    format!("🔌 Detach Partition '{}'", partition),
                    Some("log-out"),
                    "execute",
                    Some(format!(
                        "ALTER TABLE {}.{} DETACH PARTITION '{}'",
                        db_name, table_name, partition
                    )),
                    true,
                    true,
                ),
                SduiContextAction::new(
                    "partition.attach",
                    format!("🔗 Attach Partition '{}'", partition),
                    Some("log-in"),
                    "execute",
                    Some(format!(
                        "ALTER TABLE {}.{} ATTACH PARTITION '{}'",
                        db_name, table_name, partition
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "partition.optimize_final",
                    format!("🔨 Optimize Partition '{}' (FINAL)", partition),
                    Some("tool"),
                    "execute",
                    Some(format!(
                        "OPTIMIZE TABLE {}.{} PARTITION '{}' FINAL",
                        db_name, table_name, partition
                    )),
                    true,
                    false,
                ),
                SduiContextAction::new(
                    "partition.deduplicate",
                    format!("🧹 Deduplicate Partition '{}' (DEDUPLICATE)", partition),
                    Some("filter"),
                    "execute",
                    Some(format!(
                        "OPTIMIZE TABLE {}.{} PARTITION '{}' DEDUPLICATE",
                        db_name, table_name, partition
                    )),
                    true,
                    false,
                ),
            ])
        }
        "database" => {
            if parts.len() < 2 {
                return Err(DriverError::Client(format!(
                    "Invalid nodeId for database context actions: '{}'",
                    node_id
                )));
            }
            let db_name = parts[1];

            Ok(vec![
                SduiContextAction::new(
                    "db.active_mutations",
                    "🔄 Active Mutations in Database",
                    Some("activity"),
                    "query",
                    Some(format!(
                        "SELECT mutation_id, table, command, create_time, parts_to_do FROM system.mutations WHERE database = '{}' AND is_done = 0",
                        db_name
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "db.active_queries",
                    "⚡ Active Queries in Database",
                    Some("cpu"),
                    "query",
                    Some(format!(
                        "SELECT query_id, user, query, elapsed, formatReadableSize(memory_usage) AS mem FROM system.processes WHERE current_database = '{}'",
                        db_name
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "db.kill_mutations",
                    "🛑 Kill Mutations in Database",
                    Some("x-circle"),
                    "execute",
                    Some(format!("KILL MUTATION WHERE database = '{}'", db_name)),
                    true,
                    true,
                ),
                SduiContextAction::new(
                    "db.kill_queries",
                    "🛑 Kill Queries in Database",
                    Some("x-circle"),
                    "execute",
                    Some(format!("KILL QUERY WHERE current_database = '{}' ASYNC", db_name)),
                    true,
                    true,
                ),
            ])
        }
        "column" => {
            if parts.len() < 4 {
                return Err(DriverError::Client(format!(
                    "Invalid nodeId for column context actions: '{}'",
                    node_id
                )));
            }
            let db_name = parts[1];
            let table_name = parts[2];
            let col_name = parts[3];

            Ok(vec![
                SduiContextAction::new(
                    "column.stats",
                    "📈 Column Statistics (Быстрый профайлер)",
                    Some("bar-chart"),
                    "query",
                    Some(format!(
                        "SELECT count() as total_rows, countIf(isNotNull({0})) as not_nulls, uniqExact({0}) as unique_exact, min({0}) as min_val, max({0}) as max_val, topK(5)({0}) as top_5_values FROM {1}.{2}",
                        col_name, db_name, table_name
                    )),
                    false,
                    false,
                ),
                SduiContextAction::new(
                    "column.top_10",
                    "🔝 Top 10 Frequent Values",
                    Some("list"),
                    "query",
                    Some(format!(
                        "SELECT {0}, count() as cnt FROM {1}.{2} GROUP BY {0} ORDER BY cnt DESC LIMIT 10",
                        col_name, db_name, table_name
                    )),
                    false,
                    false,
                ),
            ])
        }
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_context_actions() {
        let actions = get_context_actions_for_node("table", "table.analytics.events").unwrap();
        assert_eq!(actions.len(), 8);
        assert_eq!(actions[0].id, "table.top_100");
        assert_eq!(
            actions[0].sql.as_deref(),
            Some("SELECT * FROM analytics.events LIMIT 100")
        );
        assert!(!actions[0].requires_confirmation);

        assert_eq!(actions[3].id, "table.optimize_final");
        assert_eq!(
            actions[3].sql.as_deref(),
            Some("OPTIMIZE TABLE analytics.events FINAL")
        );
        assert!(actions[3].requires_confirmation);

        assert_eq!(actions[6].id, "table.active_queries");
        assert_eq!(actions[7].id, "table.kill_mutations");
    }

    #[test]
    fn test_partition_context_actions() {
        let actions =
            get_context_actions_for_node("partition", "part.analytics.events.202607").unwrap();
        assert_eq!(actions.len(), 6);
        assert_eq!(actions[0].id, "partition.drop");
        assert_eq!(
            actions[0].sql.as_deref(),
            Some("ALTER TABLE analytics.events DROP PARTITION '202607'")
        );
        assert!(actions[0].requires_confirmation);
        assert!(actions[0].danger);

        assert_eq!(actions[1].id, "partition.freeze");
        assert_eq!(
            actions[1].sql.as_deref(),
            Some("ALTER TABLE analytics.events FREEZE PARTITION '202607'")
        );
        assert!(!actions[1].requires_confirmation);
        assert!(!actions[1].danger);

        assert_eq!(actions[2].id, "partition.detach");
        assert_eq!(
            actions[2].sql.as_deref(),
            Some("ALTER TABLE analytics.events DETACH PARTITION '202607'")
        );
        assert!(actions[2].requires_confirmation);
        assert!(actions[2].danger);

        assert_eq!(actions[3].id, "partition.attach");
        assert_eq!(
            actions[3].sql.as_deref(),
            Some("ALTER TABLE analytics.events ATTACH PARTITION '202607'")
        );
        assert!(!actions[3].requires_confirmation);
        assert!(!actions[3].danger);

        assert_eq!(actions[4].id, "partition.optimize_final");
        assert_eq!(
            actions[4].sql.as_deref(),
            Some("OPTIMIZE TABLE analytics.events PARTITION '202607' FINAL")
        );
        assert!(actions[4].requires_confirmation);

        assert_eq!(actions[5].id, "partition.deduplicate");
        assert_eq!(
            actions[5].sql.as_deref(),
            Some("OPTIMIZE TABLE analytics.events PARTITION '202607' DEDUPLICATE")
        );
        assert!(actions[5].requires_confirmation);
    }

    #[test]
    fn test_database_and_view_actions() {
        let db_actions = get_context_actions_for_node("database", "db.analytics").unwrap();
        assert_eq!(db_actions.len(), 4);
        assert_eq!(db_actions[0].id, "db.active_mutations");
        assert_eq!(db_actions[1].id, "db.active_queries");
        assert_eq!(db_actions[2].id, "db.kill_mutations");
        assert_eq!(db_actions[3].id, "db.kill_queries");

        let view_actions =
            get_context_actions_for_node("view", "view.analytics.mv_summary").unwrap();
        assert_eq!(view_actions.len(), 2);
        assert_eq!(view_actions[0].id, "view.top_100");
    }

    #[test]
    fn test_server_and_process_monitoring_actions() {
        let server_actions = get_context_actions_for_node("server", "server.cluster").unwrap();
        assert_eq!(server_actions.len(), 3);
        assert_eq!(server_actions[0].id, "server.active_mutations");
        assert_eq!(server_actions[1].id, "server.active_queries");
        assert_eq!(server_actions[2].id, "server.kill_long_queries");
        assert!(server_actions[2].danger);
    }

    #[test]
    fn test_column_context_actions() {
        let actions =
            get_context_actions_for_node("column", "col.analytics.events.user_id").unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].id, "column.stats");
        assert_eq!(
            actions[0].sql.as_deref(),
            Some(
                "SELECT count() as total_rows, countIf(isNotNull(user_id)) as not_nulls, uniqExact(user_id) as unique_exact, min(user_id) as min_val, max(user_id) as max_val, topK(5)(user_id) as top_5_values FROM analytics.events"
            )
        );
        assert_eq!(actions[0].action_type, "query");

        assert_eq!(actions[1].id, "column.top_10");
        assert_eq!(
            actions[1].sql.as_deref(),
            Some(
                "SELECT user_id, count() as cnt FROM analytics.events GROUP BY user_id ORDER BY cnt DESC LIMIT 10"
            )
        );
    }

    #[test]
    fn test_invalid_node_id() {
        assert!(get_context_actions_for_node("table", "table.only").is_err());
        assert!(get_context_actions_for_node("partition", "part.only.two").is_err());
        assert!(get_context_actions_for_node("column", "col.only.two").is_err());
    }
}
