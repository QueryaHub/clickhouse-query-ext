use tokio::io::{self, AsyncBufReadExt, BufReader, AsyncRead};
use tokio::sync::mpsc;

/// Asynchronous stdin line reader for NDJSON requests.
pub async fn start_stdin_reader(tx: mpsc::Sender<String>) -> io::Result<()> {
    start_reader(io::stdin(), tx).await
}

/// Generic asynchronous line reader for any `AsyncRead` source (useful for testing).
pub async fn start_reader<R: AsyncRead + Unpin>(source: R, tx: mpsc::Sender<String>) -> io::Result<()> {
    let mut reader = BufReader::new(source).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if tx.send(trimmed.to_string()).await.is_err() {
                // Receiver channel dropped, exit loop gracefully
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_start_reader_skips_empty_lines() {
        let input = b"{\"method\":\"ping\"}\n   \n\n{\"method\":\"handshake\"}\n";
        let (tx, mut rx) = mpsc::channel(10);

        tokio::spawn(async move {
            start_reader(&input[..], tx).await.unwrap();
        });

        let line1 = rx.recv().await.unwrap();
        assert_eq!(line1, "{\"method\":\"ping\"}");

        let line2 = rx.recv().await.unwrap();
        assert_eq!(line2, "{\"method\":\"handshake\"}");

        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn test_start_reader_channel_closed() {
        let input = b"{\"method\":\"1\"}\n{\"method\":\"2\"}\n{\"method\":\"3\"}\n";
        let (tx, mut rx) = mpsc::channel(1);

        let handle = tokio::spawn(async move {
            start_reader(&input[..], tx).await
        });

        // Read only first message then drop rx
        let line1 = rx.recv().await.unwrap();
        assert_eq!(line1, "{\"method\":\"1\"}");
        drop(rx);

        // Task should finish without error after receiver drop
        assert!(handle.await.unwrap().is_ok());
    }
}
