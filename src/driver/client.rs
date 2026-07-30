use crate::error::DriverError;
use crate::utils::secret_guard::ConnectionSecretsPool;
use reqwest::{Client, ClientBuilder, RequestBuilder};
use serde::Deserialize;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::Url;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConnectParams {
    pub connection_id: u64,
    pub connection_string: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    #[serde(alias = "username")]
    pub user: Option<String>,
    pub database: Option<String>,
    #[serde(alias = "safe_mode", alias = "safeMode")]
    pub readonly: Option<bool>,
    #[serde(alias = "sslMode")]
    pub ssl_mode: Option<String>,
}

#[derive(Debug)]
pub struct ClickHouseClient {
    pub connection_id: u64,
    pub base_url: String,
    pub user: String,
    pub database: String,
    pub readonly: bool,
    /// When true, omit `readonly=1` on HTTP requests because the server/user
    /// profile already enforces read-only mode.
    server_enforces_readonly: AtomicBool,
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
                let scheme = parsed.scheme();
                let port_str = match parsed.port() {
                    Some(p) => format!(":{}", p),
                    None => {
                        if scheme == "http" {
                            ":8123".to_string()
                        } else {
                            String::new()
                        }
                    }
                };
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
                let readonly = params.readonly.unwrap_or(false);
                let base = format!("{}://{}{}", scheme, host, port_str);
                (base, user, database, readonly)
            }
        } else {
            let host = params.host.unwrap_or_else(|| "localhost".to_string());
            let scheme = match params.ssl_mode.as_deref() {
                Some("prefer") | Some("require") => "https",
                _ => "http",
            };
            let mut port = params.port.unwrap_or(8123);
            if scheme == "https" && port == 8123 {
                port = 8443;
            }
            let user = params.user.unwrap_or_else(|| "default".to_string());
            let database = params.database.unwrap_or_else(|| "default".to_string());
            let readonly = params.readonly.unwrap_or(false);
            (
                format!("{}://{}:{}", scheme, host, port),
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
            server_enforces_readonly: AtomicBool::new(false),
            http_client,
        })
    }

    /// Appends Safe Mode session settings to a ClickHouse HTTP URL.
    pub fn append_safe_mode_settings(&self, url: &mut Url) {
        self.append_safe_mode_settings_with(url, self.omit_readonly_setting());
    }

    fn omit_readonly_setting(&self) -> bool {
        self.server_enforces_readonly.load(Ordering::Relaxed)
    }

    fn append_safe_mode_settings_with(&self, url: &mut Url, omit_readonly_setting: bool) {
        if !self.readonly {
            return;
        }
        if !omit_readonly_setting {
            url.query_pairs_mut().append_pair("readonly", "1");
        }
        url.query_pairs_mut()
            .append_pair("max_execution_time", "300")
            .append_pair("max_memory_usage", "10000000000");
    }

    fn mark_server_readonly_enforced(&self) {
        self.server_enforces_readonly.store(true, Ordering::Relaxed);
    }

    /// Returns true when ClickHouse rejects `readonly=1` because the session is
    /// already read-only at server/profile level.
    pub fn is_readonly_setting_conflict(err: &DriverError) -> bool {
        match err {
            DriverError::Client(msg) => {
                msg.contains("Cannot modify 'readonly' setting in readonly mode")
                    || msg.contains("Code: 164")
                    || msg.contains("(READONLY)")
            }
            _ => false,
        }
    }

    fn apply_auth(&self, mut req: RequestBuilder) -> RequestBuilder {
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
        req
    }

    async fn read_response(resp: reqwest::Response) -> Result<String, DriverError> {
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(DriverError::Client(format!(
                "ClickHouse HTTP error {}: {}",
                status, text
            )));
        }
        Ok(resp.text().await?)
    }

    pub async fn execute_with_readonly_retry<F, Fut>(
        &self,
        mut run: F,
    ) -> Result<String, DriverError>
    where
        F: FnMut(bool) -> Fut,
        Fut: Future<Output = Result<String, DriverError>>,
    {
        let mut omit = self.omit_readonly_setting();
        loop {
            match run(omit).await {
                Ok(text) => return Ok(text),
                Err(err) if self.readonly && !omit && Self::is_readonly_setting_conflict(&err) => {
                    self.mark_server_readonly_enforced();
                    omit = true;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Executes a GET request with optional SQL in the `query` URL parameter.
    pub async fn get_with_query(&self, sql: &str) -> Result<String, DriverError> {
        if self.base_url.starts_with("mock://") || self.base_url.starts_with("test://") {
            return Ok("mock-clickhouse-23.8.1.1".to_string());
        }

        self.execute_with_readonly_retry(|omit_readonly| async move {
            let mut url = Url::parse(&self.base_url)?;
            url.query_pairs_mut()
                .append_pair("query", sql)
                .append_pair("database", &self.database);
            self.append_safe_mode_settings_with(&mut url, omit_readonly);
            let req = self.apply_auth(self.http_client.get(url));
            Self::read_response(req.send().await?).await
        })
        .await
    }

    /// Executes a POST request with SQL in the request body.
    pub async fn post_sql(
        &self,
        sql: &str,
        mut extra_params: impl FnMut(&mut Url),
    ) -> Result<String, DriverError> {
        if self.base_url.starts_with("mock://") || self.base_url.starts_with("test://") {
            return Ok(String::new());
        }

        let sql = sql.to_string();
        let mut omit = self.omit_readonly_setting();
        loop {
            let mut url = Url::parse(&self.base_url)?;
            url.query_pairs_mut()
                .append_pair("database", &self.database);
            extra_params(&mut url);
            self.append_safe_mode_settings_with(&mut url, omit);
            let req = self
                .apply_auth(self.http_client.post(url))
                .body(sql.clone());
            match Self::read_response(req.send().await?).await {
                Ok(text) => return Ok(text),
                Err(err) if self.readonly && !omit && Self::is_readonly_setting_conflict(&err) => {
                    self.mark_server_readonly_enforced();
                    omit = true;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Check connection health by executing `SELECT version()` against ClickHouse.
    pub async fn ping_connection(&self) -> Result<String, DriverError> {
        let version = self.get_with_query("SELECT version()").await?;
        Ok(version.trim().to_string())
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
            readonly: Some(true),
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

    #[test]
    fn test_from_params_safe_mode_aliases() {
        let json_val = serde_json::json!({
            "connectionId": 10,
            "host": "localhost",
            "safe_mode": true
        });
        let params: ConnectParams = serde_json::from_value(json_val).unwrap();
        let client = ClickHouseClient::from_params(params).unwrap();
        assert!(client.readonly);

        let json_val_off = serde_json::json!({
            "connectionId": 11,
            "host": "localhost",
            "safeMode": false
        });
        let params_off: ConnectParams = serde_json::from_value(json_val_off).unwrap();
        let client_off = ClickHouseClient::from_params(params_off).unwrap();
        assert!(!client_off.readonly);
    }

    #[test]
    fn test_append_safe_mode_settings_omits_readonly_when_server_enforces() {
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 3,
            host: Some("localhost".to_string()),
            readonly: Some(true),
            ..Default::default()
        })
        .unwrap();
        client.mark_server_readonly_enforced();

        let mut url = Url::parse("http://localhost:8123").unwrap();
        client.append_safe_mode_settings(&mut url);
        let query = url.query().unwrap_or_default();
        assert!(!query.contains("readonly=1"));
        assert!(query.contains("max_execution_time=300"));
    }

    #[test]
    fn test_is_readonly_setting_conflict() {
        let err = DriverError::Client(
            "ClickHouse HTTP error 500 Internal Server Error: Code: 164. DB::Exception: Cannot modify 'readonly' setting in readonly mode. (READONLY)".to_string(),
        );
        assert!(ClickHouseClient::is_readonly_setting_conflict(&err));
    }
}
