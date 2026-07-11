#![allow(dead_code)]
mod config;
mod driver;
mod error;
mod mapper;
mod rpc;
mod sdui;
mod transport;
mod utils;

use crate::rpc::models::{RpcRequest, RpcResponse};
use tokio::sync::mpsc;
use tracing::{error, info};

#[tokio::main]
async fn main() {
    // Initialize stderr logger first. Never log to stdout!
    utils::logger::init_stderr_logger();
    utils::recovery::init_panic_hook();
    if let Err(e) = utils::recovery::ensure_scratch_directories() {
        error!("Failed to initialize scratch directories: {}", e);
    }
    info!("Starting clickhouse-query-ext Rust process sandbox driver...");

    let (tx, mut rx) = mpsc::channel::<String>(128);

    // Spawn stdin reader task
    tokio::spawn(async move {
        if let Err(e) = transport::stdio::start_stdin_reader(tx).await {
            error!("stdin reader terminated with error: {}", e);
        }
    });

    // Process incoming NDJSON requests
    while let Some(line) = rx.recv().await {
        match serde_json::from_str::<RpcRequest>(&line) {
            Ok(req) => {
                let id = req.id.clone().unwrap_or(serde_json::Value::Null);
                let method = req.method.clone();
                let is_shutdown = method == "system.shutdown";

                // Fast-track system lifecycle methods (handshake, ping, injectCredentials, shutdown)
                // directly on the dispatcher loop for instant < 5ms response latency.
                if method == "system.ping"
                    || is_shutdown
                    || method == "system.handshake"
                    || method == "system.injectCredentials"
                {
                    match rpc::router::dispatch(&method, req.params).await {
                        Ok(result) => {
                            let resp = RpcResponse::success(id, result);
                            if let Ok(json_str) = serde_json::to_string(&resp) {
                                let _ = transport::framing::write_ndjson_stdout(&json_str);
                            }
                        }
                        Err(err) => {
                            let resp =
                                RpcResponse::error(id, err.to_rpc_code(), err.to_string(), None);
                            if let Ok(json_str) = serde_json::to_string(&resp) {
                                let _ = transport::framing::write_ndjson_stdout(&json_str);
                            }
                        }
                    }
                    if is_shutdown {
                        info!("Shutdown response emitted, terminating process with code 0.");
                        std::process::exit(0);
                    }
                } else {
                    // For heavy database queries (db.*) and SDUI operations, spawn onto Tokio worker threads
                    // so that `system.ping` heartbeat is never blocked by analytical workloads.
                    tokio::spawn(async move {
                        match rpc::router::dispatch(&method, req.params).await {
                            Ok(result) => {
                                let resp = RpcResponse::success(id, result);
                                if let Ok(json_str) = serde_json::to_string(&resp) {
                                    let _ = transport::framing::write_ndjson_stdout(&json_str);
                                }
                            }
                            Err(err) => {
                                let resp = RpcResponse::error(
                                    id,
                                    err.to_rpc_code(),
                                    err.to_string(),
                                    None,
                                );
                                if let Ok(json_str) = serde_json::to_string(&resp) {
                                    let _ = transport::framing::write_ndjson_stdout(&json_str);
                                }
                            }
                        }
                    });
                }
            }
            Err(e) => {
                error!("Failed to parse incoming JSON-RPC request: {}", e);
                let resp = RpcResponse::error(
                    serde_json::Value::Null,
                    -32700,
                    format!("Parse error: {}", e),
                    None,
                );
                if let Ok(json_str) = serde_json::to_string(&resp) {
                    let _ = transport::framing::write_ndjson_stdout(&json_str);
                }
            }
        }
    }

    info!("clickhouse-query-ext process exiting gracefully.");
}
