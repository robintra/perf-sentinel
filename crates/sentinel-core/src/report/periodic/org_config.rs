//! Operator-supplied organisation / methodology / scope TOML.
//! Loaded by the daemon startup gate and by `disclose` to fill the
//! static fields of a [`PeriodicReport`]. See design doc 08.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use super::schema::{
    Conformance, DisabledPattern, ExcludedApp, ExcludedEnv, Notes, OrgIdentifiers, Organisation,
};

#[derive(Debug, Clone, Deserialize)]
pub struct OrgConfig {
    pub organisation: Organisation,
    pub methodology: MethodologyTemplate,
    pub scope_manifest: ScopeManifestTemplate,
    #[serde(default)]
    pub notes: Notes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MethodologyTemplate {
    pub sci_specification: String,
    #[serde(default)]
    pub enabled_patterns: Vec<String>,
    #[serde(default)]
    pub disabled_patterns: Vec<DisabledPattern>,
    pub conformance: Conformance,
    pub calibration: CalibrationTemplate,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CalibrationTemplate {
    #[serde(default)]
    pub cloud_regions: Vec<String>,
    pub carbon_intensity_source: String,
    pub specpower_table_version: String,
    #[serde(default)]
    pub scaphandre_used: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScopeManifestTemplate {
    pub total_applications_declared: u32,
    #[serde(default)]
    pub applications_excluded: Vec<ExcludedApp>,
    #[serde(default)]
    pub environments_measured: Vec<String>,
    #[serde(default)]
    pub environments_excluded: Vec<ExcludedEnv>,
    #[serde(default)]
    pub total_requests_in_period: Option<u64>,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OrgConfigError {
    #[error("failed to read org-config at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse org-config at {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("org-config path {path} is a symlink; refusing to follow")]
    SymlinkRefused { path: String },
}

/// Read an org-config TOML file from disk.
///
/// # Errors
///
/// Returns [`OrgConfigError::Io`] if the file cannot be opened or read,
/// and [`OrgConfigError::Parse`] when the TOML structure does not
/// satisfy the required shape.
pub fn load_from_path(path: impl AsRef<Path>) -> Result<OrgConfig, OrgConfigError> {
    let path = path.as_ref();
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return Err(OrgConfigError::SymlinkRefused {
            path: path.display().to_string(),
        });
    }
    let raw = std::fs::read_to_string(path).map_err(|source| OrgConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    toml::from_str(&raw).map_err(|source| OrgConfigError::Parse {
        path: path.display().to_string(),
        source,
    })
}

/// Collect static-field validation errors for an `intent = "official"`
/// disclosure. Returns an empty vec when the org-config is publishable.
#[must_use]
pub fn validate_for_official(cfg: &OrgConfig) -> Vec<String> {
    let mut errors = Vec::new();
    if cfg.organisation.name.trim().is_empty() {
        errors.push("organisation.name must not be empty".to_string());
    }
    if !is_iso_alpha2(&cfg.organisation.country) {
        errors.push(format!(
            "organisation.country must be a 2-letter ISO 3166-1 alpha-2 code in upper case, got {:?}",
            cfg.organisation.country
        ));
    }
    for (idx, excluded) in cfg.scope_manifest.applications_excluded.iter().enumerate() {
        if excluded.reason.trim().is_empty() {
            errors.push(format!(
                "scope_manifest.applications_excluded[{idx}] has empty reason"
            ));
        }
    }
    let src = cfg.methodology.calibration.carbon_intensity_source.trim();
    if src.is_empty() {
        errors
            .push("methodology.calibration.carbon_intensity_source must not be empty".to_string());
    } else if !matches!(src, "electricity_maps" | "static_tables" | "mixed") {
        errors.push(format!(
            "methodology.calibration.carbon_intensity_source must be one of \"electricity_maps\", \"static_tables\", \"mixed\", got {src:?}"
        ));
    }
    if cfg
        .methodology
        .calibration
        .specpower_table_version
        .trim()
        .is_empty()
    {
        errors
            .push("methodology.calibration.specpower_table_version must not be empty".to_string());
    }
    let core_required = super::schema::core_patterns_required();
    for core in &core_required {
        if !cfg.methodology.enabled_patterns.contains(core) {
            errors.push(format!(
                "methodology.enabled_patterns must include core pattern {core:?}"
            ));
        }
        if cfg
            .methodology
            .disabled_patterns
            .iter()
            .any(|d| &d.name == core)
        {
            errors.push(format!(
                "methodology.disabled_patterns must not include core pattern {core:?}"
            ));
        }
    }
    errors
}

fn is_iso_alpha2(s: &str) -> bool {
    s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase())
}

impl OrgIdentifiers {
    /// Helper for tests and example construction.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sample_toml() -> &'static str {
        r#"
[organisation]
name = "Example SAS"
country = "FR"
sector = "62.01"

[organisation.identifiers]
siren = "123456789"
domain = "example.fr"

[methodology]
sci_specification = "ISO/IEC 21031:2024"
enabled_patterns = [
    "n_plus_one_sql",
    "n_plus_one_http",
    "redundant_sql",
    "redundant_http",
    "slow_sql",
]
disabled_patterns = []
conformance = "core-required"

[methodology.calibration]
cloud_regions = ["eu-west-3"]
carbon_intensity_source = "electricity_maps"
specpower_table_version = "2026-04-24"
scaphandre_used = false

[scope_manifest]
total_applications_declared = 4
environments_measured = ["prod"]
"#
    }

    fn write_toml(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file
    }

    #[test]
    fn loads_complete_config() {
        let file = write_toml(sample_toml());
        let cfg = load_from_path(file.path()).unwrap();
        assert_eq!(cfg.organisation.name, "Example SAS");
        assert_eq!(cfg.organisation.country, "FR");
        assert_eq!(cfg.scope_manifest.total_applications_declared, 4);
        assert_eq!(cfg.methodology.calibration.cloud_regions, vec!["eu-west-3"]);
    }

    #[test]
    fn validate_for_official_passes_complete_config() {
        let file = write_toml(sample_toml());
        let cfg = load_from_path(file.path()).unwrap();
        assert!(validate_for_official(&cfg).is_empty());
    }

    #[test]
    fn validate_for_official_flags_lowercase_country() {
        let toml = sample_toml().replace("country = \"FR\"", "country = \"fr\"");
        let file = write_toml(&toml);
        let cfg = load_from_path(file.path()).unwrap();
        let errors = validate_for_official(&cfg);
        assert!(
            errors.iter().any(|e| e.contains("country")),
            "got {errors:?}"
        );
    }

    #[test]
    fn validate_for_official_flags_missing_core_pattern() {
        let toml = sample_toml().replace("\"n_plus_one_sql\",\n", "");
        let file = write_toml(&toml);
        let cfg = load_from_path(file.path()).unwrap();
        let errors = validate_for_official(&cfg);
        assert!(
            errors.iter().any(|e| e.contains("n_plus_one_sql")),
            "got {errors:?}"
        );
    }

    #[test]
    fn missing_file_returns_io_error() {
        let err = load_from_path("/no/such/path/org.toml").unwrap_err();
        assert!(matches!(err, OrgConfigError::Io { .. }));
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let file = write_toml("[organisation\nname = broken");
        let err = load_from_path(file.path()).unwrap_err();
        assert!(matches!(err, OrgConfigError::Parse { .. }));
    }
}
