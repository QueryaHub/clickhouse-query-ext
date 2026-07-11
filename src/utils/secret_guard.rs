use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use secrecy::{ExposeSecret, SecretString};

/// Secure container for connection credentials (`password` and/or `jwt_token`).
/// Uses `secrecy::SecretString` which guarantees automatic zeroization (`zeroize`) of heap memory upon drop
/// and prevents accidental leakage via `Debug` formatting.
#[derive(Clone, Default)]
pub struct ConnectionSecrets {
    pub password: Option<SecretString>,
    pub jwt_token: Option<SecretString>,
}

impl std::fmt::Debug for ConnectionSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionSecrets")
            .field("password", &self.password.as_ref().map(|_| "[REDACTED BY SECRECY]"))
            .field("jwt_token", &self.jwt_token.as_ref().map(|_| "[REDACTED BY SECRECY]"))
            .finish()
    }
}

impl ConnectionSecrets {
    /// Helper to securely expose password as string slice.
    pub fn expose_password(&self) -> Option<&str> {
        self.password.as_ref().map(|s| s.expose_secret().as_str())
    }

    /// Helper to securely expose jwt_token as string slice.
    pub fn expose_jwt_token(&self) -> Option<&str> {
        self.jwt_token.as_ref().map(|s| s.expose_secret().as_str())
    }
}

/// Thread-safe in-memory registry of zero-trust connection secrets (`ConnectionSecretsPool`).
#[derive(Default, Clone)]
pub struct ConnectionSecretsPool {
    inner: Arc<RwLock<HashMap<u64, ConnectionSecrets>>>,
}

impl ConnectionSecretsPool {
    /// Returns the global singleton instance of the `ConnectionSecretsPool`.
    pub fn global() -> &'static ConnectionSecretsPool {
        static POOL: OnceLock<ConnectionSecretsPool> = OnceLock::new();
        POOL.get_or_init(ConnectionSecretsPool::default)
    }

    /// Store or update credentials for the given `connection_id`.
    /// Any previous credentials for this ID are dropped and immediately zeroized in memory.
    pub fn inject(&self, connection_id: u64, password: Option<String>, jwt_token: Option<String>) {
        let mut map = self.inner.write().unwrap();
        map.insert(
            connection_id,
            ConnectionSecrets {
                password: password.map(SecretString::new),
                jwt_token: jwt_token.map(SecretString::new),
            },
        );
    }

    /// Retrieve a clone of the `ConnectionSecrets` handle for the given `connection_id`.
    pub fn get(&self, connection_id: u64) -> Option<ConnectionSecrets> {
        let map = self.inner.read().unwrap();
        map.get(&connection_id).cloned()
    }

    /// Remove and zeroize stored credentials for the given `connection_id` (e.g. upon `db.disconnect`).
    pub fn remove(&self, connection_id: u64) -> bool {
        let mut map = self.inner.write().unwrap();
        map.remove(&connection_id).is_some()
    }

    /// Clear all stored credentials and zeroize their heap allocations (e.g. upon `system.shutdown`).
    pub fn clear_all(&self) {
        let mut map = self.inner.write().unwrap();
        map.clear();
    }

    /// Returns the number of currently active secret entries in memory.
    pub fn count(&self) -> usize {
        let map = self.inner.read().unwrap();
        map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_and_retrieve_secrets() {
        let pool = ConnectionSecretsPool::default();
        pool.inject(
            101,
            Some("SuperSecretClickHousePass!".to_string()),
            Some("jwt.token.payload".to_string()),
        );

        assert_eq!(pool.count(), 1);
        let secrets = pool.get(101).expect("Secrets should exist for id 101");
        assert_eq!(secrets.expose_password(), Some("SuperSecretClickHousePass!"));
        assert_eq!(secrets.expose_jwt_token(), Some("jwt.token.payload"));
    }

    #[test]
    fn test_secret_string_does_not_leak_in_debug() {
        let secrets = ConnectionSecrets {
            password: Some(SecretString::new("TopSecret".to_string())),
            jwt_token: None,
        };
        let debug_str = format!("{:?}", secrets);
        assert!(!debug_str.contains("TopSecret"));
        assert!(debug_str.contains("[REDACTED BY SECRECY]"));
    }

    #[test]
    fn test_remove_and_clear_all() {
        let pool = ConnectionSecretsPool::default();
        pool.inject(1, Some("pass1".to_string()), None);
        pool.inject(2, Some("pass2".to_string()), None);
        assert_eq!(pool.count(), 2);

        assert!(pool.remove(1));
        assert_eq!(pool.count(), 1);
        assert!(pool.get(1).is_none());

        pool.clear_all();
        assert_eq!(pool.count(), 0);
        assert!(pool.get(2).is_none());
    }
}
