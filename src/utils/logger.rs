use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize sanitized logging directly to `io::stderr`.
pub fn init_stderr_logger() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,clickhouse_query_ext=debug"));

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .without_time()
        )
        .with(filter)
        .init();
}
