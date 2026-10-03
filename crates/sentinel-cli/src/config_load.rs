//! Config file loading: the main `.perf-sentinel.toml` plus its numbered `.perf-sentinel.d` fragments.

use crate::EXIT_TOOLING_ERROR;
use sentinel_core::config::Config;
use std::path::PathBuf;

pub(super) fn load_config(path: Option<&std::path::Path>) -> Config {
    load_config_with_flags(path, None)
}

/// `flags` is a last TOML document carrying command-line overrides, merged
/// before the one validation pass so its advisories read the final values.
pub(super) fn load_config_with_flags(
    path: Option<&std::path::Path>,
    flags: Option<&str>,
) -> Config {
    let config_path = path.map_or_else(
        || PathBuf::from(".perf-sentinel.toml"),
        std::path::Path::to_path_buf,
    );

    match load_config_files(&config_path, path.is_some(), flags) {
        Ok(config) => config,
        Err(error) => {
            if error.starts_with("read ") {
                eprintln!("Error reading config: {error}");
            } else {
                eprintln!("Error parsing config: {error}");
            }
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    }
}

pub(super) fn load_config_files(
    config_path: &std::path::Path,
    require_main: bool,
    flags: Option<&str>,
) -> Result<Config, String> {
    let parent = config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let fragment_dir = parent.join(".perf-sentinel.d");
    let mut fragments = Vec::new();
    match std::fs::read_dir(&fragment_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry =
                    entry.map_err(|error| format!("read {}: {error}", fragment_dir.display()))?;
                let fragment_path = entry.path();
                if fragment_path.extension().and_then(std::ffi::OsStr::to_str) != Some("toml") {
                    continue;
                }
                let name = entry.file_name().into_string().map_err(|_| {
                    format!("{} contains a non-UTF-8 filename", fragment_dir.display())
                })?;
                let priority = fragment_priority(&name).ok_or_else(|| {
                    format!(
                        "invalid fragment name {name:?} in {}. Expected NN-lowercase-name.toml",
                        fragment_dir.display()
                    )
                })?;
                fragments.push((priority, name, fragment_path));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("read {}: {error}", fragment_dir.display())),
    }
    fragments.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    for pair in fragments.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(format!(
                "duplicate fragment priority {:02}: {} and {}",
                pair[0].0, pair[0].1, pair[1].1
            ));
        }
    }

    let mut documents = Vec::with_capacity(fragments.len() + 1);
    for (_, name, fragment_path) in fragments {
        let content = std::fs::read_to_string(&fragment_path)
            .map_err(|error| format!("read {}: {error}", fragment_path.display()))?;
        documents.push((name, content));
    }
    match std::fs::read_to_string(config_path) {
        Ok(content) => documents.push((config_path.display().to_string(), content)),
        Err(error) if !require_main && error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(config_path) {
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return Err(format!("read {}: {error}", config_path.display())),
                Err(metadata_error) => {
                    return Err(format!(
                        "inspect {}: {metadata_error}",
                        config_path.display()
                    ));
                }
            }
        }
        Err(error) => return Err(format!("read {}: {error}", config_path.display())),
    }
    // No file at all still goes through the parser: the environment
    // overrides live in the conversion, so a container started with the
    // keys in its environment and no mounted config gets them applied
    // and validated instead of running with its write routes open.
    let mut borrowed: Vec<_> = documents
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_str()))
        .collect();
    borrowed.extend(flags.map(|flags| ("command-line flags", flags)));
    sentinel_core::config::load_from_fragments(&borrowed).map_err(|error| error.to_string())
}

fn fragment_priority(name: &str) -> Option<u8> {
    let stem = name.strip_suffix(".toml")?;
    let (priority, slug) = stem.split_once('-')?;
    if priority.len() != 2
        || !priority.bytes().all(|byte| byte.is_ascii_digit())
        || slug.is_empty()
        || slug.starts_with('-')
        || slug.ends_with('-')
        || slug.contains("--")
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    priority.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_config_returns_default_when_no_file() {
        // No .perf-sentinel.toml in the test working directory
        let config = load_config(None);
        assert_eq!(config.detection.n_plus_one_threshold, 5);
        assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024);
    }

    #[test]
    fn load_config_reads_valid_file() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let config_path = dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            "[detection]\nn_plus_one_min_occurrences = 15\n",
        )
        .expect("failed to write config");

        let config = load_config(Some(&config_path));
        assert_eq!(config.detection.n_plus_one_threshold, 15);
    }

    #[test]
    fn config_fragments_load_in_priority_order_before_the_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let fragments = dir.path().join(".perf-sentinel.d");
        std::fs::create_dir(&fragments).unwrap();
        std::fs::write(
            fragments.join("10-detection.toml"),
            "[detection]\nn_plus_one_min_occurrences = 7\n",
        )
        .unwrap();
        std::fs::write(
            fragments.join("30-green.toml"),
            "[green]\ndefault_region = \"eu-west-3\"\n",
        )
        .unwrap();
        let main = dir.path().join(".perf-sentinel.toml");
        std::fs::write(&main, "[detection]\nn_plus_one_min_occurrences = 13\n").unwrap();

        let config = load_config_files(&main, false, None).unwrap();
        assert_eq!(config.detection.n_plus_one_threshold, 13);
        assert_eq!(config.green.default_region.as_deref(), Some("eu-west-3"));
    }

    #[test]
    fn config_fragments_work_without_a_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let fragments = dir.path().join(".perf-sentinel.d");
        std::fs::create_dir(&fragments).unwrap();
        std::fs::write(
            fragments.join("10-detection.toml"),
            "[detection]\nn_plus_one_min_occurrences = 9\n",
        )
        .unwrap();

        let config =
            load_config_files(&dir.path().join(".perf-sentinel.toml"), false, None).unwrap();
        assert_eq!(config.detection.n_plus_one_threshold, 9);
    }

    #[test]
    fn fragment_names_and_priorities_are_unambiguous() {
        for valid in ["00-base.toml", "30-green-cloud.toml", "99-local2.toml"] {
            assert!(fragment_priority(valid).is_some(), "{valid}");
        }
        for invalid in [
            "green.toml",
            "3-green.toml",
            "030-green.toml",
            "30-Green.toml",
            "30--green.toml",
            "30-green_.toml",
        ] {
            assert!(fragment_priority(invalid).is_none(), "{invalid}");
        }

        let dir = tempfile::tempdir().unwrap();
        let fragments = dir.path().join(".perf-sentinel.d");
        std::fs::create_dir(&fragments).unwrap();
        std::fs::write(fragments.join("30-green.toml"), "[green]\n").unwrap();
        std::fs::write(fragments.join("30-cloud.toml"), "[green.cloud]\n").unwrap();
        let error =
            load_config_files(&dir.path().join(".perf-sentinel.toml"), false, None).unwrap_err();
        assert!(error.contains("duplicate fragment priority 30"));
    }
}
