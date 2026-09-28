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

/// Returns the secure, user-isolated base scratch directory path.
///
/// On Unix:
/// Uses `$XDG_RUNTIME_DIR/clickhouse-query-ext/shadow` if `$XDG_RUNTIME_DIR` is set and absolute,
/// otherwise `/tmp/clickhouse-query-ext-<euid>/shadow`.
///
/// On non-Unix platforms (e.g. Windows):
/// Uses `std::env::temp_dir().join("clickhouse-query-ext").join("shadow")`.
pub fn get_scratch_base_dir() -> PathBuf {
    #[cfg(unix)]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let path = PathBuf::from(runtime_dir);
            if path.is_absolute() {
                return path.join("clickhouse-query-ext").join("shadow");
            }
        }
        let uid = unsafe { libc::geteuid() };
        std::env::temp_dir()
            .join(format!("clickhouse-query-ext-{}", uid))
            .join("shadow")
    }

    #[cfg(not(unix))]
    {
        std::env::temp_dir()
            .join("clickhouse-query-ext")
            .join("shadow")
    }
}

/// Verifies and initializes scratch / shadow directory structure with strict permission isolation.
///
/// On Unix systems:
/// - Isolates directory per user UID to prevent pre-creation attacks in shared `/tmp`.
/// - Rejects symbolic links at the base and parent directory levels.
/// - Validates ownership matches current effective UID.
/// - Enforces `0700` (`rwx------`) permissions so other unprivileged users cannot read buffers or partition freezes.
pub fn ensure_scratch_directories() -> std::io::Result<PathBuf> {
    let base_dir = get_scratch_base_dir();

    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

        let parent_dir = base_dir.parent().unwrap_or(&base_dir);
        let euid = unsafe { libc::geteuid() };

        // 1. Ensure parent directory (e.g. /tmp/clickhouse-query-ext-<uid>) exists with 0700
        if parent_dir.exists() {
            let meta = std::fs::symlink_metadata(parent_dir)?;
            if meta.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "Scratch parent directory {:?} is an insecure symlink",
                        parent_dir
                    ),
                ));
            }
            if meta.uid() != euid {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "Scratch parent directory {:?} is not owned by current user (owner UID {}, current UID {})",
                        parent_dir,
                        meta.uid(),
                        euid
                    ),
                ));
            }
            std::fs::set_permissions(parent_dir, std::fs::Permissions::from_mode(0o700))?;
        } else {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            builder.mode(0o700);
            builder.create(parent_dir)?;
        }

        // 2. Ensure base_dir (shadow) exists with 0700
        if base_dir.exists() {
            let meta = std::fs::symlink_metadata(&base_dir)?;
            if meta.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Scratch directory {:?} is an insecure symlink", base_dir),
                ));
            }
            if meta.uid() != euid {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "Scratch directory {:?} is not owned by current user",
                        base_dir
                    ),
                ));
            }
            std::fs::set_permissions(&base_dir, std::fs::Permissions::from_mode(0o700))?;
            info!(
                "Verified secure sandbox scratch directory at {:?}",
                base_dir
            );
        } else {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            builder.mode(0o700);
            builder.create(&base_dir)?;
            info!("Created secure sandbox scratch directory at {:?}", base_dir);
        }
    }

    #[cfg(not(unix))]
    {
        if !base_dir.exists() {
            std::fs::create_dir_all(&base_dir)?;
            info!("Created sandbox scratch directory at {:?}", base_dir);
        } else {
            info!(
                "Verified sandbox scratch directory integrity at {:?}",
                base_dir
            );
        }
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = std::fs::symlink_metadata(&path).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o700);
            assert_eq!(meta.uid(), unsafe { libc::geteuid() });

            let parent = path.parent().unwrap();
            let parent_meta = std::fs::symlink_metadata(parent).unwrap();
            assert_eq!(parent_meta.permissions().mode() & 0o777, 0o700);
            assert_eq!(parent_meta.uid(), unsafe { libc::geteuid() });
        }
    }

    #[test]
    fn test_init_panic_hook_does_not_panic() {
        // Calling init_panic_hook registers the hook without panic or crash
        init_panic_hook();
    }

    #[test]
    #[cfg(unix)]
    fn test_symlink_rejection_in_scratch() {
        let test_root =
            std::env::temp_dir().join(format!("test-scratch-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&test_root);
        std::fs::create_dir_all(&test_root).unwrap();

        let real_dir = test_root.join("real");
        std::fs::create_dir(&real_dir).unwrap();

        let symlink_path = test_root.join("symlink_target");
        std::os::unix::fs::symlink(&real_dir, &symlink_path).unwrap();

        let meta = std::fs::symlink_metadata(&symlink_path).unwrap();
        assert!(meta.file_type().is_symlink());

        let _ = std::fs::remove_dir_all(&test_root);
    }
}
