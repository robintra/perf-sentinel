//! Deterministic SHA-256 content hash for a [`PeriodicReport`].
//! Canonical form (sorted keys, compact JSON, blanked `content_hash`)
//! and design rationale: `docs/design/08-PERIODIC-DISCLOSURE.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::errors::HashError;
use super::schema::PeriodicReport;

/// Soft cap on the binary read in [`binary_hash`]. perf-sentinel release
/// binaries are tens of MiB; this guards against `current_exe` resolving
/// to an unexpectedly large path (e.g. a procfs link).
const BINARY_HASH_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Compute the canonical SHA-256 content hash of a report.
///
/// The returned string is prefixed with `"sha256:"` and contains 64
/// lowercase hex characters.
///
/// # Errors
///
/// Returns [`HashError::Serialize`] if the report cannot be serialised to
/// JSON, which in practice only happens if a float is non-finite.
pub fn compute_content_hash(report: &PeriodicReport) -> Result<String, HashError> {
    let mut value = serde_json::to_value(report)?;
    blank_content_hash(&mut value);
    let canonical = canonicalize(value);
    let bytes = serde_json::to_vec(&canonical)?;
    Ok(format_sha256(&bytes))
}

/// Zero out every field that the post-`disclose` signing workflow may
/// add to or mutate on the report: `integrity.content_hash` itself,
/// the typed `integrity.signature` / `integrity.binary_attestation`
/// locators that an operator pastes in after `cosign attest`, and
/// `report_metadata.integrity_level` which flips from `hash-only` to
/// `signed` / `signed-with-attestation`. Keeping the canonical form
/// invariant under that exact set of mutations is what lets a
/// downstream consumer recompute the same `content_hash` against a
/// signed report.
fn blank_content_hash(v: &mut Value) {
    if let Some(integrity) = v.get_mut("integrity").and_then(Value::as_object_mut) {
        integrity.insert("content_hash".to_string(), Value::String(String::new()));
        integrity.insert("signature".to_string(), Value::Null);
        integrity.insert("binary_attestation".to_string(), Value::Null);
    }
    if let Some(metadata) = v.get_mut("report_metadata").and_then(Value::as_object_mut) {
        metadata.insert("integrity_level".to_string(), Value::String(String::new()));
    }
}

/// Recursively re-build every JSON object via `BTreeMap` so the output
/// has sorted keys regardless of how `serde_json::Map` happens to be
/// configured upstream. Removing this collect would silently break the
/// hash determinism the moment a transitive crate flips the
/// `serde_json/preserve_order` feature.
fn canonicalize(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(k, val)| (k, canonicalize(val)))
                .collect();
            let mut out = serde_json::Map::new();
            for (k, val) in sorted {
                out.insert(k, val);
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

fn format_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Hash the running binary at `std::env::current_exe()` and return the
/// `"sha256:<64-hex>"` string used by
/// [`crate::report::periodic::schema::Integrity::binary_hash`].
///
/// Streams the file via a `BufReader` (no whole-binary allocation) and
/// caps the read at `BINARY_HASH_MAX_BYTES` so an unexpectedly large
/// `current_exe` resolution cannot OOM the process.
///
/// # Errors
///
/// Returns the I/O error from `current_exe` or the file read when the
/// running executable cannot be resolved or read.
pub fn binary_hash() -> std::io::Result<String> {
    let path = std::env::current_exe()?;
    let file = std::fs::File::open(&path)?;
    let total_len = file.metadata().map_or(0, |m| m.len());
    if total_len > BINARY_HASH_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "binary at {} exceeds {} byte cap ({} bytes), refusing to hash a truncated view",
                path.display(),
                BINARY_HASH_MAX_BYTES,
                total_len
            ),
        ));
    }
    let mut reader = std::io::BufReader::new(file).take(BINARY_HASH_MAX_BYTES);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// Hash an arbitrary file by path and return the 64-hex SHA-256 digest
/// (without the `sha256:` prefix, to match the in-toto v1 subject digest
/// convention). Streams via the same `BUF` size and `BINARY_HASH_MAX_BYTES`
/// cap as [`binary_hash`].
///
/// # Errors
///
/// Returns the I/O error from opening or reading the file, or
/// `InvalidData` if the file exceeds the safety cap.
pub fn compute_file_sha256_hex(path: &std::path::Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let total_len = file.metadata().map_or(0, |m| m.len());
    if total_len > BINARY_HASH_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file at {} exceeds {} byte cap ({} bytes)",
                path.display(),
                BINARY_HASH_MAX_BYTES,
                total_len
            ),
        ));
    }
    let mut reader = std::io::BufReader::new(file).take(BINARY_HASH_MAX_BYTES);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::periodic::schema::{
        Application, Confidentiality, PeriodicReport, ReportIntent,
    };
    use crate::report::periodic::test_fixtures;

    fn sample_report() -> PeriodicReport {
        test_fixtures::sample_report(
            ReportIntent::Official,
            Confidentiality::Public,
            vec![Application::G1(test_fixtures::sample_g1_application())],
        )
    }

    #[test]
    fn hash_is_deterministic() {
        let r = sample_report();
        let first = compute_content_hash(&r).unwrap();
        for _ in 0..100 {
            assert_eq!(compute_content_hash(&r).unwrap(), first);
        }
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), 7 + 64);
        assert!(first[7..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_changes_on_aggregate_mutation() {
        let r = sample_report();
        let baseline = compute_content_hash(&r).unwrap();

        let mut mutated = r.clone();
        mutated.aggregate.total_energy_kwh += 0.000_001;
        let after = compute_content_hash(&mutated).unwrap();

        assert_ne!(baseline, after);
    }

    #[test]
    fn hash_ignores_existing_content_hash() {
        let mut r = sample_report();
        r.integrity.content_hash = "sha256:aaaa".to_string();
        let first = compute_content_hash(&r).unwrap();

        r.integrity.content_hash = "sha256:bbbb".to_string();
        let second = compute_content_hash(&r).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn canonicalize_is_key_order_invariant() {
        let a = serde_json::json!({ "alpha": 1, "beta": 2, "gamma": 3 });
        let b = serde_json::json!({ "gamma": 3, "alpha": 1, "beta": 2 });
        let ca = canonicalize(a);
        let cb = canonicalize(b);
        assert_eq!(
            serde_json::to_vec(&ca).unwrap(),
            serde_json::to_vec(&cb).unwrap()
        );
    }

    #[test]
    fn canonicalize_recurses_into_nested_objects() {
        let a = serde_json::json!({
            "outer": { "z": 1, "a": 2 },
            "list": [{ "b": 1, "a": 2 }]
        });
        let b = serde_json::json!({
            "list": [{ "a": 2, "b": 1 }],
            "outer": { "a": 2, "z": 1 }
        });
        assert_eq!(
            serde_json::to_vec(&canonicalize(a)).unwrap(),
            serde_json::to_vec(&canonicalize(b)).unwrap(),
        );
    }

    #[test]
    fn hash_blanks_content_hash_without_removing_key() {
        let r = sample_report();
        let mut v = serde_json::to_value(&r).unwrap();
        blank_content_hash(&mut v);
        let integrity = v.get("integrity").and_then(Value::as_object).unwrap();
        assert!(integrity.contains_key("content_hash"));
        assert_eq!(
            integrity.get("content_hash"),
            Some(&Value::String(String::new()))
        );
    }

    #[test]
    fn format_sha256_known_vector() {
        // SHA-256 of the empty string, well-known constant.
        let empty = format_sha256(&[]);
        assert_eq!(
            empty,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hash_is_invariant_under_post_sign_locator_addition() {
        use crate::report::periodic::schema::{
            BinaryAttestationMetadata, IntegrityLevel, SignatureMetadata,
        };
        // The operator workflow adds `integrity.signature`,
        // `integrity.binary_attestation`, and bumps `integrity_level`
        // AFTER `disclose` has already committed `content_hash`. The
        // canonical form must be invariant under those edits so a
        // signed disclosure still verifies.
        let r = sample_report();
        let baseline = compute_content_hash(&r).unwrap();

        let mut signed = r.clone();
        signed.report_metadata.integrity_level = IntegrityLevel::Signed;
        signed.integrity.signature = Some(SignatureMetadata {
            format: "sigstore-cosign-intoto-v1".to_string(),
            bundle_url: "https://example.fr/bundle.sig".to_string(),
            signer_identity: "ci@example.fr".to_string(),
            signer_issuer: "https://accounts.google.com".to_string(),
            rekor_url: "https://rekor.sigstore.dev".to_string(),
            rekor_log_index: 42,
            signed_at: "2026-05-14T12:00:00Z".to_string(),
        });
        assert_eq!(compute_content_hash(&signed).unwrap(), baseline);

        signed.report_metadata.integrity_level = IntegrityLevel::SignedWithAttestation;
        signed.integrity.binary_attestation = Some(BinaryAttestationMetadata {
            format: "slsa-provenance-v1".to_string(),
            attestation_url: "https://gh/p.intoto.jsonl".to_string(),
            builder_id: "https://github.com/actions/runner".to_string(),
            git_tag: "v0.7.0".to_string(),
            git_commit: "deadbeef".to_string(),
            slsa_level: "L2".to_string(),
        });
        assert_eq!(compute_content_hash(&signed).unwrap(), baseline);
    }
}
