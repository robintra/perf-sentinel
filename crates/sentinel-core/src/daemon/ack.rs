//! Daemon-side ack store: JSONL append-only persistence + in-memory cache.
//!
//! Complements the CI-side TOML acknowledgments (`crate::acknowledgments`)
//! with a runtime API for SRE-on-call use cases. The two sources are
//! unioned at query time, with TOML winning on conflict (immutable
//! baseline shipped via PR review).
//!
//! ## File format
//!
//! Append-only JSONL at `~/.local/share/perf-sentinel/acks.jsonl` by
//! default. Each line is one event:
//!
//! ```jsonl
//! {"action":"ack","signature":"<sig>","by":"alice","reason":"...","at":"2026-05-04T13:30:00Z","expires_at":null}
//! {"action":"unack","signature":"<sig>","by":"alice","at":"2026-05-04T14:00:00Z"}
//! ```
//!
//! ## Compaction
//!
//! At startup, the daemon replays the JSONL into a `HashMap<Signature, AckEntry>`
//! (apply on `Ack`, remove on `Unack`, drop on expiry), then atomically
//! rewrites the file via tmp + rename with only the active entries. A
//! runaway ack/unack loop therefore cannot accumulate forever, the file
//! resets every restart.
//!
//! ## Concurrency
//!
//! The in-memory map sits behind an `RwLock` (cheap read snapshots).
//! Disk writes go through a `Mutex<File>` so concurrent `ack`/`unack`
//! calls produce one well-formed JSONL line each. The mutex is held for
//! the entire write+map-update so a failed disk write never leaves the
//! map ahead of the persisted state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock};

/// Hard cap on the JSONL file size. A daemon that survives this much
/// churn without a restart should restart anyway, the startup
/// compaction reclaims the space.
pub const MAX_ACKS_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Hard cap on a single JSONL line. Bounds the worst-case parse cost
/// per entry and rejects pathological inputs.
pub const MAX_ACK_ENTRY_BYTES: usize = 4 * 1024;

/// Max number of simultaneously active acks held in memory. Bounds RSS
/// growth in face of an attacker who can call `POST /ack` repeatedly
/// with new signatures.
pub const MAX_ACTIVE_ACKS: usize = 10_000;

/// Soft cap on signature byte length accepted by `ack` / `unack`.
/// Output of `compute_signature` is at most ~150 bytes in practice
/// (long service name + endpoint), 256 leaves comfortable margin while
/// rejecting obvious garbage.
const MAX_SIGNATURE_LEN: usize = 256;

/// Single ack/unack event written to the JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckEntry {
    pub action: AckAction,
    pub signature: String,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AckAction {
    Ack,
    Unack,
}

#[derive(Debug, thiserror::Error)]
pub enum AckError {
    #[error("ack file IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ack file parse error at line {line}: {source}")]
    Parse {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("ack file exceeds max size of {} bytes", MAX_ACKS_FILE_BYTES)]
    FileTooLarge,
    #[error("ack entry exceeds max size of {} bytes", MAX_ACK_ENTRY_BYTES)]
    EntryTooLarge,
    #[error("active ack limit reached ({})", MAX_ACTIVE_ACKS)]
    LimitReached,
    #[error("signature already acked")]
    AlreadyAcked,
    #[error("signature not currently acked")]
    NotAcked,
    #[error("invalid signature format")]
    InvalidSignature,
    #[error("no default storage location available, set [daemon.ack] storage_path explicitly")]
    NoStorageLocation,
}

/// In-memory + persisted ack state.
pub struct AckStore {
    storage_path: PathBuf,
    active: RwLock<HashMap<String, AckEntry>>,
    file: Mutex<File>,
}

impl std::fmt::Debug for AckStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AckStore")
            .field("storage_path", &self.storage_path)
            .finish_non_exhaustive()
    }
}

impl AckStore {
    /// Open or create the storage file, replay existing entries, compact in place.
    ///
    /// On first run, creates parent directories and the file with mode
    /// 0600 on Unix. On subsequent runs, reads the JSONL into a map
    /// (`Ack` inserts, `Unack` removes, expired entries are dropped),
    /// then atomically rewrites the file with only the surviving
    /// entries.
    ///
    /// # Errors
    ///
    /// Returns [`AckError::Io`] for filesystem errors,
    /// [`AckError::Parse`] when an existing JSONL line is malformed,
    /// [`AckError::FileTooLarge`] / [`AckError::EntryTooLarge`] when
    /// caps are exceeded.
    pub async fn new(storage_path: PathBuf) -> Result<Arc<Self>, AckError> {
        if let Some(parent) = storage_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let active = if storage_path.exists() {
            replay_and_compact(&storage_path, Utc::now()).await?
        } else {
            HashMap::new()
        };

        let file = open_append(&storage_path).await?;

        Ok(Arc::new(Self {
            storage_path,
            active: RwLock::new(active),
            file: Mutex::new(file),
        }))
    }

    /// Append an `Ack` event and update the in-memory map.
    ///
    /// # Errors
    ///
    /// Returns [`AckError::AlreadyAcked`] if the signature is already
    /// active, [`AckError::LimitReached`] at the active-acks cap,
    /// [`AckError::InvalidSignature`] for malformed input,
    /// [`AckError::FileTooLarge`] / [`AckError::EntryTooLarge`] for
    /// size caps, [`AckError::Io`] for filesystem errors.
    pub async fn ack(&self, mut entry: AckEntry) -> Result<(), AckError> {
        entry.action = AckAction::Ack;
        validate_signature(&entry.signature)?;
        sanitize_entry(&mut entry);

        let mut file = self.file.lock().await;
        {
            let active = self.active.read().await;
            if active.contains_key(&entry.signature) {
                return Err(AckError::AlreadyAcked);
            }
            if active.len() >= MAX_ACTIVE_ACKS {
                return Err(AckError::LimitReached);
            }
        }
        append_line(&mut file, &entry).await?;
        let mut active = self.active.write().await;
        active.insert(entry.signature.clone(), entry);
        Ok(())
    }

    /// Append an `Unack` event and remove from the in-memory map.
    ///
    /// # Errors
    ///
    /// Returns [`AckError::NotAcked`] when the signature is not active,
    /// [`AckError::InvalidSignature`] for malformed input,
    /// [`AckError::Io`] for filesystem errors.
    pub async fn unack(&self, signature: &str, by: &str) -> Result<(), AckError> {
        validate_signature(signature)?;
        let mut file = self.file.lock().await;
        {
            let active = self.active.read().await;
            if !active.contains_key(signature) {
                return Err(AckError::NotAcked);
            }
        }
        let entry = AckEntry {
            action: AckAction::Unack,
            signature: signature.to_string(),
            by: strip_bidi(by),
            reason: None,
            at: Utc::now(),
            expires_at: None,
        };
        append_line(&mut file, &entry).await?;
        let mut active = self.active.write().await;
        active.remove(signature);
        Ok(())
    }

    /// Snapshot the active ack map for query-time filtering.
    ///
    /// Cheaper than `list_active` when the caller only needs key
    /// lookups: this returns the underlying map shape, the caller can
    /// `.get(signature)` directly.
    pub async fn snapshot_active(&self) -> HashMap<String, AckEntry> {
        let now = Utc::now();
        let active = self.active.read().await;
        active
            .iter()
            .filter(|(_, e)| !is_expired(e, now))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// List all active acks. Used by `GET /api/acks`.
    pub async fn list_active(&self) -> Vec<AckEntry> {
        let now = Utc::now();
        let active = self.active.read().await;
        active
            .values()
            .filter(|e| !is_expired(e, now))
            .cloned()
            .collect()
    }

    /// Path to the JSONL file. Exposed for diagnostics / log lines.
    #[must_use]
    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }
}

/// Default storage path: `<data_local_dir>/perf-sentinel/acks.jsonl`.
///
/// We deliberately do not fall back to `/tmp` because ack data is audit
/// material that must survive a reboot.
///
/// # Errors
///
/// Returns [`AckError::NoStorageLocation`] when `dirs::data_local_dir()`
/// cannot resolve a path (rare, e.g. minimal containers without HOME).
/// The operator must then set `[daemon.ack] storage_path` explicitly.
pub fn default_storage_path() -> Result<PathBuf, AckError> {
    let base = dirs::data_local_dir().ok_or(AckError::NoStorageLocation)?;
    Ok(base.join("perf-sentinel").join("acks.jsonl"))
}

fn validate_signature(sig: &str) -> Result<(), AckError> {
    if sig.is_empty() || sig.len() > MAX_SIGNATURE_LEN {
        return Err(AckError::InvalidSignature);
    }
    // Tail must be `:` followed by exactly 16 lowercase hex chars.
    // Service names can contain `:` legitimately so we cannot split on
    // it, but the SHA-256 prefix tail is fixed-format.
    let bytes = sig.as_bytes();
    if bytes.len() < 17 || bytes[bytes.len() - 17] != b':' {
        return Err(AckError::InvalidSignature);
    }
    let tail = &sig[sig.len() - 16..];
    if !tail
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(AckError::InvalidSignature);
    }
    Ok(())
}

fn sanitize_entry(entry: &mut AckEntry) {
    entry.by = strip_bidi(&entry.by);
    if let Some(ref reason) = entry.reason {
        entry.reason = Some(strip_bidi(reason));
    }
}

fn strip_bidi(s: &str) -> String {
    crate::report::sarif::strip_bidi_and_invisible(s)
}

fn is_expired(entry: &AckEntry, now: DateTime<Utc>) -> bool {
    entry.expires_at.is_some_and(|exp| exp < now)
}

async fn open_append(path: &Path) -> Result<File, AckError> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true).read(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let file = opts.open(path).await?;
    Ok(file)
}

async fn append_line(file: &mut File, entry: &AckEntry) -> Result<(), AckError> {
    let mut line =
        serde_json::to_string(entry).map_err(|e| AckError::Parse { line: 0, source: e })?;
    if line.len() + 1 > MAX_ACK_ENTRY_BYTES {
        return Err(AckError::EntryTooLarge);
    }
    line.push('\n');
    let metadata = file.metadata().await?;
    if metadata.len().saturating_add(line.len() as u64) > MAX_ACKS_FILE_BYTES {
        return Err(AckError::FileTooLarge);
    }
    file.write_all(line.as_bytes()).await?;
    file.flush().await?;
    file.sync_data().await?;
    Ok(())
}

async fn replay_and_compact(
    path: &Path,
    now: DateTime<Utc>,
) -> Result<HashMap<String, AckEntry>, AckError> {
    let metadata = tokio::fs::metadata(path).await?;
    if metadata.len() > MAX_ACKS_FILE_BYTES {
        return Err(AckError::FileTooLarge);
    }

    let mut file = OpenOptions::new().read(true).open(path).await?;
    file.seek(std::io::SeekFrom::Start(0)).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut active: HashMap<String, AckEntry> = HashMap::new();
    let mut line_no = 0usize;
    while let Some(line) = lines.next_line().await? {
        line_no += 1;
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_ACK_ENTRY_BYTES {
            return Err(AckError::EntryTooLarge);
        }
        let entry: AckEntry = serde_json::from_str(&line).map_err(|e| AckError::Parse {
            line: line_no,
            source: e,
        })?;
        match entry.action {
            AckAction::Ack => {
                if !is_expired(&entry, now) && active.len() < MAX_ACTIVE_ACKS {
                    active.insert(entry.signature.clone(), entry);
                }
            }
            AckAction::Unack => {
                active.remove(&entry.signature);
            }
        }
    }

    rewrite_compacted(path, &active).await?;
    Ok(active)
}

async fn rewrite_compacted(
    path: &Path,
    active: &HashMap<String, AckEntry>,
) -> Result<(), AckError> {
    let tmp = path.with_extension("jsonl.tmp");
    let mut tmp_file = open_append(&tmp).await?;
    tmp_file.set_len(0).await?;
    tmp_file.seek(std::io::SeekFrom::Start(0)).await?;
    for entry in active.values() {
        let mut line =
            serde_json::to_string(entry).map_err(|e| AckError::Parse { line: 0, source: e })?;
        line.push('\n');
        tmp_file.write_all(line.as_bytes()).await?;
    }
    tmp_file.flush().await?;
    tmp_file.sync_data().await?;
    drop(tmp_file);
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::TempDir;

    fn sample_entry(sig: &str, action: AckAction) -> AckEntry {
        AckEntry {
            action,
            signature: sig.to_string(),
            by: "alice@example.com".to_string(),
            reason: Some("test".to_string()),
            at: Utc::now(),
            expires_at: None,
        }
    }

    fn valid_sig(prefix: &str) -> String {
        format!("{prefix}:0123456789abcdef")
    }

    #[tokio::test]
    async fn new_creates_empty_file_when_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path.clone()).await.unwrap();
        assert!(path.exists());
        assert!(store.list_active().await.is_empty());
    }

    #[tokio::test]
    async fn ack_persists_entry_to_jsonl() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path.clone()).await.unwrap();
        let sig = valid_sig("n_plus_one_sql:svc:_orders");
        store.ack(sample_entry(&sig, AckAction::Ack)).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.lines().count(), 1);
        assert!(content.contains("\"action\":\"ack\""));
        assert!(content.contains(&sig));
    }

    #[tokio::test]
    async fn ack_rejects_already_acked() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path).await.unwrap();
        let sig = valid_sig("n_plus_one_sql:svc:_orders");
        store.ack(sample_entry(&sig, AckAction::Ack)).await.unwrap();
        let err = store
            .ack(sample_entry(&sig, AckAction::Ack))
            .await
            .unwrap_err();
        assert!(matches!(err, AckError::AlreadyAcked));
    }

    #[tokio::test]
    async fn unack_returns_not_acked_for_unknown_signature() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path).await.unwrap();
        let err = store
            .unack(&valid_sig("foo:bar:_baz"), "alice")
            .await
            .unwrap_err();
        assert!(matches!(err, AckError::NotAcked));
    }

    #[tokio::test]
    async fn unack_removes_active_ack_and_writes_unack_event() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path.clone()).await.unwrap();
        let sig = valid_sig("n_plus_one_sql:svc:_orders");
        store.ack(sample_entry(&sig, AckAction::Ack)).await.unwrap();
        store.unack(&sig, "alice").await.unwrap();
        assert!(store.list_active().await.is_empty());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.lines().count(), 2);
        assert!(content.contains("\"action\":\"unack\""));
    }

    #[tokio::test]
    async fn replay_compacts_ack_unack_pairs_to_zero() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        {
            let store = AckStore::new(path.clone()).await.unwrap();
            for i in 0..50 {
                let sig = valid_sig(&format!("foo:svc:_endpoint_{i}"));
                store.ack(sample_entry(&sig, AckAction::Ack)).await.unwrap();
                store.unack(&sig, "alice").await.unwrap();
            }
            let content = tokio::fs::read_to_string(&path).await.unwrap();
            assert_eq!(content.lines().count(), 100);
        }
        let store = AckStore::new(path.clone()).await.unwrap();
        assert!(store.list_active().await.is_empty());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.lines().count(), 0);
    }

    #[tokio::test]
    async fn replay_preserves_active_acks_across_restarts() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let sig = valid_sig("foo:svc:_endpoint");
        {
            let store = AckStore::new(path.clone()).await.unwrap();
            store.ack(sample_entry(&sig, AckAction::Ack)).await.unwrap();
        }
        let store = AckStore::new(path).await.unwrap();
        let active = store.list_active().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].signature, sig);
    }

    #[tokio::test]
    async fn expired_entries_are_dropped_at_compaction() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let sig_live = valid_sig("foo:svc:_alive");
        let sig_dead = valid_sig("foo:svc:_dead");
        {
            let store = AckStore::new(path.clone()).await.unwrap();
            let live = AckEntry {
                expires_at: Some(Utc::now() + Duration::days(7)),
                ..sample_entry(&sig_live, AckAction::Ack)
            };
            let dead = AckEntry {
                expires_at: Some(Utc::now() - Duration::days(1)),
                ..sample_entry(&sig_dead, AckAction::Ack)
            };
            store.ack(live).await.unwrap();
            store.ack(dead).await.unwrap();
        }
        let store = AckStore::new(path).await.unwrap();
        let active = store.list_active().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].signature, sig_live);
    }

    #[tokio::test]
    async fn snapshot_active_filters_expired_at_query_time() {
        // Plant entries with explicit expiry timestamps relative to a
        // captured reference instant rather than relying on `tokio::time::sleep`,
        // which is noisy under parallel test load.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path).await.unwrap();
        let sig_live = valid_sig("foo:svc:_alive");
        let sig_dead = valid_sig("foo:svc:_dead");
        let live = AckEntry {
            expires_at: Some(Utc::now() + Duration::days(7)),
            ..sample_entry(&sig_live, AckAction::Ack)
        };
        let dead = AckEntry {
            expires_at: Some(Utc::now() - Duration::seconds(1)),
            ..sample_entry(&sig_dead, AckAction::Ack)
        };
        store.ack(live).await.unwrap();
        // Bypass the public `ack` for the already-expired entry, which
        // would never accept it if applied in production order.
        let mut active = store.active.write().await;
        active.insert(sig_dead.clone(), dead);
        drop(active);
        let snap = store.snapshot_active().await;
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key(&sig_live));
    }

    #[tokio::test]
    async fn ack_strips_bidi_from_by_and_reason() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path).await.unwrap();
        let sig = valid_sig("foo:svc:_endpoint");
        let trojan = "alice\u{202e}bob";
        let entry = AckEntry {
            by: trojan.to_string(),
            reason: Some(format!("hidden{}text", '\u{200b}')),
            ..sample_entry(&sig, AckAction::Ack)
        };
        store.ack(entry).await.unwrap();
        let active = store.list_active().await;
        assert_eq!(active[0].by, "alicebob");
        assert_eq!(active[0].reason.as_deref(), Some("hiddentext"));
    }

    #[tokio::test]
    async fn ack_rejects_invalid_signature() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path).await.unwrap();
        for bad in &[
            "",
            "no-tail",
            "foo:bar:zzzzzzzzzzzzzzzz",
            "foo:bar:0123456789ABCDEF",
        ] {
            let err = store
                .ack(sample_entry(bad, AckAction::Ack))
                .await
                .unwrap_err();
            assert!(matches!(err, AckError::InvalidSignature), "rejected {bad}");
        }
    }

    #[tokio::test]
    async fn parse_error_includes_line_number() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let bad = b"{\"action\":\"ack\",\"signature\":\"x\",\"by\":\"a\",\"at\":\"2026-05-04T00:00:00Z\"}\n{not json}\n";
        tokio::fs::write(&path, bad).await.unwrap();
        let err = AckStore::new(path).await.unwrap_err();
        match err {
            AckError::Parse { line, .. } => assert_eq!(line, 2),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_ack_writes_are_well_formed_jsonl() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path.clone()).await.unwrap();
        let mut handles = Vec::new();
        for i in 0..30 {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let sig = valid_sig(&format!("foo:svc:_endpoint_{i}"));
                store.ack(sample_entry(&sig, AckAction::Ack)).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content.lines().count(), 30);
        for line in content.lines() {
            let _: AckEntry = serde_json::from_str(line).unwrap();
        }
    }

    #[tokio::test]
    async fn list_active_returns_only_non_expired() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path).await.unwrap();
        let sig = valid_sig("foo:svc:_endpoint");
        let entry = AckEntry {
            expires_at: Some(Utc::now() - Duration::seconds(1)),
            ..sample_entry(&sig, AckAction::Ack)
        };
        // Bypass `ack` because it would not insert an already-expired entry.
        // Use the file path directly to plant the expired entry.
        let mut active = store.active.write().await;
        active.insert(sig.clone(), entry);
        drop(active);
        assert!(store.list_active().await.is_empty());
    }

    #[test]
    fn validate_signature_accepts_compute_signature_format() {
        // Mirror the format used by `acknowledgments::compute_signature`.
        let s = "n_plus_one_sql:order-svc:_api_v1_orders:0123456789abcdef";
        assert!(validate_signature(s).is_ok());
    }

    #[test]
    fn validate_signature_accepts_service_with_colon() {
        // Service names from OTLP can contain colons; only the tail is
        // fixed-format.
        let s = "n_plus_one_sql:svc:with:colons:_endpoint:0123456789abcdef";
        assert!(validate_signature(s).is_ok());
    }
}
