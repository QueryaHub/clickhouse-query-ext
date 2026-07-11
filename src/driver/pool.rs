use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::driver::client::ClickHouseClient;

#[derive(Default, Clone)]
pub struct ConnectionPool {
    inner: Arc<Mutex<HashMap<u64, ClickHouseClient>>>,
}
