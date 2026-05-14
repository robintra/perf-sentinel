//! Per-window `Report` archive writer for the daemon: NDJSON output
//! with size rotation, count-based pruning, bounded mpsc channel with
//! drop-on-full policy. See `docs/design/08-PERIODIC-DISCLOSURE.md`.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender, error::TrySendError};
use tokio::task::JoinHandle;

use crate::config::DaemonArchiveConfig;
use crate::report::Report;

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

impl ArchiveHandle {
    /// Try to push a window to the writer without blocking. Returns
    /// `false` and logs a warning when the channel is full or closed.
    pub fn try_send(&self, archive: OwnedArchive) -> bool {
        match self.tx.try_send(archive) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::warn!("archive channel full, dropping window");
                false
            }
            Err(TrySendError::Closed(_)) => {
                tracing::warn!("archive writer task has exited, dropping window");
                false
            }
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
pub fn spawn(cfg: &DaemonArchiveConfig) -> Result<ArchiveHandle, ArchiveError> {
    let path = PathBuf::from(&cfg.path);
    refuse_symlink(&path)?;
    let file = open_append(&path)?;
    let bytes_written = metadata_len(&path);
    let cap_bytes = cfg.max_size_mb.saturating_mul(1_048_576);
    let max_files = cfg.max_files;
    let (tx, rx) = mpsc::channel::<OwnedArchive>(CHANNEL_CAPACITY);
    let join = tokio::spawn(async move {
        run_writer(rx, path, file, bytes_written, cap_bytes, max_files).await;
    });
    Ok(ArchiveHandle { tx, join })
}

fn refuse_symlink(path: &Path) -> Result<(), ArchiveError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(ArchiveError::SymlinkRefused {
            path: path.display().to_string(),
        }),
        Ok(_) | Err(_) => Ok(()),
    }
}

fn open_append(path: &Path) -> Result<BufWriter<File>, ArchiveError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| ArchiveError::Open {
            path: path.display().to_string(),
            source,
        })?;
    Ok(BufWriter::new(file))
}

fn metadata_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

async fn run_writer(
    mut rx: Receiver<OwnedArchive>,
    path: PathBuf,
    initial_file: BufWriter<File>,
    initial_bytes: u64,
    cap_bytes: u64,
    max_files: u32,
) {
    let mut file = initial_file;
    let mut bytes_written = initial_bytes;
    while let Some(archive) = rx.recv().await {
        let line = match serialize_envelope(&archive) {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!(error = %err, "archive serialization failed, dropping window");
                continue;
            }
        };
        if let Err(err) = write_line(&mut file, &line) {
            tracing::warn!(error = %err, "archive write failed, dropping line");
            continue;
        }
        bytes_written = bytes_written.saturating_add(line.len() as u64 + 1);
        if cap_bytes > 0 && bytes_written >= cap_bytes {
            match rotate(&path, &mut file, max_files) {
                Ok(()) => bytes_written = 0,
                Err(err) => {
                    tracing::warn!(error = %err, "archive rotation failed, continuing on current file");
                }
            }
        }
    }
    if let Err(err) = file.flush() {
        tracing::warn!(error = %err, "archive flush at shutdown failed");
    }
}

fn serialize_envelope(archive: &OwnedArchive) -> Result<String, serde_json::Error> {
    serde_json::to_string(&serde_json::json!({
        "ts": archive.ts,
        "report": &archive.report,
    }))
}

fn write_line(file: &mut BufWriter<File>, line: &str) -> std::io::Result<()> {
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

fn rotate(active: &Path, file: &mut BufWriter<File>, max_files: u32) -> std::io::Result<()> {
    file.flush()?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%fZ").to_string();
    let rotated_name = match active.file_stem().and_then(|s| s.to_str()) {
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
    *file = BufWriter::new(fresh);
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
    let active_name = active.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let active_stem = active.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if active_stem.is_empty() {
        return Ok(());
    }
    let prefix = format!("{active_stem}-");

    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
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
    use crate::report::interpret::InterpretationLevel;
    use crate::report::{Analysis, GreenSummary, QualityGate, Report};
    use tempfile::TempDir;

    fn cfg(dir: &TempDir, max_size_mb: u64, max_files: u32) -> DaemonArchiveConfig {
        DaemonArchiveConfig {
            path: dir.path().join("archive.ndjson").display().to_string(),
            max_size_mb,
            max_files,
        }
    }

    fn sample_report() -> Report {
        Report {
            analysis: Analysis {
                duration_ms: 0,
                events_processed: 0,
                traces_analyzed: 0,
            },
            findings: vec![],
            green_summary: GreenSummary {
                total_io_ops: 0,
                avoidable_io_ops: 0,
                io_waste_ratio: 0.0,
                io_waste_ratio_band: InterpretationLevel::Healthy,
                top_offenders: vec![],
                co2: None,
                regions: vec![],
                transport_gco2: None,
                scoring_config: None,
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
        }
    }

    fn sample_archive() -> OwnedArchive {
        OwnedArchive {
            ts: Utc::now(),
            report: sample_report(),
        }
    }

    #[tokio::test]
    async fn writer_appends_lines() {
        let dir = TempDir::new().unwrap();
        let handle = spawn(&cfg(&dir, 100, 12)).unwrap();
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
    async fn writer_rotates_at_size_cap_and_preserves_history() {
        let dir = TempDir::new().unwrap();
        let handle = spawn(&cfg(&dir, 1, 4)).unwrap();
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
            std::fs::File::create(&p).unwrap();
        }
        let decoy = dir.path().join("archive-evil.ndjson");
        std::fs::File::create(&decoy).unwrap();

        let handle = spawn(&cfg(&dir, 1, 2)).unwrap();
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

    #[tokio::test]
    async fn writer_refuses_symlink_target() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real.ndjson");
        std::fs::File::create(&real).unwrap();
        let link = dir.path().join("archive.ndjson");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = spawn(&DaemonArchiveConfig {
            path: link.display().to_string(),
            max_size_mb: 1,
            max_files: 4,
        })
        .unwrap_err();
        assert!(matches!(err, ArchiveError::SymlinkRefused { .. }));
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
