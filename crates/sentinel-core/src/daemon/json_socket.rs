//! Unix JSON socket listener. Reads newline-delimited `SpanEvent` arrays.
//!
//! Each NDJSON line is parsed through [`crate::ingest::json::JsonIngest`]
//! and the resulting `Vec<SpanEvent>` is forwarded to the daemon event loop.

#![cfg(unix)]

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::event::SpanEvent;

/// Run the JSON socket listener on Unix platforms.
///
/// Reads newline-delimited JSON (NDJSON): each line is a JSON array of `SpanEvent`s.
pub(super) async fn run_json_socket(
    path: &str,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    max_payload_size: usize,
) {
    use tokio::net::UnixListener;

    // Symlink-TOCTOU defense: refuse to unlink anything at `path` that
    // is a symlink. A local attacker who controls the parent directory
    // could otherwise point `path` at `/etc/passwd` (or any other file
    // the daemon user owns) and the `remove_file` on the next line
    // would follow the symlink and delete the target. `symlink_metadata`
    // does NOT follow symlinks, so we can detect and refuse safely.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::error!(
                "Refusing to bind Unix socket at {path}: path is a \
                 symlink, remove it manually after verifying the \
                 target is safe"
            );
            return;
        }
        _ => {}
    }

    // Clean up stale socket file (now verified to be a regular file or
    // absent).
    let _ = std::fs::remove_file(path);

    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind Unix socket {path}: {e}");
            return;
        }
    };

    // Restrict socket permissions to owner-only (prevent other local users from injecting events)
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::error!(
                "Failed to set socket permissions on {path}: {e}, refusing to listen on insecure socket"
            );
            let _ = std::fs::remove_file(path);
            return;
        }
    }

    tracing::info!("JSON socket listening on {path}");

    // Limit concurrent connections to prevent local DoS via connection flooding
    let semaphore = Arc::new(tokio::sync::Semaphore::new(128));

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let tx = tx.clone();
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    break; // semaphore closed
                };
                tokio::spawn(async move {
                    handle_json_connection(stream, tx, max_payload_size).await;
                    drop(permit);
                });
            }
            Err(e) => {
                tracing::error!("Unix socket accept error: {e}");
            }
        }
    }
}

/// Process a single JSON socket connection: read NDJSON lines and forward events.
async fn handle_json_connection(
    stream: tokio::net::UnixStream,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    max_payload_size: usize,
) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    const CONNECTION_LIMIT_FACTOR: u64 = 16;
    let limited = stream.take(max_payload_size as u64 * CONNECTION_LIMIT_FACTOR);
    let reader = tokio::io::BufReader::new(limited);
    let mut lines = reader.lines();
    let ingest = crate::ingest::json::JsonIngest::new(max_payload_size);
    while let Ok(Some(line)) = lines.next_line().await {
        if line.len() > max_payload_size {
            tracing::warn!("JSON socket: line exceeds max payload size, skipping");
            continue;
        }
        match crate::ingest::IngestSource::ingest(&ingest, line.as_bytes()) {
            Ok(events) if !events.is_empty() => {
                if tx.send(events).await.is_err() {
                    tracing::warn!("JSON socket: event channel closed");
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("JSON socket: failed to parse line: {e}");
            }
        }
    }
}

/// Build a unique Unix-socket path inside a fresh `tempfile::TempDir`
/// rooted at `/tmp/`, not `std::env::temp_dir()`.
///
/// Why `/tmp/` instead of `tempfile::tempdir()` (no arg): on macOS
/// `std::env::temp_dir()` resolves to `/var/folders/<hash>/T/...`,
/// which easily exceeds the Unix-socket `SUN_LEN` limit (104 bytes
/// on macOS, 108 on Linux). A `tempfile::TempDir` rooted at `/tmp`
/// gives us:
///
/// - **Collision-free by construction** (random 6-char suffix from
///   `tempfile`, not a timestamp-based pseudo-unique name).
/// - **Symlink-TOCTOU safe**: the directory is created with
///   `mkdir(..., 0o700)` atomically, so a local attacker cannot
///   substitute a symlink between path generation and socket bind.
/// - **Auto-cleanup on drop**: the `TempDir` owner (the test body)
///   removes the directory when it goes out of scope, including the
///   socket file and the parent dir.
///
/// The returned `TempDir` must be kept alive for the duration of the
/// test; the returned path borrows from it.
#[cfg(test)]
pub(super) fn unique_socket_dir_and_path(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("psd-{name}-"))
        .tempdir_in("/tmp")
        .expect("mkdtemp in /tmp should succeed");
    let path = dir.path().join("daemon.sock");
    (dir, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn handle_json_connection_happy_path_forwards_events() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (client, server) = UnixStream::pair().expect("UnixStream::pair should succeed");
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);

        // Spawn the connection handler (reads from `server`).
        let handle = tokio::spawn(async move {
            handle_json_connection(server, tx, 1024 * 1024).await;
        });

        // Write one NDJSON line with a minimal valid SpanEvent array,
        // then close the client half so the server sees EOF and returns.
        let line = r#"[{"timestamp":"2025-07-10T14:32:01.123Z","trace_id":"t1","span_id":"s1","service":"svc","type":"sql","operation":"SELECT","target":"SELECT 1","duration_us":100,"source":{"endpoint":"GET /test","method":"m"}}]"#;
        let mut client = client;
        client.write_all(line.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        // The handler should send the decoded events through the channel.
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive events within 2s")
            .expect("channel still open");
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].trace_id, "t1");

        handle.await.unwrap();
    }

    #[tokio::test]
    async fn handle_json_connection_skips_oversize_line() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);

        // Small max_payload so the line is over the limit.
        let handle = tokio::spawn(async move {
            handle_json_connection(server, tx, 32).await;
        });

        let mut client = client;
        // This line is > 32 bytes, triggers the "line exceeds max payload size" branch.
        let oversize_line = r#"[{"timestamp":"2025-07-10T14:32:01.123Z","trace_id":"t1","span_id":"s1","service":"svc","type":"sql","operation":"SELECT","target":"x","duration_us":1,"source":{"endpoint":"/","method":"m"}}]"#;
        client.write_all(oversize_line.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        let recv = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            recv.is_err() || recv.unwrap().is_none(),
            "oversize line must be dropped, channel should not receive anything"
        );
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn handle_json_connection_skips_malformed_line() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);

        let handle = tokio::spawn(async move {
            handle_json_connection(server, tx, 1024 * 1024).await;
        });

        let mut client = client;
        // Malformed: hits the Err(e) branch in the match.
        client.write_all(b"not json at all\n").await.unwrap();
        client.shutdown().await.unwrap();

        let recv = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            recv.is_err() || recv.unwrap().is_none(),
            "malformed line must be dropped"
        );
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn run_json_socket_accepts_connection_and_forwards_events() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        // Keep `_dir` alive until the end of the test; drop removes the
        // socket + parent tempdir. `path` is a PathBuf owned by us.
        let (_dir, path) = unique_socket_dir_and_path("accept");
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        let path_for_server = path.to_string_lossy().into_owned();
        let server = tokio::spawn(async move {
            run_json_socket(&path_for_server, tx, 1024 * 1024).await;
        });

        // Give the listener a brief moment to bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect as a client, write one NDJSON line, close.
        let mut client = UnixStream::connect(&path).await.expect("connect to socket");
        let line = r#"[{"timestamp":"2025-07-10T14:32:01.123Z","trace_id":"t-sock","span_id":"s1","service":"svc","type":"sql","operation":"SELECT","target":"SELECT 1","duration_us":100,"source":{"endpoint":"GET /test","method":"m"}}]"#;
        client.write_all(line.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive events within 2s")
            .expect("channel still open");
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].trace_id, "t-sock");

        server.abort();
        let _ = server.await;
        // _dir drops here, removing the socket and parent tempdir.
    }

    #[tokio::test]
    async fn run_json_socket_fails_to_bind_on_invalid_path() {
        // Path inside a non-existent directory → bind returns Err, the
        // function emits a tracing::error and returns without panicking.
        let path = "/nonexistent-directory-for-test/perf-sentinel.sock".to_string();
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        // Should return near-immediately (bind fails).
        tokio::time::timeout(Duration::from_secs(2), run_json_socket(&path, tx, 1024))
            .await
            .expect("bind failure must return immediately, not hang");
    }

    #[tokio::test]
    async fn run_json_socket_refuses_to_clobber_symlink() {
        // Symlink-TOCTOU regression guard: create a symlink at `path`
        // pointing at a sentinel victim file, call run_json_socket, and
        // verify the victim is NOT deleted (i.e., the symlink-aware
        // pre-check fired and the function returned early).
        use std::os::unix::fs::symlink;

        let (dir, sock_path) = unique_socket_dir_and_path("symlink-guard");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "important").unwrap();
        // Replace the sock path with a symlink to the victim.
        symlink(&victim, &sock_path).expect("symlink creation");

        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        let sock_str = sock_path.to_string_lossy().into_owned();
        tokio::time::timeout(Duration::from_secs(2), run_json_socket(&sock_str, tx, 1024))
            .await
            .expect("symlink refusal must return immediately, not hang");

        // Victim must still exist and still contain its original data.
        let content = std::fs::read_to_string(&victim)
            .expect("victim file must still exist after symlink refusal");
        assert_eq!(content, "important");
    }
}
