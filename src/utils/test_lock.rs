use tokio::sync::Mutex;

/// Global async mutex to serialize unit tests touching global `ConnectionPool` and `ConnectionSecretsPool`.
pub static GLOBAL_TEST_LOCK: Mutex<()> = Mutex::const_new(());
