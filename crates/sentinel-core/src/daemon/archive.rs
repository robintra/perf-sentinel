//! Per-window `Report` archive writer for the daemon: NDJSON output
//! with size rotation, count-based pruning, bounded mpsc channel with
//! drop-on-full policy. See `docs/design/08-PERIODIC-DISCLOSURE.md`.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender, error::TrySendError};
use tokio::task::JoinHandle;

use crate::config::DaemonArchiveConfig;
use crate::report::Report;
use crate::report::metrics::{ArchiveDropReason, MetricsState};
use crate::report::periodic::hasher::{ARCHIVE_CHAIN_SEED, archive_chain_hash};
use std::sync::Arc;

const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ArchiveError {
    #[error("failed to open archive file {path}: {source}")]
    Open {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("archive path {path} is a symlink; refusing to follow")]
    SymlinkRefused { path: String },
}

/// Owned snapshot of one scoring window, serialised by the writer task.
pub struct OwnedArchive {
    pub ts: DateTime<Utc>,
    pub report: Report,
}

#[derive(Debug)]
pub struct ArchiveHandle {
    pub tx: Sender<OwnedArchive>,
    pub join: JoinHandle<()>,
}

/// Try to push a window to the writer without blocking. A full or
/// closed channel drops the window, counted on
/// `perf_sentinel_archive_windows_dropped_total` and logged. Free
/// function so the analysis worker can call it on a cloned `Sender`
/// without holding the `ArchiveHandle` (whose `join` stays with
/// `daemon::run`).
pub fn try_send(tx: &Sender<OwnedArchive>, archive: OwnedArchive, metrics: &MetricsState) {
    match tx.try_send(archive) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            metrics.record_archive_drop(ArchiveDropReason::ChannelFull);
            tracing::warn!(
                "archive channel full, dropping window (see \
                 perf_sentinel_archive_windows_dropped_total)"
            );
        }
        Err(TrySendError::Closed(_)) => {
            metrics.record_archive_drop(ArchiveDropReason::WriterExited);
            tracing::warn!(
                "archive writer task has exited, dropping window (see \
                 perf_sentinel_archive_windows_dropped_total)"
            );
        }
    }
}

/// Spawn the archive writer task and return its sender.
///
/// # Errors
///
/// [`ArchiveError::Open`] on open failure, [`ArchiveError::SymlinkRefused`]
/// when the configured path is a symlink (operator must point to a real
/// file the daemon owns).
pub fn spawn(
    cfg: &DaemonArchiveConfig,
    metrics: Arc<MetricsState>,
) -> Result<ArchiveHandle, ArchiveError> {
    let path = PathBuf::from(&cfg.path);
    refuse_symlink(&path)?;
    let mut file = open_append(&path)?;
    terminate_incomplete_line(&mut file).map_err(|source| ArchiveError::Open {
        path: path.display().to_string(),
        source,
    })?;
    let bytes_written = metadata_len(&path);
    let cap_bytes = cfg.max_size_mb.saturating_mul(1_048_576);
    let max_files = cfg.max_files;
    let (tx, rx) = mpsc::channel::<OwnedArchive>(CHANNEL_CAPACITY);
    let join = tokio::spawn(async move {
        run_writer(rx, path, file, bytes_written, cap_bytes, max_files, metrics).await;
    });
    Ok(ArchiveHandle { tx, join })
}

fn refuse_symlink(path: &Path) -> Result<(), ArchiveError> {
    if is_symlink(path) {
        return Err(ArchiveError::SymlinkRefused {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

/// Whether `path` is a symlink, without following it. Shared with the
/// incident appender, which refuses the same thing under its own error.
pub(super) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn open_append(path: &Path) -> Result<File, ArchiveError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|source| ArchiveError::Open {
            path: path.display().to_string(),
            source,
        })?;
    Ok(file)
}

/// Keep a crash-truncated record from being joined to the next window.
/// A complete JSON value that only missed its newline stays usable; a
/// partial value becomes one malformed line that disclosure can skip.
pub(super) fn terminate_incomplete_line(file: &mut File) -> std::io::Result<()> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    if last[0] != b'\n' {
        file.write_all(b"\n")?;
    }
    Ok(())
}

fn metadata_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

// Synchronous buffered I/O on a dedicated task, intentional: producers
// drop-on-full via try_send so a stalled filesystem never blocks the
// analysis path, and rotation runs once per cap_bytes (rare).
async fn run_writer(
    mut rx: Receiver<OwnedArchive>,
    path: PathBuf,
    initial_file: File,
    initial_bytes: u64,
    cap_bytes: u64,
    max_files: u32,
    metrics: Arc<MetricsState>,
) {
    let mut file = initial_file;
    let mut bytes_written = initial_bytes;
    let (mut prev, mut seq) = resume_chain(&path);
    while let Some(archive) = rx.recv().await {
        let line = match serialize_envelope(&archive, &prev, seq, metrics.archive_drops_total()) {
            Ok(line) => line,
            Err(err) => {
                metrics.record_archive_drop(ArchiveDropReason::SerializeError);
                tracing::warn!(
                    error = %err,
                    "archive serialization failed, dropping window (see \
                     perf_sentinel_archive_windows_dropped_total)"
                );
                continue;
            }
        };
        if let Err(err) = write_line(&mut file, &line) {
            // A failed write can still have landed part of the line, so the
            // file is cut back to the last complete window. Leaving the
            // fragment there would publish an I/O error as tampering.
            metrics.record_archive_drop(ArchiveDropReason::WriteError);
            tracing::warn!(
                error = %err,
                "archive write failed, dropping line (see \
                 perf_sentinel_archive_windows_dropped_total)"
            );
            if let Err(err) = file.set_len(bytes_written) {
                tracing::warn!(error = %err, "archive truncation after a failed write failed");
                // The fragment stays: seal it and resync the count, or a
                // later truncation would cut into a complete window. The
                // resync must read the fd, not the path: a rename under the
                // writer would map a path stat to 0 and a later set_len(0)
                // would destroy the archive. On a stat error, keep the old
                // count: it marks the last complete window.
                let _ = terminate_incomplete_line(&mut file);
                bytes_written = file.metadata().map_or(bytes_written, |m| m.len());
            }
            continue;
        }
        prev = extract_hash(&line).unwrap_or(prev);
        seq = seq.saturating_add(1);
        bytes_written = bytes_written.saturating_add(line.len() as u64 + 1);
        if cap_bytes > 0 && bytes_written >= cap_bytes {
            match rotate(&path, &mut file, max_files) {
                // A rotated file opens a fresh chain: the reader treats a
                // seed-rooted first line as a start, not as a break.
                Ok(()) => {
                    bytes_written = 0;
                    prev = ARCHIVE_CHAIN_SEED.to_string();
                    seq = 0;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "archive rotation failed, continuing on current file");
                }
            }
        }
    }
}

/// Serialise one window and chain it to `prev`.
///
/// `hash` covers `{ts, report, prev, seq}` in canonical form, so editing
/// any of them breaks it. `prev` ties the line to its predecessor, so
/// removing or reordering a line breaks the next one, and `seq` pins its
/// position so a break says how many lines are off, not only that one is.
/// A tail cut cleanly off the file stays invisible to both: what remains
/// is a shorter self-consistent chain, and only an anchor kept outside the
/// file can see it. It detects tampering after the fact, not an insincere
/// writer: whoever owns the daemon can rewrite the whole chain.
///
/// `drops` is the daemon-lifetime cumulative drop count at write time
/// (see `MetricsState::archive_drops_total`). Two consecutive lines
/// whose `drops` differ bracket the loss between them, which is how the
/// disclosure aggregator counts the windows a period lost without a
/// Prometheus scrape. Cumulative rather than per-line: a burst of drops
/// with no subsequent write still shows on the next line that does land.
fn serialize_envelope(
    archive: &OwnedArchive,
    prev: &str,
    seq: u64,
    drops: u64,
) -> Result<String, serde_json::Error> {
    let body = serde_json::json!({
        "ts": archive.ts,
        "report": &archive.report,
        "prev": prev,
        "seq": seq,
        "drops": drops,
    });
    let hash = archive_chain_hash(&body)?;
    let mut line = body;
    if let Some(obj) = line.as_object_mut() {
        obj.insert("hash".to_string(), serde_json::Value::String(hash));
    }
    serde_json::to_string(&line)
}

/// Chain head already on disk as `(prev, next_seq)`, so a restarted
/// daemon continues instead of opening a break at every restart. Falls
/// back to the seed on an empty, unreadable or pre-chain file.
fn resume_chain(path: &Path) -> (String, u64) {
    let seed = || (ARCHIVE_CHAIN_SEED.to_string(), 0);
    let Ok(file) = File::open(path) else {
        return seed();
    };
    // A linear scan avoids any record-size ceiling. A sidecar head can be
    // added if startup on multi-gigabyte unrotated archives becomes slow.
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(&line).ok()?;
            let hash = value.get("hash")?.as_str()?.to_string();
            let seq = value.get("seq").and_then(serde_json::Value::as_u64)?;
            Some((hash, seq.saturating_add(1)))
        })
        .last()
        .unwrap_or_else(seed)
}

fn extract_hash(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("hash")?
        .as_str()
        .map(ToString::to_string)
}

/// Write one line straight to the file.
///
/// Unbuffered on purpose: at window cadence buffering saves nothing, and
/// a `BufWriter` would leave a half-written line on an ungraceful death
/// (SIGKILL, OOM) or hold bytes the caller then cannot account for when a
/// write fails. Both cases end up published as a chain break.
fn write_line(file: &mut File, line: &str) -> std::io::Result<()> {
    let mut record = Vec::with_capacity(line.len() + 1);
    record.extend_from_slice(line.as_bytes());
    record.push(b'\n');
    file.write_all(&record)
}

fn rotate(active: &Path, file: &mut File, max_files: u32) -> std::io::Result<()> {
    file.flush()?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%fZ").to_string();
    let rotated_name = match active.file_stem().and_then(OsStr::to_str) {
        Some(stem) => format!("{stem}-{stamp}.ndjson"),
        None => format!("archive-{stamp}.ndjson"),
    };
    let rotated_path = active.parent().map_or_else(
        || PathBuf::from(&rotated_name),
        |dir| dir.join(&rotated_name),
    );
    std::fs::rename(active, &rotated_path)?;
    // create_new refuses to open if `active` already exists, which
    // closes the TOCTOU race where a co-resident attacker plants a
    // symlink between the rename and the re-open.
    let fresh = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(active)?;
    *file = fresh;
    prune(active, max_files)?;
    Ok(())
}

fn prune(active: &Path, max_files: u32) -> std::io::Result<()> {
    // A bare filename ("archive.ndjson") yields `parent() == Some("")`
    // which resolves to the current working directory, not "no parent".
    let dir_buf: PathBuf;
    let dir: &Path = match active.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => {
            dir_buf = PathBuf::from(".");
            dir_buf.as_path()
        }
    };
    let active_name = active.file_name().and_then(OsStr::to_str).unwrap_or("");
    let active_stem = active.file_stem().and_then(OsStr::to_str).unwrap_or("");
    if active_stem.is_empty() {
        return Ok(());
    }
    let prefix = format!("{active_stem}-");

    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        let Some(name) = p.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name == active_name {
            continue;
        }
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(stamp) = rest.strip_suffix(".ndjson") else {
            continue;
        };
        if !is_rotation_stamp(stamp) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        candidates.push((mtime, p));
    }
    candidates.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, path) in candidates.into_iter().skip(max_files as usize) {
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!(path = %path.display(), error = %err, "failed to prune rotated archive");
        }
    }
    Ok(())
}

fn is_rotation_stamp(s: &str) -> bool {
    // Format: YYYYMMDDTHHMMSS<frac>Z where frac is up to 9 digits
    // (nanoseconds via `%f`). Cap at 15 digits total to avoid matching
    // an unrelated stamp with an arbitrarily long suffix.
    let Some(without_z) = s.strip_suffix('Z') else {
        return false;
    };
    let mut parts = without_z.splitn(2, 'T');
    let Some(date) = parts.next() else {
        return false;
    };
    let Some(time) = parts.next() else {
        return false;
    };
    date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
        && (6..=15).contains(&time.len())
        && time.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::empty_report;
    // Only the Unix-gated `writer_refuses_symlink_target` test uses this.
    #[cfg(unix)]
    use core::assert_matches;
    use tempfile::TempDir;

    fn cfg(dir: &TempDir, max_size_mb: u64, max_files: u32) -> DaemonArchiveConfig {
        DaemonArchiveConfig {
            path: dir.path().join("archive.ndjson").display().to_string(),
            max_size_mb,
            max_files,
        }
    }

    fn test_metrics() -> Arc<MetricsState> {
        Arc::new(MetricsState::new())
    }

    fn dropped(metrics: &MetricsState, reason: ArchiveDropReason) -> u64 {
        metrics
            .archive_windows_dropped_total
            .with_label_values(&[reason.as_str()])
            .get()
    }

    fn sample_archive() -> OwnedArchive {
        OwnedArchive {
            ts: Utc::now(),
            report: empty_report(),
        }
    }

    #[tokio::test]
    async fn writer_appends_lines() {
        let dir = TempDir::new().unwrap();
        let handle = spawn(&cfg(&dir, 100, 12), test_metrics()).unwrap();
        handle.tx.send(sample_archive()).await.unwrap();
        handle.tx.send(sample_archive()).await.unwrap();
        drop(handle.tx);
        handle.join.await.unwrap();

        let contents = std::fs::read_to_string(dir.path().join("archive.ndjson")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("ts").is_some());
            assert!(v.get("report").is_some());
        }
    }

    #[tokio::test]
    async fn a_restarted_writer_resumes_the_chain_instead_of_breaking_it() {
        // The writer opens in append mode, so without reading the last hash
        // back every daemon restart would look like tampering to disclose.
        let dir = TempDir::new().unwrap();
        let first = spawn(&cfg(&dir, 100, 12), test_metrics()).unwrap();
        first.tx.send(sample_archive()).await.unwrap();
        drop(first.tx);
        first.join.await.unwrap();

        let second = spawn(&cfg(&dir, 100, 12), test_metrics()).unwrap();
        second.tx.send(sample_archive()).await.unwrap();
        drop(second.tx);
        second.join.await.unwrap();

        let contents = std::fs::read_to_string(dir.path().join("archive.ndjson")).unwrap();
        let lines: Vec<serde_json::Value> = contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0]["prev"].as_str().unwrap(),
            ARCHIVE_CHAIN_SEED,
            "the first line roots the chain"
        );
        assert_eq!(
            lines[1]["prev"].as_str().unwrap(),
            lines[0]["hash"].as_str().unwrap(),
            "the line written after the restart must reference the last one on disk"
        );
    }

    #[test]
    fn a_window_larger_than_64_mib_still_resumes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let line = serde_json::json!({
            "hash": "large-window-head",
            "seq": 7,
            "padding": "x".repeat(64 * 1024 * 1024),
        });
        std::fs::write(&path, serde_json::to_vec(&line).unwrap()).unwrap();
        assert_eq!(resume_chain(&path), ("large-window-head".to_string(), 8),);
    }

    #[tokio::test]
    async fn restart_separates_a_crash_truncated_line_from_the_next_window() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let first = serialize_envelope(&sample_archive(), ARCHIVE_CHAIN_SEED, 0, 0).unwrap();
        std::fs::write(&path, format!("{first}\n{{\"partial\"")).unwrap();

        let handle = spawn(&cfg(&dir, 100, 12), test_metrics()).unwrap();
        handle.tx.send(sample_archive()).await.unwrap();
        drop(handle.tx);
        handle.join.await.unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(serde_json::from_str::<serde_json::Value>(lines[1]).is_err());
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let resumed: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(resumed["prev"], first["hash"]);
        assert_eq!(resumed["seq"], 1);
    }

    /// Every line carries the daemon-lifetime cumulative drop count so
    /// the disclosure aggregator can derive per-period losses from the
    /// archive alone. A drop recorded before a write shows on that
    /// write's line.
    #[tokio::test]
    async fn lines_carry_the_cumulative_drop_count() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("archive.ndjson");
        let metrics = test_metrics();
        let handle = spawn(&cfg(&dir, 100, 12), Arc::clone(&metrics)).unwrap();
        handle.tx.send(sample_archive()).await.unwrap();
        // Wait for the first line to land before recording the drop, so
        // the two lines bracket it deterministically (the writer reads
        // the counter when it dequeues, not when the producer sends).
        let mut first_line_landed = false;
        for _ in 0..2_000 {
            if std::fs::read_to_string(&path).map_or(0, |c| c.lines().count()) == 1 {
                first_line_landed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert!(first_line_landed, "first archive line never landed");
        metrics.record_archive_drop(ArchiveDropReason::ChannelFull);
        handle.tx.send(sample_archive()).await.unwrap();
        drop(handle.tx);
        handle.join.await.unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        let lines: Vec<serde_json::Value> = contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["drops"], 0);
        assert_eq!(lines[1]["drops"], 1);
    }

    #[tokio::test]
    async fn writer_rotates_at_size_cap_and_preserves_history() {
        let dir = TempDir::new().unwrap();
        let handle = spawn(&cfg(&dir, 1, 4), test_metrics()).unwrap();
        for _ in 0..30 {
            // Each report serialises to a few hundred bytes; force rotation
            // by pushing enough envelopes to cross the 1 MB cap.
            let mut archive = sample_archive();
            archive.report.warnings = vec!["x".repeat(60_000)];
            handle.tx.send(archive).await.unwrap();
        }
        drop(handle.tx);
        handle.join.await.unwrap();

        let mut active_lines = 0usize;
        let mut rotated_lines = 0usize;
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let content = std::fs::read_to_string(entry.path()).unwrap();
            let lines = content.lines().count();
            if name == "archive.ndjson" {
                active_lines = lines;
            } else if name.starts_with("archive-") && name.ends_with(".ndjson") {
                assert!(lines > 0, "rotated archive {name} must not be empty");
                rotated_lines += lines;
            }
        }
        assert!(
            rotated_lines >= 1,
            "expected rotated archive to carry history"
        );
        assert!(active_lines + rotated_lines >= 30);
    }

    #[tokio::test]
    async fn writer_prunes_to_max_files_using_timestamp_filter() {
        let dir = TempDir::new().unwrap();
        // Five real rotation files plus one decoy that does not match the
        // timestamp suffix: prune must spare the decoy.
        for i in 0..5 {
            let p = dir
                .path()
                .join(format!("archive-2026010{i}T000000000000000Z.ndjson"));
            File::create(&p).unwrap();
        }
        let decoy = dir.path().join("archive-evil.ndjson");
        File::create(&decoy).unwrap();

        let handle = spawn(&cfg(&dir, 1, 2), test_metrics()).unwrap();
        for _ in 0..15 {
            let mut archive = sample_archive();
            archive.report.warnings = vec!["x".repeat(80_000)];
            handle.tx.send(archive).await.unwrap();
        }
        drop(handle.tx);
        handle.join.await.unwrap();

        assert!(decoy.exists(), "non-stamp file must be spared by prune");
        let rotated: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("archive-")
                    && name.ends_with(".ndjson")
                    && name != "archive-evil.ndjson"
                {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            rotated.len() <= 2,
            "pruning should keep at most 2 rotated files, got {rotated:?}"
        );
    }

    // Uses `std::os::unix::fs::symlink` to create the symlink under test, so
    // it only builds and runs on Unix. The symlink-refusal logic itself is
    // cross-platform (`symlink_metadata().is_symlink()`).
    #[cfg(unix)]
    #[tokio::test]
    async fn writer_refuses_symlink_target() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real.ndjson");
        File::create(&real).unwrap();
        let link = dir.path().join("archive.ndjson");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = spawn(
            &DaemonArchiveConfig {
                path: link.display().to_string(),
                max_size_mb: 1,
                max_files: 4,
            },
            test_metrics(),
        )
        .unwrap_err();
        assert_matches!(err, ArchiveError::SymlinkRefused { .. });
    }

    #[tokio::test]
    async fn try_send_counts_full_and_closed_drops() {
        let metrics = test_metrics();
        let (tx, rx) = mpsc::channel::<OwnedArchive>(1);
        try_send(&tx, sample_archive(), &metrics);
        assert_eq!(dropped(&metrics, ArchiveDropReason::ChannelFull), 0);
        try_send(&tx, sample_archive(), &metrics);
        assert_eq!(
            dropped(&metrics, ArchiveDropReason::ChannelFull),
            1,
            "a full channel must count the dropped window"
        );
        drop(rx);
        try_send(&tx, sample_archive(), &metrics);
        assert_eq!(
            dropped(&metrics, ArchiveDropReason::WriterExited),
            1,
            "a closed channel must count the dropped window"
        );
    }

    #[test]
    fn is_rotation_stamp_accepts_valid_format() {
        assert!(is_rotation_stamp("20260514T083000000123456Z"));
        assert!(is_rotation_stamp("20260101T000000Z"));
    }

    #[test]
    fn is_rotation_stamp_rejects_malformed() {
        assert!(!is_rotation_stamp("evil"));
        assert!(!is_rotation_stamp("20260514T083000"));
        assert!(!is_rotation_stamp("2026-05-14T08:30:00Z"));
        assert!(!is_rotation_stamp(""));
    }
}
