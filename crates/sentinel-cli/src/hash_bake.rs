//! `perf-sentinel hash-bake` subcommand.
//!
//! Reads a `PeriodicReport` JSON file, computes the canonical SHA-256
//! `content_hash` via `sentinel_core::report::periodic::compute_content_hash`,
//! writes it into `integrity.content_hash`, and saves the report back
//! atomically. Intended for test fixture generation and debugging when a
//! report's hash has drifted from canonical.
//!
//! Exit codes:
//!
//! - `0` success
//! - `1` refused (`integrity.signature` already populated and
//!   `--allow-signed` not passed)
//! - `3` `INPUT_ERROR` (read failure, JSON parse error, write failure)

use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

use sentinel_core::report::periodic::{compute_content_hash, schema::PeriodicReport};
use sentinel_core::text_safety::sanitize_for_terminal;

pub const EXIT_OK: i32 = 0;
pub const EXIT_REFUSED: i32 = 1;
pub const EXIT_INPUT_ERROR: i32 = 3;

/// Entry point invoked from `main.rs` dispatch.
pub fn cmd_hash_bake(report_path: &Path, output_path: &Path, allow_signed: bool) -> i32 {
    let bytes = match fs::read(report_path) {
        Ok(b) => b,
        Err(err) => {
            eprintln!(
                "Error: failed to read report at {}: {err}",
                sanitize_for_terminal(&report_path.display().to_string())
            );
            return EXIT_INPUT_ERROR;
        }
    };

    let mut report: PeriodicReport = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(err) => {
            eprintln!(
                "Error: failed to parse report at {}: {err}",
                sanitize_for_terminal(&report_path.display().to_string())
            );
            return EXIT_INPUT_ERROR;
        }
    };

    if report.integrity.signature.is_some() && !allow_signed {
        eprintln!(
            "Error: report at {} already has integrity.signature populated.",
            sanitize_for_terminal(&report_path.display().to_string())
        );
        eprintln!(
            "Re-baking would not invalidate the signature (content_hash blanches signature in canonical form), but you should confirm the intent."
        );
        eprintln!("Pass --allow-signed to proceed.");
        return EXIT_REFUSED;
    }

    let hash = match compute_content_hash(&report) {
        Ok(h) => h,
        Err(err) => {
            eprintln!("Error: failed to compute content hash: {err}");
            return EXIT_INPUT_ERROR;
        }
    };
    report.integrity.content_hash.clone_from(&hash);

    if let Err(code) = write_atomic_pretty(&report, output_path) {
        return code;
    }

    println!("Content hash baked: {hash}");
    println!(
        "Written to: {}",
        sanitize_for_terminal(&output_path.display().to_string())
    );
    EXIT_OK
}

// Write `report` to `output` atomically via a sibling `.tmp` file and
// `rename`. Cleans up the temp on rename failure (best effort).
fn write_atomic_pretty(report: &PeriodicReport, output: &Path) -> Result<(), i32> {
    let tmp = output.with_extension("tmp");
    let file = match fs::File::create(&tmp) {
        Ok(f) => f,
        Err(err) => {
            eprintln!(
                "Error: failed to create temp file at {}: {err}",
                sanitize_for_terminal(&tmp.display().to_string())
            );
            return Err(EXIT_INPUT_ERROR);
        }
    };
    let mut writer = BufWriter::new(file);
    if let Err(err) = serde_json::to_writer_pretty(&mut writer, report) {
        eprintln!("Error: failed to serialize report: {err}");
        let _ = fs::remove_file(&tmp);
        return Err(EXIT_INPUT_ERROR);
    }
    if let Err(err) = writer.write_all(b"\n") {
        eprintln!("Error: failed to write trailing newline: {err}");
        let _ = fs::remove_file(&tmp);
        return Err(EXIT_INPUT_ERROR);
    }
    if let Err(err) = writer.flush() {
        eprintln!("Error: failed to flush temp file: {err}");
        let _ = fs::remove_file(&tmp);
        return Err(EXIT_INPUT_ERROR);
    }
    drop(writer);
    if let Err(err) = fs::rename(&tmp, output) {
        eprintln!(
            "Error: failed to rename temp file to {}: {err}",
            sanitize_for_terminal(&output.display().to_string())
        );
        let _ = fs::remove_file(&tmp);
        return Err(EXIT_INPUT_ERROR);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::report::periodic::schema::SignatureMetadata;
    use std::path::PathBuf;

    fn workspace_doc(rel: &str) -> PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest_dir).join("..").join("..").join(rel)
    }

    fn placeholder_g2_bytes() -> Vec<u8> {
        fs::read(workspace_doc(
            "docs/schemas/examples/example-official-public-G2.json",
        ))
        .expect("read G2 example")
    }

    fn dummy_signature() -> SignatureMetadata {
        SignatureMetadata {
            format: "sigstore-cosign-intoto-v1".to_string(),
            bundle_url: "https://example.invalid/x.sig".to_string(),
            signer_identity: "user@example.invalid".to_string(),
            signer_issuer: "https://accounts.google.com".to_string(),
            rekor_url: "https://rekor.sigstore.dev".to_string(),
            rekor_log_index: 1,
            signed_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn hash_bake_writes_canonical_hash_on_unsigned_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let in_path = tmp.path().join("in.json");
        let out_path = tmp.path().join("out.json");
        fs::write(&in_path, placeholder_g2_bytes()).unwrap();

        let code = cmd_hash_bake(&in_path, &out_path, false);
        assert_eq!(code, EXIT_OK);

        let baked_bytes = fs::read(&out_path).unwrap();
        let baked: PeriodicReport = serde_json::from_slice(&baked_bytes).unwrap();
        let recomputed = compute_content_hash(&baked).unwrap();
        assert_eq!(baked.integrity.content_hash, recomputed);
        let zero_placeholder = format!("sha256:{}", "0".repeat(64));
        assert_ne!(baked.integrity.content_hash, zero_placeholder);
    }

    #[test]
    fn hash_bake_in_place_works() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("report.json");
        fs::write(&path, placeholder_g2_bytes()).unwrap();

        let code = cmd_hash_bake(&path, &path, false);
        assert_eq!(code, EXIT_OK);

        let baked: PeriodicReport = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let recomputed = compute_content_hash(&baked).unwrap();
        assert_eq!(baked.integrity.content_hash, recomputed);
    }

    #[test]
    fn hash_bake_refuses_signed_report_without_flag() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let in_path = tmp.path().join("in.json");
        let out_path = tmp.path().join("out.json");
        let mut report: PeriodicReport = serde_json::from_slice(&placeholder_g2_bytes()).unwrap();
        report.integrity.signature = Some(dummy_signature());
        fs::write(&in_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();

        let code = cmd_hash_bake(&in_path, &out_path, false);
        assert_eq!(code, EXIT_REFUSED);
        assert!(
            !out_path.exists(),
            "refused bake must not create output file"
        );
    }

    #[test]
    fn hash_bake_accepts_signed_report_with_allow_signed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let in_path = tmp.path().join("in.json");
        let out_path = tmp.path().join("out.json");
        let mut report: PeriodicReport = serde_json::from_slice(&placeholder_g2_bytes()).unwrap();
        report.integrity.signature = Some(dummy_signature());
        let expected_hash = compute_content_hash(&report).unwrap();
        fs::write(&in_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();

        let code = cmd_hash_bake(&in_path, &out_path, true);
        assert_eq!(code, EXIT_OK);
        let baked: PeriodicReport = serde_json::from_slice(&fs::read(&out_path).unwrap()).unwrap();
        // POST_SIGN_FIELDS blanching means the signature does not affect
        // the canonical hash, so the written value must match the
        // pre-bake computation.
        assert_eq!(baked.integrity.content_hash, expected_hash);
        assert!(baked.integrity.signature.is_some());
    }

    #[test]
    fn hash_bake_input_error_on_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out_path = tmp.path().join("out.json");
        let code = cmd_hash_bake(
            Path::new("/nonexistent/path/to/missing.json"),
            &out_path,
            false,
        );
        assert_eq!(code, EXIT_INPUT_ERROR);
    }

    #[test]
    fn hash_bake_input_error_on_invalid_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let in_path = tmp.path().join("in.json");
        let out_path = tmp.path().join("out.json");
        fs::write(&in_path, b"not json").unwrap();

        let code = cmd_hash_bake(&in_path, &out_path, false);
        assert_eq!(code, EXIT_INPUT_ERROR);
    }
}
