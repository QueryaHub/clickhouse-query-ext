use crate::driver::client::ClickHouseClient;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// Global thread-safe registry of active `ClickHouseClient` instances (`db.*` sessions).
#[derive(Default, Clone)]
pub struct ConnectionPool {
    inner: Arc<RwLock<HashMap<u64, Arc<ClickHouseClient>>>>,
}

impl ConnectionPool {
    /// Returns the global singleton instance of the `ConnectionPool`.
    pub fn global() -> &'static ConnectionPool {
        static POOL: OnceLock<ConnectionPool> = OnceLock::new();
        POOL.get_or_init(ConnectionPool::default)
    }

    /// Register or replace a `ClickHouseClient` session in the pool.
    pub fn insert(&self, client: ClickHouseClient) {
        let id = client.connection_id;
        let mut map = self.inner.write().unwrap();
        map.insert(id, Arc::new(client));
    }

    /// Retrieve an `Arc` handle to the active `ClickHouseClient` for `connection_id`.
    pub fn get(&self, connection_id: u64) -> Option<Arc<ClickHouseClient>> {
        let map = self.inner.read().unwrap();
        map.get(&connection_id).cloned()
    }

    /// Remove a client session from the pool (`db.disconnect`).
    pub fn remove(&self, connection_id: u64) -> bool {
        let mut map = self.inner.write().unwrap();
        map.remove(&connection_id).is_some()
    }

    /// Clear all active sessions (`system.shutdown`).
    pub fn clear_all(&self) {
        let mut map = self.inner.write().unwrap();
        map.clear();
    }

    /// Returns the number of currently active database connections.
    pub fn count(&self) -> usize {
        let map = self.inner.read().unwrap();
        map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::client::ConnectParams;

    #[test]
    fn test_pool_lifecycle() {
        let pool = ConnectionPool::default();
        let client = ClickHouseClient::from_params(ConnectParams {
            connection_id: 50,
            connection_string: Some("mock://localhost:8123".to_string()),
            ..Default::default()
        })
        .unwrap();

        pool.insert(client);
        assert_eq!(pool.count(), 1);

        let retrieved = pool.get(50).expect("Client 50 should exist");
        assert_eq!(retrieved.connection_id, 50);

        assert!(pool.remove(50));
        assert_eq!(pool.count(), 0);
        assert!(pool.get(50).is_none());
    }
}
