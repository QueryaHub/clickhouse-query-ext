use std::io::{self, Write};

/// Thread-safe / mutex-guarded NDJSON response writer to standard output.
pub fn write_ndjson_stdout(payload: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(payload.as_bytes())?;
    handle.write_all(b"\n")?;
    handle.flush()?;
    Ok(())
}
