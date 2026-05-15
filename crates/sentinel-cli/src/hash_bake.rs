//! `perf-sentinel hash-bake` subcommand.
//!
//! Reads a `PeriodicReport` JSON file, computes the canonical SHA-256
//! `content_hash` via `sentinel_core::report::periodic::compute_content_hash`,
//! writes it into `integrity.content_hash`, and saves the report back
//! atomically. Intended for test fixture generation and debugging when a
//! report's hash has drifted from canonical.
//!
//! The input file is capped at 64 MiB. The temp file is created
//! with `create_new` (and `O_NOFOLLOW` on unix) so a stale
//! `<output>.tmp` or a hostile symlink at that path triggers an exit
//! 3 instead of silently clobbering the target.
//!
//! Exit codes:
//!
//! - `0` success
//! - `1` refused (`integrity.signature` already populated and
//!   `--allow-signed` not passed)
//! - `3` `INPUT_ERROR` (file unreadable, JSON parse error, file over
//!   the size cap, temp file path collision, write failure)

use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use sentinel_core::report::periodic::{compute_content_hash, schema::PeriodicReport};
use sentinel_core::text_safety::sanitize_for_terminal;

pub const EXIT_OK: i32 = 0;
pub const EXIT_REFUSED: i32 = 1;
pub const EXIT_INPUT_ERROR: i32 = 3;

// Hard cap on the input report size. Legitimate periodic disclosures
// are well under 10 MB, 64 MiB leaves room for outliers (deep G1
// archives, large per-service breakdowns) while bounding the
// allocation a single CLI invocation will perform.
const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;

/// Entry point invoked from `main.rs` dispatch.
pub fn cmd_hash_bake(report_path: &Path, output_path: &Path, allow_signed: bool) -> i32 {
    // Refuse oversized inputs before allocating to bound the worst
    // case (poisoned mirror feeding a multi-GB JSON to a CI runner).
    match fs::metadata(report_path) {
        Ok(meta) if meta.len() > MAX_REPORT_BYTES => {
            eprintln!(
                "Error: report at {} is {} bytes, exceeds the {}-byte cap.",
                sanitize_for_terminal(&report_path.display().to_string()),
                meta.len(),
                MAX_REPORT_BYTES
            );
            return EXIT_INPUT_ERROR;
        }
        Err(err) => {
            eprintln!(
                "Error: failed to stat report at {}: {err}",
                sanitize_for_terminal(&report_path.display().to_string())
            );
            return EXIT_INPUT_ERROR;
        }
        Ok(_) => {}
    }

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
            "Error: report at {} already has integrity.signature populated.\n\
             The canonical content_hash excludes integrity.signature, so re-baking does not invalidate an existing signature.\n\
             This refusal guards against accidental overwrites of signed reports. Pass --allow-signed to proceed.",
            sanitize_for_terminal(&report_path.display().to_string())
        );
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
// `rename`. Each post-open failure removes the temp best-effort.
fn write_atomic_pretty(report: &PeriodicReport, output: &Path) -> Result<(), i32> {
    // Append `.tmp` instead of replacing the extension so a path like
    // `report.tmp` (whose extension is already `tmp`) does not collide
    // with itself, which would defeat the atomic guarantee.
    let tmp = {
        let mut buf = output.as_os_str().to_owned();
        buf.push(".tmp");
        PathBuf::from(buf)
    };
    // `create_new` fails if the temp already exists, and `O_NOFOLLOW`
    // refuses to follow a symlink at that path. Together they defeat
    // the symlink-clobber attack a co-located process could attempt
    // on a shared CI workspace.
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match opts.open(&tmp) {
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
        // The temp file must not survive a successful bake.
        let leaked_tmp = tmp.path().join("out.json.tmp");
        assert!(
            !leaked_tmp.exists(),
            "temp file leaked after successful rename: {}",
            leaked_tmp.display()
        );
    }

    #[test]
    fn hash_bake_handles_output_already_named_dot_tmp() {
        // Regression: `with_extension("tmp")` would have collided here,
        // since the output path already ends in `.tmp`. Appending `.tmp`
        // produces `out.tmp.tmp`, which is distinct and preserves the
        // atomic guarantee.
        let tmp = tempfile::tempdir().expect("tempdir");
        let in_path = tmp.path().join("in.json");
        let out_path = tmp.path().join("out.tmp");
        fs::write(&in_path, placeholder_g2_bytes()).unwrap();

        let code = cmd_hash_bake(&in_path, &out_path, false);
        assert_eq!(code, EXIT_OK);
        assert!(out_path.exists(), "output not created");
        let baked: PeriodicReport = serde_json::from_slice(&fs::read(&out_path).unwrap()).unwrap();
        assert_eq!(
            baked.integrity.content_hash,
            compute_content_hash(&baked).unwrap()
        );
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
        // The signature-stable canonicalization blanks integrity.signature,
        // so the canonical hash is identical before and after baking a
        // signed report.
        assert_eq!(baked.integrity.content_hash, expected_hash);
        assert!(baked.integrity.signature.is_some());
    }

    #[test]
    fn hash_bake_input_error_on_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out_path = tmp.path().join("out.json");
        let missing = tmp.path().join("does-not-exist.json");
        let code = cmd_hash_bake(&missing, &out_path, false);
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

    #[test]
    fn hash_bake_input_error_on_oversized_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let in_path = tmp.path().join("huge.json");
        let out_path = tmp.path().join("out.json");
        // Reserve a virtual file just over the cap. `set_len` extends
        // sparsely on every supported filesystem, so the bytes are not
        // actually written.
        let file = fs::File::create(&in_path).unwrap();
        file.set_len(MAX_REPORT_BYTES + 1).unwrap();
        drop(file);

        let code = cmd_hash_bake(&in_path, &out_path, false);
        assert_eq!(code, EXIT_INPUT_ERROR);
        assert!(!out_path.exists());
    }

    #[test]
    fn hash_bake_input_error_when_temp_path_already_exists() {
        // Pre-existing `<output>.tmp` (stale or hostile) must abort the
        // bake instead of being silently truncated. Guards against the
        // symlink-clobber attack on shared workspaces.
        let tmp = tempfile::tempdir().expect("tempdir");
        let in_path = tmp.path().join("in.json");
        let out_path = tmp.path().join("out.json");
        let collision = tmp.path().join("out.json.tmp");
        fs::write(&in_path, placeholder_g2_bytes()).unwrap();
        fs::write(&collision, b"stale").unwrap();

        let code = cmd_hash_bake(&in_path, &out_path, false);
        assert_eq!(code, EXIT_INPUT_ERROR);
        assert!(!out_path.exists(), "output must not be created");
        // The collision file is untouched; the operator can inspect or
        // remove it themselves.
        assert_eq!(fs::read(&collision).unwrap(), b"stale");
    }
}
