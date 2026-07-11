use crate::error::DriverError;
use crate::utils::secret_guard::ConnectionSecretsPool;
use reqwest::{Client, ClientBuilder};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConnectParams {
    pub connection_id: u64,
    pub connection_string: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub database: Option<String>,
    pub readonly: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ClickHouseClient {
    pub connection_id: u64,
    pub base_url: String,
    pub user: String,
    pub database: String,
    pub readonly: bool,
    pub http_client: Client,
}

impl ClickHouseClient {
    /// Construct a new `ClickHouseClient` and underlying HTTP connection pool from `ConnectParams`.
    pub fn from_params(params: ConnectParams) -> Result<Self, DriverError> {
        let (base_url, user, database, readonly) = if let Some(cs) = params.connection_string {
            if cs.starts_with("mock://") || cs.starts_with("test://") {
                let u = params.user.unwrap_or_else(|| "default".to_string());
                let db = params.database.unwrap_or_else(|| "default".to_string());
                (cs, u, db, params.readonly.unwrap_or(true))
            } else {
                let parsed = Url::parse(&cs)?;
                let host = parsed.host_str().unwrap_or("localhost");
                let port = parsed.port().unwrap_or(8123);
                let scheme = parsed.scheme();
                let user = if !parsed.username().is_empty() {
                    parsed.username().to_string()
                } else {
                    params.user.unwrap_or_else(|| "default".to_string())
                };
                let db_path = parsed.path().trim_start_matches('/');
                let database = if !db_path.is_empty() {
                    db_path.to_string()
                } else {
                    params.database.unwrap_or_else(|| "default".to_string())
                };
                // Safe Mode: default to readonly = true unless explicitly disabled
                let readonly = params.readonly.unwrap_or(true);
                let base = format!("{}://{}:{}", scheme, host, port);
                (base, user, database, readonly)
            }
        } else {
            let host = params.host.unwrap_or_else(|| "localhost".to_string());
            let port = params.port.unwrap_or(8123);
            let user = params.user.unwrap_or_else(|| "default".to_string());
            let database = params.database.unwrap_or_else(|| "default".to_string());
            let readonly = params.readonly.unwrap_or(true);
            (
                format!("http://{}:{}", host, port),
                user,
                database,
                readonly,
            )
        };

        let http_client = ClientBuilder::new()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(5))
            .build()?;

        Ok(Self {
            connection_id: params.connection_id,
            base_url,
            user,
            database,
            readonly,
            http_client,
        })
    }

    /// Check connection health by executing `SELECT version()` against ClickHouse.
    /// Utilizes zero-trust `ConnectionSecretsPool` for authentication headers without persisting secrets in struct memory.
    pub async fn ping_connection(&self) -> Result<String, DriverError> {
        if self.base_url.starts_with("mock://") || self.base_url.starts_with("test://") {
            return Ok("mock-clickhouse-23.8.1.1".to_string());
        }

        let mut url = Url::parse(&self.base_url)?;
        url.query_pairs_mut()
            .append_pair("query", "SELECT version()")
            .append_pair("database", &self.database);
        if self.readonly {
            url.query_pairs_mut().append_pair("readonly", "1");
        }

        let mut req = self.http_client.get(url);

        // Retrieve secret securely just-in-time from ConnectionSecretsPool
        if let Some(secrets) = ConnectionSecretsPool::global().get(self.connection_id) {
            if let Some(jwt) = secrets.expose_jwt_token() {
                req = req.header("Authorization", format!("Bearer {}", jwt));
            } else if let Some(pass) = secrets.expose_password() {
                req = req
                    .header("X-ClickHouse-User", &self.user)
                    .header("X-ClickHouse-Key", pass);
            } else {
                req = req.header("X-ClickHouse-User", &self.user);
            }
        } else {
            req = req.header("X-ClickHouse-User", &self.user);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(DriverError::Client(format!(
                "ClickHouse HTTP error {}: {}",
                status, text
            )));
        }

        let version = resp.text().await?.trim().to_string();
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_params_connection_string() {
        let params = ConnectParams {
            connection_id: 1,
            connection_string: Some("http://admin@localhost:8123/analytics?readonly=1".to_string()),
            ..Default::default()
        };
        let client = ClickHouseClient::from_params(params).unwrap();
        assert_eq!(client.connection_id, 1);
        assert_eq!(client.base_url, "http://localhost:8123");
        assert_eq!(client.user, "admin");
        assert_eq!(client.database, "analytics");
        assert!(client.readonly);
    }

    #[tokio::test]
    async fn test_ping_mock_connection() {
        let params = ConnectParams {
            connection_id: 2,
            connection_string: Some("mock://localhost:8123/default".to_string()),
            ..Default::default()
        };
        let client = ClickHouseClient::from_params(params).unwrap();
        let ver = client.ping_connection().await.unwrap();
        assert_eq!(ver, "mock-clickhouse-23.8.1.1");
    }
}
