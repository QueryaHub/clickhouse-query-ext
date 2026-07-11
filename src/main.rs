mod config;
mod error;
mod transport;
mod rpc;
mod driver;
mod mapper;
mod sdui;
mod utils;

use tokio::sync::mpsc;
use tracing::{error, info};
use crate::rpc::models::{RpcRequest, RpcResponse};

#[tokio::main]
async fn main() {
    // Initialize stderr logger first. Never log to stdout!
    utils::logger::init_stderr_logger();
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
                match rpc::router::dispatch(&req.method, req.params).await {
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
