//! Daemon-side ack store: JSONL append-only persistence + in-memory cache.
//!
//! See `docs/design/06-INGESTION-AND-DAEMON.md` § "Daemon ack store:
//! JSONL + concurrency" for the file format, compaction strategy and
//! concurrency model rationale.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncWriteExt, BufReader};
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
/// Worst case in `compute_signature` is roughly
/// `<finding_type>` (~30 B) + `:` + `service.name` (255 B per `OTel`
/// spec) + `:` + sanitized endpoint (typically ~256 B but unbounded in
/// the source) + `:` + 32 hex. 1024 covers any realistic combination
/// while still rejecting obvious garbage like a multi-KB blob.
#[doc(hidden)]
pub const MAX_SIGNATURE_LEN: usize = 1024;

/// Soft cap on `AckEntry::by` byte length. Bounds JSONL line size for a
/// pathological caller and matches typical email / SSO identifier
/// lengths (~64-128 bytes).
const MAX_BY_LEN: usize = 256;

/// Soft cap on `AckEntry::reason` byte length. Mirrors the field cap on
/// span events: dozens to hundreds of bytes in practice, this leaves
/// headroom for ticket links and short justifications without letting
/// an attacker fill the audit log with multi-KB descriptions.
const MAX_REASON_LEN: usize = 1024;

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

impl AckAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ack => "ack",
            Self::Unack => "unack",
        }
    }
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
    #[error("ack entry serialization error: {0}")]
    Serialize(#[source] serde_json::Error),
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
    #[error("ack file at '{path}' is a symlink, refusing to follow")]
    SymlinkRefused { path: String },
    #[error("ack file '{path}' has insecure permissions ({mode:o}), refusing to open")]
    InsecurePermissions { path: String, mode: u32 },
    #[error("no default storage location available, set [daemon.ack] storage_path explicitly")]
    NoStorageLocation,
}

/// In-memory + persisted ack state.
///
/// The `file` mutex is the ack-state lock: every state mutation (`ack`,
/// `unack`, replay) holds it for the entire critical section, so the
/// file is the linearization point for the in-memory map. Readers take
/// only the `RwLock` and observe the `Arc<HashMap>` snapshot without
/// blocking writers. Writers swap a fresh `Arc` after each mutation so
/// outstanding read snapshots stay valid (cheap `Arc::clone` per read).
pub struct AckStore {
    storage_path: PathBuf,
    active: RwLock<Arc<HashMap<String, AckEntry>>>,
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
            // Tighten `<base>/perf-sentinel/` to 0700 on Unix; the leaf
            // is already 0600+O_NOFOLLOW so chmod failure is non-fatal.
            #[cfg(unix)]
            if !parent.as_os_str().is_empty() {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = tokio::fs::metadata(parent).await {
                    let mut perms = metadata.permissions();
                    let current = perms.mode() & 0o777;
                    if current & 0o077 != 0 {
                        perms.set_mode(0o700);
                        if let Err(e) = tokio::fs::set_permissions(parent, perms).await {
                            tracing::warn!(
                                path = %parent.display(),
                                error = %e,
                                "could not tighten ack store parent directory to 0700"
                            );
                        }
                    }
                }
            }
        }

        let active = if storage_path.exists() {
            replay_and_compact(&storage_path, Utc::now()).await?
        } else {
            HashMap::new()
        };

        let file = open_append(&storage_path).await?;

        Ok(Arc::new(Self {
            storage_path,
            active: RwLock::new(Arc::new(active)),
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

        // The file mutex linearizes all writers. Readers (`snapshot_active`,
        // `list_active`) take only the `RwLock` to clone the current `Arc`,
        // so the pattern below is race-free against other ack/unack
        // callers and never blocks readers.
        let mut file = self.file.lock().await;
        // Clone the current map outside the write lock. The file mutex
        // we hold guarantees no other writer can mutate `active` while
        // we work on `new_map`, so the slow O(N) HashMap clone runs
        // without blocking concurrent `snapshot_active` readers.
        let mut new_map = {
            let active = self.active.read().await;
            if active.contains_key(&entry.signature) {
                return Err(AckError::AlreadyAcked);
            }
            if active.len() >= MAX_ACTIVE_ACKS {
                return Err(AckError::LimitReached);
            }
            (**active).clone()
        };
        append_line(&mut file, &entry).await?;
        new_map.insert(entry.signature.clone(), entry);
        {
            let mut active = self.active.write().await;
            *active = Arc::new(new_map);
        }
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
        let mut new_map = {
            let active = self.active.read().await;
            if !active.contains_key(signature) {
                return Err(AckError::NotAcked);
            }
            (**active).clone()
        };
        let mut by = crate::report::sarif::strip_bidi_and_invisible(by).into_owned();
        crate::event::truncate_field(&mut by, MAX_BY_LEN);
        let entry = AckEntry {
            action: AckAction::Unack,
            signature: signature.to_string(),
            by,
            reason: None,
            at: Utc::now(),
            expires_at: None,
        };
        append_line(&mut file, &entry).await?;
        new_map.remove(signature);
        {
            let mut active = self.active.write().await;
            *active = Arc::new(new_map);
        }
        Ok(())
    }

    /// Cheap snapshot of the active ack map for query-time filtering.
    ///
    /// Returns an `Arc<HashMap>` clone (single atomic refcount inc, no
    /// data copy). Callers that need O(1) signature lookup hold the
    /// `Arc` for the lifetime of their filter pass, no lock contention
    /// with concurrent `ack`/`unack` writers. Expired entries are not
    /// filtered out at this stage, the caller applies its own
    /// expiration check via [`is_expired`] at query time.
    pub async fn snapshot_active(&self) -> Arc<HashMap<String, AckEntry>> {
        Arc::clone(&*self.active.read().await)
    }

    /// List all active acks. Used by `GET /api/acks`. Filters expired
    /// entries (they are removed from the persisted map at compaction
    /// time, but a daemon that has been running past an entry's
    /// `expires_at` would still surface them otherwise).
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
    // Tail must be `:` followed by exactly 32 lowercase hex chars,
    // with at least one byte before that colon (the kind/service/
    // endpoint prefix is non-empty in any real `compute_signature`
    // output). Service names can contain `:` legitimately so we cannot
    // split on it, but the SHA-256 prefix tail is fixed-format.
    let bytes = sig.as_bytes();
    if bytes.len() < 34 || bytes[bytes.len() - 33] != b':' {
        return Err(AckError::InvalidSignature);
    }
    let tail = &sig[sig.len() - 32..];
    if !tail
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(AckError::InvalidSignature);
    }
    Ok(())
}

fn sanitize_entry(entry: &mut AckEntry) {
    if let Cow::Owned(stripped) = crate::report::sarif::strip_bidi_and_invisible(&entry.by) {
        entry.by = stripped;
    }
    crate::event::truncate_field(&mut entry.by, MAX_BY_LEN);
    if let Some(reason) = entry.reason.as_mut() {
        if let Cow::Owned(stripped) = crate::report::sarif::strip_bidi_and_invisible(reason) {
            *reason = stripped;
        }
        crate::event::truncate_field(reason, MAX_REASON_LEN);
    }
}

pub(crate) fn is_expired(entry: &AckEntry, now: DateTime<Utc>) -> bool {
    entry.expires_at.is_some_and(|exp| exp < now)
}

async fn open_append(path: &Path) -> Result<File, AckError> {
    #[cfg(unix)]
    refuse_if_symlink(path).await?;
    let mut opts = OpenOptions::new();
    opts.create(true).append(true).read(true);
    #[cfg(unix)]
    opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let file = opts.open(path).await?;
    #[cfg(unix)]
    {
        // `mode(0o600)` only applies on creation. If a hostile local
        // user pre-created the file with weaker permissions, refuse
        // to append rather than leak audit data.
        use std::os::unix::fs::PermissionsExt;
        let metadata = file.metadata().await?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(AckError::InsecurePermissions {
                path: path.display().to_string(),
                mode,
            });
        }
    }
    Ok(file)
}

async fn append_line(file: &mut File, entry: &AckEntry) -> Result<(), AckError> {
    let mut line = serde_json::to_string(entry).map_err(AckError::Serialize)?;
    if line.len() + 1 > MAX_ACK_ENTRY_BYTES {
        return Err(AckError::EntryTooLarge);
    }
    line.push('\n');
    let metadata = file.metadata().await?;
    if metadata.len().saturating_add(line.len() as u64) > MAX_ACKS_FILE_BYTES {
        return Err(AckError::FileTooLarge);
    }
    // `fsync` per write is intentional: a daemon crash after a 201
    // Created response must not lose the ack on disk. Acks are
    // operator-driven and rare (dozens per day across a fleet), so
    // the per-write durability cost is negligible.
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
    let file_size = metadata.len();
    if file_size > MAX_ACKS_FILE_BYTES {
        return Err(AckError::FileTooLarge);
    }

    // Open via the same hardened helper so an attacker cannot symlink
    // the path to leak file content during replay.
    let file = open_for_replay(path).await?;
    let mut reader = BufReader::new(file);
    let mut active: HashMap<String, AckEntry> = HashMap::new();
    let mut line_no = 0usize;
    let mut dropped_for_limit = 0usize;
    let mut dropped_for_invalid_signature = 0usize;
    while let Some(line) = read_capped_line(&mut reader).await? {
        line_no += 1;
        if line.is_empty() {
            continue;
        }
        let mut entry: AckEntry = serde_json::from_str(&line).map_err(|e| AckError::Parse {
            line: line_no,
            source: e,
        })?;
        // Apply field caps to entries already on disk: a malformed
        // line under MAX_ACK_ENTRY_BYTES total may still carry an
        // oversized `by` or `reason` field. `sanitize_entry` enforces
        // the per-field caps and re-strips BiDi defensively.
        sanitize_entry(&mut entry);
        // Skip-with-warn on a signature that does not parse against the
        // current format. After the 0.5.28 16-hex to 32-hex bump, every
        // legacy entry trips this branch. Inserting it would surface as
        // an active ack in /api/acks that matches no finding, eating
        // slots against MAX_ACTIVE_ACKS and silently masking the reset.
        // Bypassing the check would also mask a typo in a new JSONL
        // line. We do NOT abort the daemon on this path, the operator
        // sees one warn per line plus an end-of-replay summary.
        if validate_signature(&entry.signature).is_err() {
            dropped_for_invalid_signature += 1;
            continue;
        }
        match entry.action {
            AckAction::Ack => {
                if is_expired(&entry, now) {
                    continue;
                }
                if active.len() >= MAX_ACTIVE_ACKS {
                    dropped_for_limit += 1;
                    continue;
                }
                active.insert(entry.signature.clone(), entry);
            }
            AckAction::Unack => {
                active.remove(&entry.signature);
            }
        }
    }
    if dropped_for_limit > 0 {
        tracing::warn!(
            dropped = dropped_for_limit,
            cap = MAX_ACTIVE_ACKS,
            "ack store at MAX_ACTIVE_ACKS during replay, additional Ack lines dropped"
        );
    }
    if dropped_for_invalid_signature > 0 {
        tracing::warn!(
            dropped = dropped_for_invalid_signature,
            "ack store dropped lines with invalid signature format on replay, \
             likely a leftover from a pre-0.5.28 install. Findings previously \
             acked under the 16-hex format will reappear, re-ack them under \
             the new 32-hex format."
        );
    }

    // Always rewrite, even when the file was empty: this is the
    // canonical "reset to mode 0600" point, dropping the rewrite to
    // save three syscalls on a no-op startup would let a pre-existing
    // weak-mode file slip past `open_append`'s post-open permission
    // check.
    rewrite_compacted(path, &active).await?;
    Ok(active)
}

#[cfg(unix)]
async fn open_for_replay(path: &Path) -> Result<File, AckError> {
    refuse_if_symlink(path).await?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await?;
    Ok(file)
}

#[cfg(not(unix))]
async fn open_for_replay(path: &Path) -> Result<File, AckError> {
    let file = OpenOptions::new().read(true).open(path).await?;
    Ok(file)
}

/// Refuse to follow a symlinked storage path. A hostile local user
/// could pre-create the leaf as a symlink to `~/.bashrc` or
/// `~/.ssh/authorized_keys`, and the daemon would happily append JSONL
/// lines to the target. `O_NOFOLLOW` covers the open path, this
/// pre-check covers the upstream `symlink_metadata` audit so we can
/// surface a typed error rather than the kernel's `ELOOP`. The leaf is
/// the only path component we control, deeper components like
/// `~/.local/share` are operator-trusted.
#[cfg(unix)]
async fn refuse_if_symlink(path: &Path) -> Result<(), AckError> {
    if let Ok(metadata) = tokio::fs::symlink_metadata(path).await
        && metadata.file_type().is_symlink()
    {
        return Err(AckError::SymlinkRefused {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

/// Read one JSONL line with a hard byte cap on the in-memory buffer.
///
/// `tokio::io::Lines::next_line()` would normally append to an internal
/// `String` until it sees a newline, with no allocation cap. With the
/// upstream `MAX_ACKS_FILE_BYTES = 64 MiB` file cap, a malformed file
/// with one giant un-terminated line could let the reader allocate
/// up to 64 MiB before any per-line check fired. This function reads
/// byte-by-byte and bails as soon as the buffer reaches
/// `MAX_ACK_ENTRY_BYTES`, bounding the worst-case allocation at
/// 4 KiB per call.
async fn read_capped_line<R>(reader: &mut BufReader<R>) -> Result<Option<String>, AckError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = reader.read(&mut byte).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        if buf.len() >= MAX_ACK_ENTRY_BYTES {
            return Err(AckError::EntryTooLarge);
        }
        buf.push(byte[0]);
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| AckError::Parse {
            line: 0,
            source: serde::de::Error::custom(format!("invalid UTF-8: {e}")),
        })
}

async fn rewrite_compacted(
    path: &Path,
    active: &HashMap<String, AckEntry>,
) -> Result<(), AckError> {
    let tmp = path.with_extension("jsonl.tmp");
    let mut opts = OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut tmp_file = opts.open(&tmp).await?;
    for entry in active.values() {
        let mut line = serde_json::to_string(entry).map_err(AckError::Serialize)?;
        line.push('\n');
        tmp_file.write_all(line.as_bytes()).await?;
    }
    tmp_file.flush().await?;
    tmp_file.sync_data().await?;
    drop(tmp_file);
    // Re-check just before swap: rename follows target symlinks and the
    // startup-time check does not cover the long compaction window.
    #[cfg(unix)]
    refuse_if_symlink(path).await?;
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
        format!("{prefix}:0123456789abcdef0123456789abcdef")
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
        let mut new_map = (**active).clone();
        new_map.insert(sig_dead.clone(), dead);
        *active = Arc::new(new_map);
        drop(active);
        let snap = store.snapshot_active().await;
        // snapshot_active does not filter expired (callers do), so the
        // expired entry is present in the snapshot.
        assert_eq!(snap.len(), 2);
        assert!(snap.contains_key(&sig_live));
        // list_active filters expired at query time.
        let listed = store.list_active().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].signature, sig_live);
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
            // Length-too-short branch: 16-hex tail with a short prefix
            // fails the `len < 34` guard.
            "foo:bar:0123456789abcdef",
            // Realistic legacy 16-hex shape (pre-0.5.28). Long enough
            // to clear the length guard, fails the colon-position
            // check at `bytes[len - 33]`. Locks in the rejection of
            // every previously-valid 16-hex signature.
            "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef",
            // 33-byte signature shaped `:<32 hex>` with empty prefix.
            // Was accepted by the pre-amend `len < 33` check, now
            // rejected by `len < 34` since the kind segment must be
            // non-empty.
            ":0123456789abcdef0123456789abcdef",
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
        // Build a fresh map containing the expired entry and swap it in.
        let mut active = store.active.write().await;
        let mut new_map = (**active).clone();
        new_map.insert(sig.clone(), entry);
        *active = Arc::new(new_map);
        drop(active);
        assert!(store.list_active().await.is_empty());
    }

    #[test]
    fn validate_signature_accepts_compute_signature_format() {
        // Mirror the format used by `acknowledgments::compute_signature`.
        let s = "n_plus_one_sql:order-svc:_api_v1_orders:0123456789abcdef0123456789abcdef";
        assert!(validate_signature(s).is_ok());
    }

    #[test]
    fn validate_signature_accepts_service_with_colon() {
        // Service names from OTLP can contain colons, only the tail is
        // fixed-format.
        let s = "n_plus_one_sql:svc:with:colons:_endpoint:0123456789abcdef0123456789abcdef";
        assert!(validate_signature(s).is_ok());
    }

    #[tokio::test]
    async fn ack_truncates_oversized_by_and_reason() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        let store = AckStore::new(path).await.unwrap();
        let sig = valid_sig("foo:svc:_endpoint");
        let entry = AckEntry {
            by: "x".repeat(MAX_BY_LEN + 100),
            reason: Some("y".repeat(MAX_REASON_LEN + 500)),
            ..sample_entry(&sig, AckAction::Ack)
        };
        store.ack(entry).await.unwrap();
        let active = store.list_active().await;
        assert_eq!(active[0].by.len(), MAX_BY_LEN);
        assert_eq!(active[0].reason.as_ref().unwrap().len(), MAX_REASON_LEN);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_append_refuses_symlink() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.jsonl");
        let link = dir.path().join("acks.jsonl");
        tokio::fs::write(&target, b"").await.unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = AckStore::new(link.clone()).await.unwrap_err();
        assert!(
            matches!(err, AckError::SymlinkRefused { .. }),
            "got {err:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_compaction_resets_weak_permissions_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        // Plant a pre-existing file with world-readable permissions.
        tokio::fs::write(&path, b"").await.unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Startup compaction unconditionally rewrites the file with
        // mode 0600, eliminating any weak-permission window an
        // attacker could have planted before daemon launch.
        let _store = AckStore::new(path.clone()).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode after startup is {mode:o}");
    }

    #[tokio::test]
    async fn replay_rejects_overlong_line_without_full_buffer_alloc() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("acks.jsonl");
        // One unterminated giant line over the per-line cap, no newline
        // until well past MAX_ACK_ENTRY_BYTES. The reader should bail
        // with EntryTooLarge rather than allocating the whole thing
        // into a single String before parsing.
        let blob = vec![b'x'; MAX_ACK_ENTRY_BYTES + 100];
        tokio::fs::write(&path, blob).await.unwrap();
        let err = AckStore::new(path).await.unwrap_err();
        assert!(matches!(err, AckError::EntryTooLarge), "got {err:?}");
    }
}
