use crate::utils::secret_guard::ConnectionSecretsPool;
use std::path::PathBuf;
use tracing::{error, info};

/// Sets up the global panic hook.
/// Intercepts any unexpected Rust panic, logs a formatted error message to `stderr`
/// (`[clickhouse-query-ext PANIC] ...`), wipes sensitive credentials from memory (`ConnectionSecretsPool::global().clear_all()`),
/// and terminates with exit code `101` so `SandboxAutoRecovery` can handle exponential backoff restarts.
pub fn init_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = match panic_info.payload().downcast_ref::<&str>() {
            Some(s) => *s,
            None => match panic_info.payload().downcast_ref::<String>() {
                Some(s) => &s[..],
                None => "Box<Any>",
            },
        };

        let location = panic_info.location().map_or_else(
            || "unknown location".to_string(),
            |loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()),
        );

        let err_msg = format!("CRITICAL RUST PANIC at [{}]: {}", location, msg);
        // Direct eprintln to ensure output even if tracing is impaired during a panic
        eprintln!("[clickhouse-query-ext PANIC] {}", err_msg);
        error!("[clickhouse-query-ext PANIC] {}", err_msg);

        // Security requirement: clear all in-memory secrets before terminating due to panic
        ConnectionSecretsPool::global().clear_all();

        std::process::exit(101);
    }));
}

/// Verifies and initializes scratch / shadow directory structure (`/tmp/clickhouse-query-ext/shadow/` or system temp).
/// Called on startup by `main.rs` to ensure temporary buffers and partition freezes have a reliable workspace.
pub fn ensure_scratch_directories() -> std::io::Result<PathBuf> {
    let base_dir = std::env::temp_dir()
        .join("clickhouse-query-ext")
        .join("shadow");
    if !base_dir.exists() {
        std::fs::create_dir_all(&base_dir)?;
        info!("Created sandbox scratch directory at {:?}", base_dir);
    } else {
        info!(
            "Verified sandbox scratch directory integrity at {:?}",
            base_dir
        );
    }
    Ok(base_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_scratch_directories() {
        let path = ensure_scratch_directories().expect("Failed to create/ensure scratch directory");
        assert!(path.exists());
        assert!(path.is_dir());
        assert!(path.ends_with("shadow"));
    }

    #[test]
    fn test_init_panic_hook_does_not_panic() {
        // Calling init_panic_hook registers the hook without panic or crash
        init_panic_hook();
    }
}
