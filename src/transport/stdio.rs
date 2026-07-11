use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// Asynchronous stdin line reader for NDJSON requests.
pub async fn start_stdin_reader(tx: mpsc::Sender<String>) -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if tx.send(trimmed.to_string()).await.is_err() {
                break;
            }
        }
    }
    Ok(())
}
