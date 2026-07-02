use super::*;

#[test]
fn default_config_has_safe_defaults() {
    let config = Config::default();
    assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024);
    assert_eq!(config.daemon.listen_addr, "127.0.0.1");
    assert_eq!(config.detection.n_plus_one_threshold, 5);
    assert_eq!(config.detection.window_duration_ms, 500);
    assert_eq!(config.daemon.trace_ttl_ms, 30_000);
    assert_eq!(config.daemon.max_active_traces, 10_000);
    assert_eq!(config.daemon.max_events_per_trace, 1_000);
}

#[test]
fn parse_empty_toml_gives_defaults() {
    let config = load_from_str("").unwrap();
    assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024);
}

#[test]
fn parse_partial_toml() {
    let config = load_from_str("[detection]\nn_plus_one_min_occurrences = 10").unwrap();
    assert_eq!(config.detection.n_plus_one_threshold, 10);
    assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024); // default preserved
}

#[test]
fn parse_window_config() {
    let config = load_from_str(
        "[detection]\nwindow_duration_ms = 1000\n\
         [daemon]\ntrace_ttl_ms = 60000\nmax_active_traces = 5000",
    )
    .unwrap();
    assert_eq!(config.detection.window_duration_ms, 1000);
    assert_eq!(config.daemon.trace_ttl_ms, 60_000);
    assert_eq!(config.daemon.max_active_traces, 5000);
}

#[test]
fn parse_sectioned_format() {
    let toml = r#"
[thresholds]
n_plus_one_sql_critical_max = 2
n_plus_one_http_warning_max = 5
io_waste_ratio_max = 0.50

[detection]
window_duration_ms = 1000
n_plus_one_min_occurrences = 10

[green]
enabled = false

[daemon]
listen_address = "0.0.0.0"
listen_port_http = 9418
listen_port_grpc = 9417
json_socket = "/var/run/perf-sentinel.sock"
max_active_traces = 20000
trace_ttl_ms = 60000
sampling_rate = 0.5
max_events_per_trace = 500
max_payload_size = 2097152
"#;
    let config = load_from_str(toml).unwrap();
    assert_eq!(config.thresholds.n_plus_one_sql_critical_max, 2);
    assert_eq!(config.thresholds.n_plus_one_http_warning_max, 5);
    assert!((config.thresholds.io_waste_ratio_max - 0.50).abs() < f64::EPSILON);
    assert_eq!(config.detection.n_plus_one_threshold, 10);
    assert_eq!(config.detection.window_duration_ms, 1000);
    assert!(!config.green.enabled);
    assert_eq!(config.daemon.listen_addr, "0.0.0.0");
    assert_eq!(config.daemon.listen_port, 9418);
    assert_eq!(config.daemon.listen_port_grpc, 9417);
    assert_eq!(config.daemon.json_socket, "/var/run/perf-sentinel.sock");
    assert_eq!(config.daemon.max_active_traces, 20_000);
    assert_eq!(config.daemon.trace_ttl_ms, 60_000);
    assert!((config.daemon.sampling_rate - 0.5).abs() < f64::EPSILON);
    assert_eq!(config.daemon.max_events_per_trace, 500);
    assert_eq!(config.daemon.max_payload_size, 2_097_152);
}

// 0.6.0 breaking change: the 8 legacy top-level flats deprecated in
// 0.5.26 are now removed. Loading a config that still uses them must
// fail loudly with a migration message rather than silently accept
// the file with the legacy key ignored. Each test below covers one
// (key, replacement) pair so a regression is easy to attribute.

fn assert_legacy_top_level_rejected(toml: &str, key: &str, replacement: &str) {
    let err = load_from_str(toml).expect_err("legacy top-level key must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains(key),
        "error must name the legacy key '{key}': {msg}"
    );
    assert!(
        msg.contains(replacement),
        "error must point at '{replacement}': {msg}"
    );
    assert!(
        msg.contains("0.6.0"),
        "error must tag the breaking-change version: {msg}"
    );
}

#[test]
fn legacy_flat_n_plus_one_threshold_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "n_plus_one_threshold = 7\n",
        "n_plus_one_threshold",
        "[detection] n_plus_one_min_occurrences",
    );
}

#[test]
fn legacy_flat_window_duration_ms_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "window_duration_ms = 1500\n",
        "window_duration_ms",
        "[detection] window_duration_ms",
    );
}

#[test]
fn legacy_flat_listen_addr_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "listen_addr = \"0.0.0.0\"\n",
        "listen_addr",
        "[daemon] listen_address",
    );
}

#[test]
fn legacy_flat_listen_port_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "listen_port = 9418\n",
        "listen_port",
        "[daemon] listen_port_http",
    );
}

#[test]
fn legacy_flat_max_active_traces_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "max_active_traces = 5000\n",
        "max_active_traces",
        "[daemon] max_active_traces",
    );
}

#[test]
fn legacy_flat_trace_ttl_ms_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "trace_ttl_ms = 60000\n",
        "trace_ttl_ms",
        "[daemon] trace_ttl_ms",
    );
}

#[test]
fn legacy_flat_max_events_per_trace_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "max_events_per_trace = 250\n",
        "max_events_per_trace",
        "[daemon] max_events_per_trace",
    );
}

#[test]
fn legacy_flat_max_payload_size_rejected_with_migration_hint() {
    assert_legacy_top_level_rejected(
        "max_payload_size = 1048576\n",
        "max_payload_size",
        "[daemon] max_payload_size",
    );
}

#[test]
fn empty_config_yields_defaults_after_legacy_removal() {
    let config = load_from_str("").unwrap();
    let d = Config::default();
    assert_eq!(
        config.detection.n_plus_one_threshold,
        d.detection.n_plus_one_threshold
    );
    assert_eq!(
        config.detection.window_duration_ms,
        d.detection.window_duration_ms
    );
    assert_eq!(config.daemon.listen_addr, d.daemon.listen_addr);
    assert_eq!(config.daemon.listen_port, d.daemon.listen_port);
    assert_eq!(config.daemon.max_active_traces, d.daemon.max_active_traces);
    assert_eq!(config.daemon.trace_ttl_ms, d.daemon.trace_ttl_ms);
    assert_eq!(
        config.daemon.max_events_per_trace,
        d.daemon.max_events_per_trace
    );
    assert_eq!(config.daemon.max_payload_size, d.daemon.max_payload_size);
}

#[test]
fn parse_sanitizer_aware_classification_modes() {
    use crate::detect::sanitizer_aware::SanitizerAwareMode;

    let default_config = load_from_str("").unwrap();
    assert_eq!(
        default_config.detection.sanitizer_aware_classification,
        SanitizerAwareMode::Auto
    );

    for (value, expected) in [
        ("auto", SanitizerAwareMode::Auto),
        ("always", SanitizerAwareMode::Always),
        ("never", SanitizerAwareMode::Never),
        ("ALWAYS", SanitizerAwareMode::Always),
        ("strict", SanitizerAwareMode::Strict),
        ("STRICT", SanitizerAwareMode::Strict),
    ] {
        let toml = format!("[detection]\nsanitizer_aware_classification = \"{value}\"\n");
        let config = load_from_str(&toml).unwrap();
        assert_eq!(
            config.detection.sanitizer_aware_classification, expected,
            "value: {value}"
        );
    }

    let unknown =
        load_from_str("[detection]\nsanitizer_aware_classification = \"unknown\"\n").unwrap();
    assert_eq!(
        unknown.detection.sanitizer_aware_classification,
        SanitizerAwareMode::Auto,
        "unknown value should fall back to Auto"
    );
}

#[test]
fn parse_windows_style_json_socket_path_in_basic_string() {
    let config = load_from_str(
        r#"
[daemon]
json_socket = "C:\temp\perf-sentinel.sock"
"#,
    )
    .unwrap();
    assert_eq!(config.daemon.json_socket, r"C:\temp\perf-sentinel.sock");
}

#[test]
fn parse_escaped_windows_style_json_socket_path_stays_stable() {
    let config = load_from_str(
        r#"
[daemon]
json_socket = "C:\\temp\\perf-sentinel.sock"
"#,
    )
    .unwrap();
    assert_eq!(config.daemon.json_socket, r"C:\temp\perf-sentinel.sock");
}

#[test]
fn parse_windows_style_json_socket_path_with_trailing_comment() {
    // Covers `find_basic_string_end` stopping before `#`, a common
    // hand-edited config shape the initial test matrix missed.
    let config = load_from_str(
        "[daemon]\n\
         json_socket = \"C:\\temp\\sock\" # inline note\n",
    )
    .unwrap();
    assert_eq!(config.daemon.json_socket, r"C:\temp\sock");
}

#[test]
fn parse_unc_json_socket_path_preserves_double_leading_backslash() {
    // Raw UNC `\\server\share\sock` must round-trip verbatim. The
    // `raw_unc_prefix` branch in `escape_toml_path_backslashes`
    // emits 4 leading `\` so TOML decode yields 2.
    let config = load_from_str(
        r#"
[daemon]
json_socket = "\\server\share\sock"
"#,
    )
    .unwrap();
    assert_eq!(config.daemon.json_socket, r"\\server\share\sock");
}

#[test]
fn parse_pre_escaped_unc_json_socket_path_is_stable() {
    let config = load_from_str(
        r#"
[daemon]
json_socket = "\\\\server\\share\\sock"
"#,
    )
    .unwrap();
    assert_eq!(config.daemon.json_socket, r"\\server\share\sock");
}

#[test]
fn literal_string_windows_path_bypasses_normalization() {
    // TOML literal strings (`'...'`) already treat `\` literally.
    // Our normalizer must not touch them; checked indirectly by
    // confirming the parser accepts a path with lone `\` inside `'`.
    let config = load_from_str(
        r"
[daemon]
json_socket = 'C:\temp\sock'
",
    )
    .unwrap();
    assert_eq!(config.daemon.json_socket, r"C:\temp\sock");
}

#[test]
fn normalization_applies_to_tls_cert_and_key_paths() {
    // TLS paths are validated as filesystem entries, so a non-existent
    // literal yields ConfigError::Validation. The test passes iff the
    // error message surfaces the expected *normalized* path, i.e. our
    // rewriter reached both keys before validation ran.
    let err = load_from_str(
        r#"
[daemon]
tls_cert_path = "C:\certs\server.crt"
tls_key_path = "C:\certs\server.key"
"#,
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains(r"C:\certs\server.crt") || msg.contains(r"C:\certs\server.key"),
        "expected normalized TLS path in error, got: {msg}"
    );
}

#[test]
fn normalization_applies_to_all_registered_path_keys() {
    // Guard against a copy-paste bug in `TOML_PATH_STRING_KEYS`:
    // exercise each key via the unit-level normalizer rather than
    // the full loader (hourly/calibration files would otherwise
    // trigger disk I/O or validation noise).
    for key in TOML_PATH_STRING_KEYS {
        let line = format!("{key} = \"C:\\temp\\x\"\n");
        let rewritten = normalize_toml_path_strings(&line);
        assert!(
            matches!(rewritten, Cow::Owned(_)),
            "{key}: expected normalization to rewrite bare Windows path"
        );
        assert!(
            rewritten.as_ref().contains(r#""C:\\temp\\x""#),
            "{key}: normalized output missing escaped path, got {rewritten}"
        );
    }
}

#[test]
fn normalization_leaves_toml_escape_sequences_literal_in_path_keys() {
    // `\t` and `\n` inside a path key are treated as literal
    // backslash-sequences, not TOML escapes. This is by design for
    // `TOML_PATH_STRING_KEYS` and documented in the helper's rustdoc.
    let config = load_from_str(
        r#"
[daemon]
json_socket = "C:\new\tmp\sock"
"#,
    )
    .unwrap();
    assert_eq!(config.daemon.json_socket, r"C:\new\tmp\sock");
}

#[test]
fn load_from_str_falls_back_when_original_error_is_unrelated_to_path() {
    // Force the normalization branch (Cow::Owned) via a Windows path,
    // then introduce a type mismatch on a strictly-typed key
    // (`listen_port` is `u16`). Both the normalized and the original
    // parse fail; we just assert we surface a ConfigError::Parse
    // rather than silently masking the issue.
    let err = load_from_str(
        r#"
[daemon]
json_socket = "C:\temp\sock"
sampling_rate = "not a number"
"#,
    )
    .unwrap_err();
    assert!(
        matches!(err, ConfigError::Parse(_)),
        "expected ConfigError::Parse, got {err:?}"
    );
}

#[test]
fn find_basic_string_end_handles_escaped_inner_quote() {
    // `"a\"b"`: the first `"` at byte 3 is escaped, real end at byte
    // 5. Guards the linear `run`-counter rewrite against regressions
    // that would terminate too early.
    let value = r#""a\"b""#;
    assert_eq!(find_basic_string_end(value), Some(5));
}

#[test]
fn find_basic_string_end_survives_very_long_backslash_run() {
    // Previously the lookbehind was O(n²); this is a smoke test
    // that a pathological input completes in well under the test
    // timeout. If this regresses to quadratic, it still passes,
    // but the timing would blow up.
    let mut input = String::from("\"");
    input.extend(std::iter::repeat_n('\\', 10_000));
    input.push('"');
    // 10_000 backslashes → 5_000 `\\` pairs → closing `"` valid.
    assert_eq!(find_basic_string_end(&input), Some(10_001));
}

#[test]
fn detection_section_drives_thresholds() {
    let toml = r"
[detection]
n_plus_one_min_occurrences = 12
window_duration_ms = 800
";
    let config = load_from_str(toml).unwrap();
    assert_eq!(config.detection.n_plus_one_threshold, 12);
    assert_eq!(config.detection.window_duration_ms, 800);
}

#[test]
fn new_fields_have_correct_defaults() {
    let config = Config::default();
    assert_eq!(config.thresholds.n_plus_one_sql_critical_max, 0);
    assert_eq!(config.thresholds.n_plus_one_http_warning_max, 3);
    assert!((config.thresholds.io_waste_ratio_max - 0.30).abs() < f64::EPSILON);
    assert!(config.green.enabled);
    assert_eq!(config.daemon.listen_port_grpc, 4317);
    assert_eq!(config.daemon.json_socket, "/tmp/perf-sentinel.sock");
    assert!((config.daemon.sampling_rate - 1.0).abs() < f64::EPSILON);
}

#[test]
fn default_config_validates() {
    let config = Config::default();
    assert!(config.validate().is_ok());
}

#[test]
fn rejects_sampling_rate_above_one() {
    let result = load_from_str("[daemon]\nsampling_rate = 5.0");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("sampling_rate"), "got: {err}");
}

#[test]
fn rejects_negative_sampling_rate() {
    let result = load_from_str("[daemon]\nsampling_rate = -0.1");
    assert!(result.is_err());
}

#[test]
fn rejects_io_waste_ratio_max_above_one() {
    let result = load_from_str("[thresholds]\nio_waste_ratio_max = 1.5");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("io_waste_ratio_max"), "got: {err}");
}

#[test]
fn rejects_zero_max_payload_size() {
    let result = load_from_str("[daemon]\nmax_payload_size = 0");
    assert!(result.is_err());
}

#[test]
fn rejects_zero_n_plus_one_threshold() {
    let result = load_from_str("n_plus_one_threshold = 0");
    assert!(result.is_err());
}

#[test]
fn rejects_zero_max_active_traces() {
    let result = load_from_str("max_active_traces = 0");
    assert!(result.is_err());
}

#[test]
fn rejects_zero_max_events_per_trace() {
    let result = load_from_str("max_events_per_trace = 0");
    assert!(result.is_err());
}

#[test]
fn slow_query_defaults() {
    let config = Config::default();
    assert_eq!(config.detection.slow_query_threshold_ms, 500);
    assert_eq!(config.detection.slow_query_min_occurrences, 3);
    assert!(config.green.default_region.is_none());
    assert!(config.green.service_regions.is_empty());
    assert!(
        (config.green.embodied_carbon_per_request_gco2 - DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn parse_slow_query_config() {
    let toml = r"
[detection]
slow_query_threshold_ms = 1000
slow_query_min_occurrences = 5
";
    let config = load_from_str(toml).unwrap();
    assert_eq!(config.detection.slow_query_threshold_ms, 1000);
    assert_eq!(config.detection.slow_query_min_occurrences, 5);
}

#[test]
fn parse_green_default_region() {
    let toml = r#"
[green]
enabled = true
default_region = "eu-west-3"
"#;
    let config = load_from_str(toml).unwrap();
    assert_eq!(config.green.default_region.as_deref(), Some("eu-west-3"));
}

#[test]
fn parse_green_service_regions() {
    let toml = r#"
[green]
enabled = true
default_region = "eu-west-3"

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc" = "ap-southeast-1"
"#;
    let config = load_from_str(toml).unwrap();
    assert_eq!(config.green.service_regions.len(), 2);
    assert_eq!(
        config
            .green
            .service_regions
            .get("order-svc")
            .map(String::as_str),
        Some("us-east-1")
    );
    assert_eq!(
        config
            .green
            .service_regions
            .get("chat-svc")
            .map(String::as_str),
        Some("ap-southeast-1")
    );
}

#[test]
fn parse_green_embodied_carbon_override() {
    let toml = r"
[green]
enabled = true
embodied_carbon_per_request_gco2 = 0.005
";
    let config = load_from_str(toml).unwrap();
    assert!((config.green.embodied_carbon_per_request_gco2 - 0.005).abs() < f64::EPSILON);
}

#[test]
fn rejects_negative_embodied_carbon() {
    let result = load_from_str("[green]\nembodied_carbon_per_request_gco2 = -0.001");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("embodied_carbon_per_request_gco2"),
        "got: {err}"
    );
}

#[test]
fn accepts_zero_embodied_carbon() {
    let toml = r"
[green]
embodied_carbon_per_request_gco2 = 0.0
";
    let config = load_from_str(toml).unwrap();
    assert!((config.green.embodied_carbon_per_request_gco2 - 0.0).abs() < f64::EPSILON);
}

#[test]
fn empty_service_regions_default() {
    let toml = r#"
[green]
default_region = "eu-west-3"
"#;
    let config = load_from_str(toml).unwrap();
    assert!(config.green.service_regions.is_empty());
}

// ----- Region validation + lowercase + both-set -----

#[test]
fn rejects_invalid_default_region_characters() {
    // Space in region name: log-injection protection at config load.
    let result = load_from_str("[green]\ndefault_region = \"eu west 3\"");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("default_region"),
        "error should mention default_region, got: {err}"
    );
}

#[test]
fn rejects_oversized_default_region() {
    // 65 chars, just over the 64-char cap.
    let long_region = "a".repeat(65);
    let toml = format!("[green]\ndefault_region = \"{long_region}\"");
    let result = load_from_str(&toml);
    assert!(result.is_err());
}

#[test]
fn rejects_default_region_with_newline_escape() {
    // In a TOML basic string, `\n` is an escape sequence for a real
    // newline byte. The validator must reject the resulting control
    // char to block log-forging via default_region.
    let result = load_from_str("[green]\ndefault_region = \"eu-west-3\\n\"");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("default_region"),
        "error should mention default_region"
    );
}

#[test]
fn rejects_default_region_with_literal_newline() {
    // Multi-line basic string with an actual newline byte in the
    // value. Also rejected at load time.
    let result = load_from_str("[green]\ndefault_region = \"\"\"eu-west-3\n\"\"\"");
    assert!(result.is_err());
}

#[test]
fn accepts_known_regions() {
    // Sanity: all known region names pass the validator.
    for region in ["eu-west-3", "us-east-1", "fr", "mars-1", "unknown"] {
        let toml = format!("[green]\ndefault_region = \"{region}\"");
        let config = load_from_str(&toml)
            .unwrap_or_else(|e| panic!("region '{region}' should be accepted, got error: {e}"));
        assert_eq!(config.green.default_region.as_deref(), Some(region));
    }
}

#[test]
fn rejects_invalid_service_regions_service_name() {
    let toml = r#"
[green.service_regions]
"bad service" = "us-east-1"
"#;
    let result = load_from_str(toml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("service_regions"),
        "error should mention service_regions, got: {err}"
    );
}

#[test]
fn rejects_invalid_service_regions_region_value() {
    let toml = r#"
[green.service_regions]
"order-svc" = "us east 1"
"#;
    let result = load_from_str(toml);
    assert!(result.is_err());
}

#[test]
fn rejects_oversized_service_regions_map() {
    // Fat-finger or malicious config with too many entries gets
    // rejected at load time with a clear error mentioning the cap.
    use std::fmt::Write as _;
    let mut toml = String::from("[green.service_regions]\n");
    for i in 0..1025 {
        let _ = writeln!(toml, "\"svc-{i:04}\" = \"eu-west-3\"");
    }
    let result = load_from_str(&toml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("service_regions") && err.contains("1025"),
        "error should mention service_regions and the count, got: {err}"
    );
}

#[test]
fn accepts_service_regions_at_exactly_the_cap() {
    // Boundary check: exactly 1024 entries should pass.
    use std::fmt::Write as _;
    let mut toml = String::from("[green.service_regions]\n");
    for i in 0..1024 {
        let _ = writeln!(toml, "\"svc-{i:04}\" = \"eu-west-3\"");
    }
    let config = load_from_str(&toml).expect("1024 entries should be accepted");
    assert_eq!(config.green.service_regions.len(), 1024);
}

#[test]
fn service_regions_keys_are_lowercased_on_load() {
    // Config loader lowercases keys so resolve_region's
    // case-insensitive lookup works transparently.
    let toml = r#"
[green.service_regions]
"Order-Svc" = "us-east-1"
"CHAT-SVC" = "ap-southeast-1"
"#;
    let config = load_from_str(toml).unwrap();
    assert_eq!(config.green.service_regions.len(), 2);
    // Keys are lowercased regardless of TOML casing.
    assert_eq!(
        config
            .green
            .service_regions
            .get("order-svc")
            .map(String::as_str),
        Some("us-east-1")
    );
    assert_eq!(
        config
            .green
            .service_regions
            .get("chat-svc")
            .map(String::as_str),
        Some("ap-southeast-1")
    );
    // The original casings should NOT be present.
    assert!(!config.green.service_regions.contains_key("Order-Svc"));
}

#[test]
fn rejects_zero_slow_query_threshold() {
    let result = load_from_str("[detection]\nslow_query_threshold_ms = 0");
    assert!(result.is_err());
}

#[test]
fn rejects_zero_slow_query_min_occurrences() {
    let result = load_from_str("[detection]\nslow_query_min_occurrences = 0");
    assert!(result.is_err());
}

#[test]
fn rejects_zero_max_fanout() {
    let result = load_from_str("[detection]\nmax_fanout = 0");
    assert!(result.is_err());
}

#[test]
fn rejects_max_fanout_over_100k() {
    let result = load_from_str("[detection]\nmax_fanout = 100001");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("max_fanout"), "got: {err}");
}

#[test]
fn accepts_max_fanout_at_100k() {
    let result = load_from_str("[detection]\nmax_fanout = 100000");
    assert!(result.is_ok());
}

#[test]
fn rejects_max_payload_size_over_100mb() {
    let result = load_from_str("[daemon]\nmax_payload_size = 104857601");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("max_payload_size"), "got: {err}");
}

#[test]
fn accepts_max_payload_size_at_100mb() {
    let result = load_from_str("[daemon]\nmax_payload_size = 104857600");
    assert!(result.is_ok());
}

#[test]
fn rejects_max_active_traces_over_1m() {
    let result = load_from_str("[daemon]\nmax_active_traces = 1000001");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("max_active_traces"), "got: {err}");
}

#[test]
fn accepts_max_active_traces_at_1m() {
    let result = load_from_str("[daemon]\nmax_active_traces = 1000000");
    assert!(result.is_ok());
}

#[test]
fn rejects_max_events_per_trace_over_100k() {
    let result = load_from_str("[daemon]\nmax_events_per_trace = 100001");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("max_events_per_trace"), "got: {err}");
}

#[test]
fn accepts_max_events_per_trace_at_100k() {
    let result = load_from_str("[daemon]\nmax_events_per_trace = 100000");
    assert!(result.is_ok());
}

// --- comfort-zone warnings (parse OK, hard caps unchanged) ---

#[test]
fn config_defaults_sit_inside_every_comfort_zone() {
    // Locks the invariant that the canonical defaults never trigger
    // a startup warning. If a default is moved, this test forces an
    // explicit re-check of the matching comfort band.
    let cfg = Config::default();
    assert!(
        (256 * 1024..=16 * 1024 * 1024).contains(&cfg.daemon.max_payload_size),
        "default max_payload_size {} is outside its comfort zone",
        cfg.daemon.max_payload_size
    );
    assert!(
        (1_000..=100_000).contains(&cfg.daemon.max_active_traces),
        "default max_active_traces {} is outside its comfort zone",
        cfg.daemon.max_active_traces
    );
    assert!(
        (100..=10_000).contains(&cfg.daemon.max_events_per_trace),
        "default max_events_per_trace {} is outside its comfort zone",
        cfg.daemon.max_events_per_trace
    );
    assert!(
        (100..=100_000).contains(&cfg.daemon.max_retained_findings),
        "default max_retained_findings {} is outside its comfort zone",
        cfg.daemon.max_retained_findings
    );
    assert!(
        (1_000..=600_000).contains(&cfg.daemon.trace_ttl_ms),
        "default trace_ttl_ms {} is outside its comfort zone",
        cfg.daemon.trace_ttl_ms
    );
    assert!(
        (5..=1_000).contains(&cfg.detection.max_fanout),
        "default max_fanout {} is outside its comfort zone",
        cfg.detection.max_fanout
    );
}

#[test]
fn accepts_max_active_traces_below_comfort_floor_with_warning() {
    // 500 < comfort floor (1_000) but well within hard floor (1).
    let result = load_from_str("[daemon]\nmax_active_traces = 500");
    assert!(result.is_ok(), "expected parse OK, got {result:?}");
}

#[test]
fn accepts_max_active_traces_above_comfort_ceiling_with_warning() {
    // 500_000 > comfort ceiling (100_000) but within hard ceiling (1_000_000).
    let result = load_from_str("[daemon]\nmax_active_traces = 500000");
    assert!(result.is_ok(), "expected parse OK, got {result:?}");
}

#[test]
fn accepts_max_events_per_trace_outside_comfort_zone() {
    // 10 < comfort floor (100); 50_000 > comfort ceiling (10_000).
    // Both inside hard bounds [1, 100_000].
    for value in [10, 50_000] {
        let result = load_from_str(&format!("[daemon]\nmax_events_per_trace = {value}\n"));
        assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
    }
}

#[test]
fn accepts_trace_ttl_outside_comfort_but_inside_hard_bounds() {
    // 200ms < comfort floor (1s); 1_800_000 (30min) > comfort ceiling (10min).
    for value in [200_u64, 1_800_000_u64] {
        let result = load_from_str(&format!("[daemon]\ntrace_ttl_ms = {value}\n"));
        assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
    }
}

#[test]
fn accepts_max_fanout_outside_comfort_but_inside_hard_bounds() {
    // 2 < comfort floor (5); 5_000 > comfort ceiling (1_000).
    for value in [2, 5_000] {
        let result = load_from_str(&format!("[detection]\nmax_fanout = {value}\n"));
        assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
    }
}

#[test]
fn accepts_max_payload_size_outside_comfort_but_inside_hard_bounds() {
    // 64 KiB < comfort floor (256 KiB); 32 MiB > comfort ceiling (16 MiB).
    for value in [64 * 1024_u64, 32 * 1024 * 1024_u64] {
        let result = load_from_str(&format!("[daemon]\nmax_payload_size = {value}\n"));
        assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
    }
}

// --- max_retained_findings hard cap (was unbounded before) ---

#[test]
fn accepts_zero_max_retained_findings_disables_store() {
    // `0` is a documented way to disable the findings store and
    // reclaim its memory. It must keep parsing.
    let result = load_from_str("[daemon]\nmax_retained_findings = 0");
    assert!(result.is_ok(), "expected 0 to parse, got {result:?}");
}

#[test]
fn rejects_max_retained_findings_above_10m() {
    let result = load_from_str("[daemon]\nmax_retained_findings = 10000001");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("max_retained_findings"), "got: {err}");
}

#[test]
fn accepts_max_retained_findings_at_10m_hard_ceiling() {
    let result = load_from_str("[daemon]\nmax_retained_findings = 10000000");
    assert!(result.is_ok());
}

#[test]
fn accepts_max_retained_findings_outside_comfort_but_inside_hard_bounds() {
    // 50 < comfort floor (100); 500_000 > comfort ceiling (100_000).
    for value in [50, 500_000] {
        let result = load_from_str(&format!("[daemon]\nmax_retained_findings = {value}\n"));
        assert!(result.is_ok(), "expected {value} to parse, got {result:?}");
    }
}

#[test]
fn rejects_trace_ttl_below_100() {
    let result = load_from_str("[daemon]\ntrace_ttl_ms = 50");
    assert!(result.is_err());
}

#[test]
fn rejects_zero_window_duration() {
    let result = load_from_str("[detection]\nwindow_duration_ms = 0");
    assert!(result.is_err());
}

#[test]
fn green_disabled_parses() {
    let config = load_from_str("[green]\nenabled = false").unwrap();
    assert!(!config.green.enabled);
}

// -- Port validation --

#[test]
fn rejects_port_zero() {
    let result = load_from_str("[daemon]\nlisten_port_http = 0");
    assert!(result.is_err());
}

#[test]
fn accepts_port_one() {
    let config = load_from_str("[daemon]\nlisten_port_http = 1").unwrap();
    assert_eq!(config.daemon.listen_port, 1);
}

#[test]
fn accepts_port_65535() {
    let config = load_from_str("[daemon]\nlisten_port_http = 65535").unwrap();
    assert_eq!(config.daemon.listen_port, 65535);
}

#[test]
fn rejects_grpc_port_zero() {
    let result = load_from_str("[daemon]\nlisten_port_grpc = 0");
    assert!(result.is_err());
}

// -- trace_ttl_ms upper bound --

#[test]
fn rejects_trace_ttl_above_1h() {
    let result = load_from_str("[daemon]\ntrace_ttl_ms = 3600001");
    assert!(result.is_err());
}

#[test]
fn accepts_trace_ttl_at_1h() {
    let config = load_from_str("[daemon]\ntrace_ttl_ms = 3600000").unwrap();
    assert_eq!(config.daemon.trace_ttl_ms, 3_600_000);
}

#[test]
fn accepts_trace_ttl_at_100ms() {
    let config = load_from_str("[daemon]\ntrace_ttl_ms = 100").unwrap();
    assert_eq!(config.daemon.trace_ttl_ms, 100);
}

// -- Sampling rate edge cases --

#[test]
fn accepts_sampling_rate_zero() {
    let config = load_from_str("[daemon]\nsampling_rate = 0.0").unwrap();
    assert!((config.daemon.sampling_rate - 0.0).abs() < f64::EPSILON);
}

#[test]
fn accepts_sampling_rate_one() {
    let config = load_from_str("[daemon]\nsampling_rate = 1.0").unwrap();
    assert!((config.daemon.sampling_rate - 1.0).abs() < f64::EPSILON);
}

// --- [daemon] environment parsing ---

#[test]
fn daemon_environment_defaults_to_staging() {
    let config = Config::default();
    assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
    assert_eq!(config.confidence(), Confidence::DaemonStaging);
}

#[test]
fn daemon_environment_omitted_uses_default() {
    let config = load_from_str("[daemon]\nmax_active_traces = 100").unwrap();
    assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
}

#[test]
fn daemon_environment_staging() {
    let config = load_from_str("[daemon]\nenvironment = \"staging\"").unwrap();
    assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
    assert_eq!(config.confidence(), Confidence::DaemonStaging);
}

#[test]
fn daemon_environment_production() {
    let config = load_from_str("[daemon]\nenvironment = \"production\"").unwrap();
    assert_eq!(config.daemon.environment, DaemonEnvironment::Production);
    assert_eq!(config.confidence(), Confidence::DaemonProduction);
}

#[test]
fn daemon_environment_case_insensitive() {
    let config = load_from_str("[daemon]\nenvironment = \"PRODUCTION\"").unwrap();
    assert_eq!(config.daemon.environment, DaemonEnvironment::Production);
    let config = load_from_str("[daemon]\nenvironment = \"Staging\"").unwrap();
    assert_eq!(config.daemon.environment, DaemonEnvironment::Staging);
}

#[test]
fn daemon_environment_rejects_unknown() {
    let result = load_from_str("[daemon]\nenvironment = \"prod\"");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("environment"), "got: {err}");
    assert!(err.contains("staging"), "error should mention valid values");
    assert!(
        err.contains("production"),
        "error should mention valid values"
    );
}

#[test]
fn daemon_environment_rejects_empty() {
    let result = load_from_str("[daemon]\nenvironment = \"\"");
    assert!(result.is_err());
}

#[test]
fn daemon_environment_rejects_dev() {
    let result = load_from_str("[daemon]\nenvironment = \"dev\"");
    assert!(result.is_err());
}

#[test]
fn daemon_environment_as_str() {
    assert_eq!(DaemonEnvironment::Staging.as_str(), "staging");
    assert_eq!(DaemonEnvironment::Production.as_str(), "production");
}

// --- [green] use_hourly_profiles ---

#[test]
fn green_use_hourly_profiles_defaults_to_true() {
    let config = Config::default();
    assert!(config.green.use_hourly_profiles);
}

#[test]
fn green_use_hourly_profiles_omitted_uses_default() {
    let config = load_from_str("[green]\nenabled = true\n").unwrap();
    assert!(config.green.use_hourly_profiles);
}

#[test]
fn green_use_hourly_profiles_explicit_false() {
    let config = load_from_str("[green]\nuse_hourly_profiles = false\n").unwrap();
    assert!(!config.green.use_hourly_profiles);
}

#[test]
fn green_use_hourly_profiles_explicit_true() {
    let config = load_from_str("[green]\nuse_hourly_profiles = true\n").unwrap();
    assert!(config.green.use_hourly_profiles);
}

// --- [green] hourly_profiles_file ---

#[test]
fn hourly_profiles_file_absent_by_default() {
    let config = Config::default();
    assert!(config.green.hourly_profiles_file.is_none());
    assert!(config.green.custom_hourly_profiles.is_none());
}

#[test]
fn hourly_profiles_file_control_chars_rejected() {
    let config = load_from_str("[green]\nhourly_profiles_file = \"/tmp/profiles\\n.json\"\n");
    // The control character check happens during loading (sets None)
    // and then validate_green rejects the config.
    if let Ok(c) = config {
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("control characters") || err.contains("failed to load"),
            "expected control char or load failure error, got: {err}"
        );
    } else {
        // TOML parse error is also acceptable
    }
}

#[test]
fn hourly_profiles_file_nonexistent_path_rejected() {
    let result = load_from_str("[green]\nhourly_profiles_file = \"/nonexistent/profiles.json\"\n");
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("failed to load"),
        "expected load failure error, got: {err}"
    );
}

#[test]
fn hourly_profiles_windows_path_reports_load_failure_not_parse_error() {
    let err = load_from_str(
        r#"
[green]
hourly_profiles_file = "C:\temp\profiles.json"
"#,
    )
    .unwrap_err();
    assert!(
        format!("{err}").contains("failed to load"),
        "expected load failure error, got: {err}"
    );
}

// --- [green.scaphandre] parsing ---

#[test]
fn scaphandre_absent_by_default() {
    let config = Config::default();
    assert!(config.green.scaphandre.is_none());
}

#[test]
fn scaphandre_empty_section_parses_to_none() {
    // An empty [green.scaphandre] table (no endpoint) is treated
    // as "Scaphandre not configured": the scraper is not spawned.
    let config = load_from_str("[green.scaphandre]\n").unwrap();
    assert!(config.green.scaphandre.is_none());
}

#[test]
fn scaphandre_endpoint_only() {
    let config =
        load_from_str("[green.scaphandre]\nendpoint = \"http://localhost:8080/metrics\"\n")
            .unwrap();
    let cfg = config.green.scaphandre.unwrap();
    assert_eq!(cfg.endpoint, "http://localhost:8080/metrics");
    // Default interval is 5 s.
    assert_eq!(cfg.scrape_interval.as_secs(), 5);
    assert!(cfg.process_map.is_empty());
}

#[test]
fn scaphandre_full_config() {
    let toml = r#"
[green.scaphandre]
endpoint = "http://localhost:9090/metrics"
scrape_interval_secs = 10

[green.scaphandre.process_map."order-svc"]
exe_contains = "bin/java"
cmdline_contains = "order-svc.jar"

[green.scaphandre.process_map."chat-svc"]
exe_contains = "bin/dotnet"
cmdline_contains = "chat-svc.dll"
"#;
    let config = load_from_str(toml).unwrap();
    let cfg = config.green.scaphandre.unwrap();
    assert_eq!(cfg.endpoint, "http://localhost:9090/metrics");
    assert_eq!(cfg.scrape_interval.as_secs(), 10);
    let order = cfg.process_map.get("order-svc").unwrap();
    assert_eq!(order.exe_contains, "bin/java");
    assert_eq!(order.cmdline_contains.as_deref(), Some("order-svc.jar"));
    let chat = cfg.process_map.get("chat-svc").unwrap();
    assert_eq!(chat.exe_contains, "bin/dotnet");
    assert_eq!(chat.cmdline_contains.as_deref(), Some("chat-svc.dll"));
}

#[test]
fn scaphandre_rejects_unknown_field_in_process_matcher() {
    // A typo like `cmdline_containss` (double s) on a matcher would
    // silently fall through to `None` without `deny_unknown_fields`,
    // and the matcher would over-attribute power to any process
    // sharing the runtime. Verify serde rejects the typo at load.
    let toml = r#"
[green.scaphandre]
endpoint = "http://localhost/metrics"

[green.scaphandre.process_map."order-svc"]
exe_contains = "bin/java"
cmdline_containss = "order-svc.jar"
"#;
    let result = load_from_str(toml);
    assert!(result.is_err(), "typo in matcher field must be rejected");
}

#[test]
fn scaphandre_rejects_legacy_string_form() {
    // Pre-0.7.6 form was `"order-svc" = "java"`. The new form
    // requires a table, so serde must reject the old string form.
    let toml = r#"
[green.scaphandre]
endpoint = "http://localhost/metrics"

[green.scaphandre.process_map]
"order-svc" = "java"
"#;
    let result = load_from_str(toml);
    assert!(result.is_err(), "legacy string form must be rejected");
}

#[test]
fn scaphandre_accepts_exe_only_matcher() {
    // cmdline_contains is optional, for processes whose exe is
    // already unique (a native binary, a renamed runtime, etc.).
    let toml = r#"
[green.scaphandre]
endpoint = "http://localhost/metrics"

[green.scaphandre.process_map."native-svc"]
exe_contains = "/opt/native/bin/svc"
"#;
    let config = load_from_str(toml).unwrap();
    let cfg = config.green.scaphandre.unwrap();
    let native = cfg.process_map.get("native-svc").unwrap();
    assert_eq!(native.exe_contains, "/opt/native/bin/svc");
    assert!(native.cmdline_contains.is_none());
}

#[test]
fn scaphandre_accepts_https_endpoint() {
    let result = load_from_str("[green.scaphandre]\nendpoint = \"https://secure:8080/metrics\"\n");
    assert!(result.is_ok(), "HTTPS endpoints should be accepted");
}

#[test]
fn scaphandre_rejects_zero_interval() {
    let result = load_from_str(
        "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 0\n",
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("scrape_interval_secs"), "got: {err}");
}

#[test]
fn scaphandre_rejects_huge_interval() {
    let result = load_from_str(
        "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 99999\n",
    );
    assert!(result.is_err());
}

#[test]
fn scaphandre_rejects_empty_exe_in_process_map() {
    let toml = r#"
[green.scaphandre]
endpoint = "http://localhost/metrics"

[green.scaphandre.process_map."order-svc"]
exe_contains = ""
"#;
    let result = load_from_str(toml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("process_map"), "got: {err}");
}

#[test]
fn scaphandre_accepts_interval_at_boundary_1s() {
    let config = load_from_str(
        "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 1\n",
    )
    .unwrap();
    assert_eq!(
        config
            .green
            .scaphandre
            .as_ref()
            .unwrap()
            .scrape_interval
            .as_secs(),
        1
    );
}

#[test]
fn scaphandre_accepts_interval_at_boundary_3600s() {
    let config = load_from_str(
        "[green.scaphandre]\nendpoint = \"http://localhost/metrics\"\nscrape_interval_secs = 3600\n",
    )
    .unwrap();
    assert_eq!(
        config
            .green
            .scaphandre
            .as_ref()
            .unwrap()
            .scrape_interval
            .as_secs(),
        3600
    );
}

// ------------------------------------------------------------------
// [green.cloud] config tests
// ------------------------------------------------------------------

#[test]
fn cloud_section_absent_yields_none() {
    let toml = "[green]\nenabled = true\n";
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    assert!(cfg.green.cloud_energy.is_none());
}

#[test]
fn cloud_section_endpoint_only_parses_with_defaults() {
    let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
"#;
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    let cloud = cfg.green.cloud_energy.unwrap();
    assert_eq!(cloud.prometheus_endpoint, "http://prom:9090");
    assert_eq!(cloud.scrape_interval.as_secs(), 15);
    assert!(cloud.default_provider.is_none());
    assert!(cloud.services.is_empty());
}

#[test]
fn cloud_section_full_config_with_both_service_types() {
    let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
scrape_interval_secs = 30
default_provider = "aws"

[green.cloud.services.svc-a]
provider = "gcp"
instance_type = "n2-standard-8"

[green.cloud.services.svc-b]
idle_watts = 45
max_watts = 120
"#;
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    assert!(cfg.validate().is_ok());
    let cloud = cfg.green.cloud_energy.as_ref().unwrap();
    assert_eq!(cloud.scrape_interval.as_secs(), 30);
    assert_eq!(cloud.default_provider.as_deref(), Some("aws"));
    assert_eq!(cloud.services.len(), 2);
}

#[test]
fn cloud_accepts_https_endpoint() {
    let toml = r#"
[green.cloud]
prometheus_endpoint = "https://prom:9090"
"#;
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    assert!(cfg.validate().is_ok(), "HTTPS endpoints should be accepted");
}

#[test]
fn cloud_rejects_credentials_in_endpoint() {
    let toml = r#"
[green.cloud]
prometheus_endpoint = "http://user:pass@prom:9090"
"#;
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("credentials"), "error: {err}");
}

#[test]
fn cloud_rejects_invalid_scrape_interval() {
    let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
scrape_interval_secs = 0
"#;
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("scrape_interval"), "error: {err}");
}

#[test]
fn cloud_rejects_invalid_provider() {
    let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"
default_provider = "alibaba"
"#;
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("default_provider"), "error: {err}");
}

#[test]
fn cloud_rejects_max_watts_less_than_idle() {
    let toml = r#"
[green.cloud]
prometheus_endpoint = "http://prom:9090"

[green.cloud.services.bad-svc]
idle_watts = 100
max_watts = 50
"#;
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("max_watts"), "error: {err}");
}

#[test]
fn cloud_rejects_service_name_with_control_chars() {
    let toml = "
[green.cloud]
prometheus_endpoint = \"http://prom:9090\"

[green.cloud.services.\"bad\\nsvc\"]
idle_watts = 10
max_watts = 50
";
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("control characters"), "error: {err}");
}

#[test]
fn config_per_operation_coefficients_default_true() {
    let cfg = Config::default();
    assert!(cfg.green.per_operation_coefficients);
}

#[test]
fn config_include_network_transport_default_false() {
    let cfg = Config::default();
    assert!(!cfg.green.include_network_transport);
}

#[test]
fn config_network_energy_per_byte_kwh_default() {
    let cfg = Config::default();
    assert!(
        (cfg.green.network_energy_per_byte_kwh
            - crate::score::carbon::DEFAULT_NETWORK_ENERGY_PER_BYTE_KWH)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn config_network_energy_per_byte_kwh_rejects_negative() {
    let toml = r"
[green]
network_energy_per_byte_kwh = -0.001
";
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("network_energy_per_byte_kwh"), "error: {err}");
}

#[test]
fn config_network_energy_per_byte_kwh_rejects_nan() {
    let cfg = Config {
        green: GreenConfig {
            network_energy_per_byte_kwh: f64::NAN,
            ..GreenConfig::default()
        },
        ..Config::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("network_energy_per_byte_kwh"), "error: {err}");
}

#[test]
fn config_per_operation_coefficients_from_toml() {
    let toml = r"
[green]
per_operation_coefficients = false
";
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    assert!(!cfg.green.per_operation_coefficients);
}

#[test]
fn config_include_network_transport_from_toml() {
    let toml = r"
[green]
include_network_transport = true
network_energy_per_byte_kwh = 0.00000000008
";
    let cfg: Config = toml::from_str::<RawConfig>(toml).unwrap().into();
    assert!(cfg.green.include_network_transport);
    assert!((cfg.green.network_energy_per_byte_kwh - 0.000_000_000_08).abs() < f64::EPSILON);
}

// --- validate_http_authority error paths ---

#[test]
fn validate_http_authority_rejects_empty_host() {
    assert!(validate_http_authority("http://", "test").is_err());
}

#[test]
fn validate_http_authority_rejects_credentials() {
    let err = validate_http_authority("http://user:pass@host/", "test").unwrap_err();
    assert!(err.contains("credentials"));
}

#[test]
fn validate_http_authority_rejects_control_char() {
    // Embed a tab (0x09) in the host.
    let err = validate_http_authority("http://bad\thost/", "test").unwrap_err();
    assert!(err.contains("control"));
}

#[test]
fn has_control_char_rejects_c1_range() {
    // U+0080..=U+009F survive a byte-level `< 0x20` filter because they
    // encode as `0xC2 0x80..0x9F` in UTF-8. They carry CSI/ST/OSC in
    // VT-family terminals, so any TOML field that ends up in stderr
    // must reject them at load time.
    assert!(has_control_char("path\u{009b}[31m"));
    assert!(has_control_char("path\u{009c}suffix"));
    assert!(has_control_char("path\u{009d}OSC"));
    // Boundary checks: U+0080 and U+009F are the inclusive endpoints.
    assert!(has_control_char("\u{0080}"));
    assert!(has_control_char("\u{009f}"));
    // U+00A0 (NBSP) sits one past the C1 range and stays allowed.
    assert!(!has_control_char("\u{00a0}plain"));
}

#[test]
fn validate_http_authority_rejects_invalid_ipv4_port() {
    let err = validate_http_authority("http://host:abc/", "test").unwrap_err();
    assert!(err.contains("port"));
}

#[test]
fn validate_http_authority_accepts_bare_ipv6() {
    // `[::1]` without port: should not error on the port-parse branch.
    assert!(validate_http_authority("http://[::1]/metrics", "test").is_ok());
}

#[test]
fn validate_http_authority_accepts_ipv6_with_port() {
    assert!(validate_http_authority("http://[::1]:8080/metrics", "test").is_ok());
}

#[test]
fn validate_http_authority_rejects_ipv6_with_invalid_port() {
    let err = validate_http_authority("http://[::1]:abc/metrics", "test").unwrap_err();
    assert!(err.contains("port"));
}

#[test]
fn validate_http_authority_accepts_https_scheme() {
    assert!(validate_http_authority("https://host:443/", "test").is_ok());
}

// --- validate_green error paths ---

#[test]
fn validate_green_rejects_nonfinite_embodied_carbon() {
    let toml = "[green]\nembodied_carbon_per_request_gco2 = nan\n";
    let err = load_from_str(toml).unwrap_err();
    assert!(format!("{err:?}").contains("finite"));
}

#[test]
fn validate_green_rejects_hourly_profiles_file_that_fails_to_load() {
    let toml = r#"
[green]
hourly_profiles_file = "/tmp/does-not-exist-perfsentinel-test.json"
"#;
    let err = load_from_str(toml).unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("hourly_profiles_file") || msg.contains("failed to load"));
}

// --- convert_electricity_maps_section branches ---

use crate::score::electricity_maps::config::{EmissionFactorType, TemporalGranularity};
// Local imports used by all the electricity_maps tests below.
// `HashMap` and `Duration` are already in scope via `use super::*;`
// at the top of this module, but Qodana flags the fully-qualified
// forms as unnecessary; using the short names reads cleaner anyway.
use crate::score::electricity_maps::ElectricityMapsConfig;

#[test]
fn electricity_maps_empty_api_key_returns_none() {
    // When the api_key is explicitly an empty string and no env var is
    // set, the conversion returns None (subsystem stays inactive).
    let raw = ElectricityMapsSection {
        api_key: Some(String::new()),
        endpoint: None,
        poll_interval_secs: None,
        region_map: HashMap::new(),
        emission_factor_type: None,
        temporal_granularity: None,
    };
    // Pass a stubbed env-lookup that returns None so the test is
    // independent of the ambient process environment (no `unsafe`
    // env mutation, no races with other tests in the same binary).
    assert!(convert_electricity_maps_section_with_env(&raw, || None).is_none());
}

#[test]
fn electricity_maps_warn_when_api_key_in_config_file() {
    // `api_key` set, env var unset → returns Some(...) but emits a
    // warning about preferring the env var. The warning path is
    // exercised; we just verify the conversion succeeds.
    let mut region_map = HashMap::new();
    region_map.insert("eu-west-3".to_string(), "FR".to_string());
    let raw = ElectricityMapsSection {
        api_key: Some("file-token".to_string()),
        endpoint: None,
        poll_interval_secs: Some(600),
        region_map,
        emission_factor_type: None,
        temporal_granularity: None,
    };
    let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
    assert_eq!(cfg.auth_token, "file-token");
    assert_eq!(cfg.poll_interval, Duration::from_mins(10));
    // default endpoint fallback
    assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v4");
    // region key was lowercased (it was already lowercase, so idempotent)
    assert!(cfg.region_map.contains_key("eu-west-3"));
}

#[test]
fn electricity_maps_legacy_v3_endpoint_loads_cleanly() {
    // Backward-compat guard: an explicit v3 endpoint in the TOML
    // must still produce a valid ElectricityMapsConfig. The
    // deprecation warning is emitted at scraper startup (covered
    // by the `is_legacy_v3_endpoint` unit tests), the conversion
    // path here just keeps the field as-is.
    let mut region_map = HashMap::new();
    region_map.insert("eu-west-3".to_string(), "FR".to_string());
    let raw = ElectricityMapsSection {
        api_key: Some("tok".to_string()),
        endpoint: Some("https://api.electricitymaps.com/v3".to_string()),
        poll_interval_secs: Some(300),
        region_map,
        emission_factor_type: None,
        temporal_granularity: None,
    };
    let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
    assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v3");
}

#[test]
fn electricity_maps_endpoint_trailing_slash_is_normalized() {
    // A copy-paste with trailing slash must not produce a
    // double-slash URL when we later format
    // `{api_endpoint}/carbon-intensity/latest`. The trim happens at
    // config-load so the canonical form propagates everywhere
    // (state, logs, error messages).
    let mut region_map = HashMap::new();
    region_map.insert("eu-west-3".to_string(), "FR".to_string());
    let raw = ElectricityMapsSection {
        api_key: Some("tok".to_string()),
        endpoint: Some("https://api.electricitymaps.com/v4/".to_string()),
        poll_interval_secs: Some(300),
        region_map,
        emission_factor_type: None,
        temporal_granularity: None,
    };
    let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
    assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v4");
}

#[test]
fn electricity_maps_endpoint_strips_multiple_trailing_slashes() {
    let mut region_map = HashMap::new();
    region_map.insert("eu-west-3".to_string(), "FR".to_string());
    let raw = ElectricityMapsSection {
        api_key: Some("tok".to_string()),
        endpoint: Some("https://api.electricitymaps.com/v4///".to_string()),
        poll_interval_secs: Some(300),
        region_map,
        emission_factor_type: None,
        temporal_granularity: None,
    };
    let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
    assert_eq!(cfg.api_endpoint, "https://api.electricitymaps.com/v4");
}

#[test]
fn electricity_maps_region_map_keys_lowercased() {
    let mut region_map = HashMap::new();
    region_map.insert("EU-WEST-3".to_string(), "FR".to_string());
    region_map.insert("Us-East-1".to_string(), "US-MIDA-PJM".to_string());
    let raw = ElectricityMapsSection {
        api_key: Some("tok".to_string()),
        endpoint: Some("https://custom.api/v3".to_string()),
        poll_interval_secs: Some(120),
        region_map,
        emission_factor_type: None,
        temporal_granularity: None,
    };
    let cfg = convert_electricity_maps_section_with_env(&raw, || None).expect("should convert");
    assert!(cfg.region_map.contains_key("eu-west-3"));
    assert!(cfg.region_map.contains_key("us-east-1"));
    assert_eq!(cfg.api_endpoint, "https://custom.api/v3");
}

#[test]
fn electricity_maps_env_var_takes_precedence_over_config_file() {
    // Env-lookup returns a token → it wins over `api_key` in the file.
    // Covers the from_env branch of convert_electricity_maps_section_with_env
    // without touching the real process environment.
    let mut region_map = HashMap::new();
    region_map.insert("eu-west-3".to_string(), "FR".to_string());
    let raw = ElectricityMapsSection {
        api_key: Some("from-file".to_string()),
        endpoint: None,
        poll_interval_secs: None,
        region_map,
        emission_factor_type: None,
        temporal_granularity: None,
    };
    let cfg = convert_electricity_maps_section_with_env(&raw, || Some("from-env".to_string()))
        .expect("env-supplied token should produce a valid config");
    assert_eq!(cfg.auth_token, "from-env");
}

#[test]
fn cloud_auth_header_env_var_takes_precedence_over_config_file() {
    // Env-lookup returns a header → it wins over `auth_header` in the file.
    // Mirrors electricity_maps_env_var_takes_precedence_over_config_file.
    let raw = CloudSection {
        prometheus_endpoint: Some("http://prometheus:9090".to_string()),
        scrape_interval_secs: None,
        default_provider: None,
        default_instance_type: None,
        cpu_metric: None,
        services: HashMap::new(),
        auth_header: Some("Authorization: Bearer from-file".to_string()),
    };
    let cfg =
        convert_cloud_section_with_env(&raw, || Some("Authorization: Bearer from-env".to_string()))
            .expect("cloud section should convert");
    assert_eq!(
        cfg.auth_header.as_deref(),
        Some("Authorization: Bearer from-env"),
        "env var must take precedence over the config file value"
    );
}

#[test]
fn cloud_auth_header_falls_back_to_config_when_env_unset() {
    let raw = CloudSection {
        prometheus_endpoint: Some("http://prometheus:9090".to_string()),
        scrape_interval_secs: None,
        default_provider: None,
        default_instance_type: None,
        cpu_metric: None,
        services: HashMap::new(),
        auth_header: Some("Authorization: Bearer from-file".to_string()),
    };
    let cfg = convert_cloud_section_with_env(&raw, || None).expect("cloud section should convert");
    assert_eq!(
        cfg.auth_header.as_deref(),
        Some("Authorization: Bearer from-file"),
        "config value is used when the env var is unset"
    );
}

#[test]
fn scaphandre_auth_header_env_var_takes_precedence_over_config_file() {
    let raw = ScaphandreSection {
        endpoint: Some("http://localhost:8080/metrics".to_string()),
        scrape_interval_secs: None,
        process_map: HashMap::new(),
        auth_header: Some("Authorization: Bearer from-file".to_string()),
    };
    let cfg = convert_scaphandre_section_with_env(&raw, || {
        Some("Authorization: Bearer from-env".to_string())
    })
    .expect("scaphandre section should convert");
    assert_eq!(
        cfg.auth_header.as_deref(),
        Some("Authorization: Bearer from-env"),
        "env var must take precedence over the config file value"
    );
}

#[test]
fn scaphandre_auth_header_falls_back_to_config_when_env_unset() {
    let raw = ScaphandreSection {
        endpoint: Some("http://localhost:8080/metrics".to_string()),
        scrape_interval_secs: None,
        process_map: HashMap::new(),
        auth_header: Some("Authorization: Bearer from-file".to_string()),
    };
    let cfg = convert_scaphandre_section_with_env(&raw, || None)
        .expect("scaphandre section should convert");
    assert_eq!(
        cfg.auth_header.as_deref(),
        Some("Authorization: Bearer from-file"),
        "config value is used when the env var is unset"
    );
}

// --- validate_electricity_maps error paths ---

#[test]
fn validate_electricity_maps_rejects_control_char_in_token() {
    let cfg = ElectricityMapsConfig {
        api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
        auth_token: "tok\x07en".to_string(), // contains a control char
        poll_interval: Duration::from_mins(5),
        region_map: {
            let mut m = HashMap::new();
            m.insert("eu-west-3".to_string(), "FR".to_string());
            m
        },
        emission_factor_type: EmissionFactorType::default(),
        temporal_granularity: TemporalGranularity::default(),
    };
    let err = Config::validate_electricity_maps(&cfg).unwrap_err();
    assert!(err.contains("control"));
}

#[test]
fn validate_electricity_maps_rejects_empty_region_map() {
    let cfg = ElectricityMapsConfig {
        api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
        auth_token: "tok".to_string(),
        poll_interval: Duration::from_mins(5),
        region_map: HashMap::new(),
        emission_factor_type: EmissionFactorType::default(),
        temporal_granularity: TemporalGranularity::default(),
    };
    let err = Config::validate_electricity_maps(&cfg).unwrap_err();
    assert!(err.contains("region_map"));
}

#[test]
fn validate_electricity_maps_rejects_empty_zone() {
    let mut region_map = HashMap::new();
    region_map.insert("eu-west-3".to_string(), String::new());
    let cfg = ElectricityMapsConfig {
        api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
        auth_token: "tok".to_string(),
        poll_interval: Duration::from_mins(5),
        region_map,
        emission_factor_type: EmissionFactorType::default(),
        temporal_granularity: TemporalGranularity::default(),
    };
    let err = Config::validate_electricity_maps(&cfg).unwrap_err();
    assert!(err.contains("empty"));
}

#[test]
fn validate_electricity_maps_rejects_invalid_poll_interval() {
    let mut region_map = HashMap::new();
    region_map.insert("eu-west-3".to_string(), "FR".to_string());
    let cfg = ElectricityMapsConfig {
        api_endpoint: "https://api.electricitymaps.com/v4".to_string(),
        auth_token: "tok".to_string(),
        poll_interval: Duration::from_secs(10), // below 60
        region_map,
        emission_factor_type: EmissionFactorType::default(),
        temporal_granularity: TemporalGranularity::default(),
    };
    let err = Config::validate_electricity_maps(&cfg).unwrap_err();
    assert!(err.contains("poll_interval"));
}

// ---------------------------------------------------------------
// TLS validation
// ---------------------------------------------------------------

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_tls_accepts_both_absent() {
    let cfg = Config::default();
    assert!(cfg.validate_tls().is_ok());
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_tls_rejects_cert_without_key() {
    let mut cfg = Config::default();
    cfg.daemon.tls.cert_path = Some("/tmp/cert.pem".to_string());
    let err = cfg.validate_tls().unwrap_err();
    assert!(err.contains("tls.key_path is missing"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_tls_rejects_key_without_cert() {
    let mut cfg = Config::default();
    cfg.daemon.tls.key_path = Some("/tmp/key.pem".to_string());
    let err = cfg.validate_tls().unwrap_err();
    assert!(err.contains("tls.cert_path is missing"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_tls_rejects_nonexistent_cert() {
    let mut cfg = Config::default();
    cfg.daemon.tls.cert_path = Some("/nonexistent/cert.pem".to_string());
    cfg.daemon.tls.key_path = Some("/nonexistent/key.pem".to_string());
    let err = cfg.validate_tls().unwrap_err();
    assert!(err.contains("does not exist"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_tls_accepts_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, b"fake cert").unwrap();
    std::fs::write(&key, b"fake key").unwrap();

    let mut cfg = Config::default();
    cfg.daemon.tls.cert_path = Some(cert.to_str().unwrap().to_string());
    cfg.daemon.tls.key_path = Some(key.to_str().unwrap().to_string());
    assert!(cfg.validate_tls().is_ok());
}

#[test]
fn tls_config_fields_round_trip_through_toml() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, b"fake cert").unwrap();
    std::fs::write(&key, b"fake key").unwrap();
    let toml = format!(
        "[daemon]\ntls_cert_path = \"{}\"\ntls_key_path = \"{}\"",
        cert.display(),
        key.display()
    );
    let cfg = load_from_str(&toml).unwrap();
    assert_eq!(
        cfg.daemon.tls.cert_path.as_deref(),
        Some(cert.to_str().unwrap())
    );
    assert_eq!(
        cfg.daemon.tls.key_path.as_deref(),
        Some(key.to_str().unwrap())
    );
}

#[test]
fn tls_config_defaults_to_none() {
    let cfg = load_from_str("").unwrap();
    assert!(cfg.daemon.tls.cert_path.is_none());
    assert!(cfg.daemon.tls.key_path.is_none());
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_tls_rejects_control_chars_in_cert_path() {
    let mut cfg = Config::default();
    cfg.daemon.tls.cert_path = Some("/tmp/cert\x00.pem".to_string());
    cfg.daemon.tls.key_path = Some("/tmp/key.pem".to_string());
    let err = cfg.validate_tls().unwrap_err();
    assert!(err.contains("control characters"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_tls_rejects_control_chars_in_key_path() {
    let mut cfg = Config::default();
    cfg.daemon.tls.cert_path = Some("/tmp/cert.pem".to_string());
    cfg.daemon.tls.key_path = Some("/tmp/key\n.pem".to_string());
    let err = cfg.validate_tls().unwrap_err();
    assert!(err.contains("control characters"), "{err}");
}

#[test]
fn default_daemon_ack_is_enabled_with_no_secrets() {
    let cfg = Config::default();
    assert!(cfg.daemon.ack.enabled);
    assert!(cfg.daemon.ack.storage_path.is_none());
    assert!(cfg.daemon.ack.api_key.is_none());
    assert!(cfg.daemon.ack.toml_path.is_none());
}

#[test]
fn parse_daemon_ack_section_overrides() {
    let toml = "
[daemon.ack]
enabled = false
storage_path = \"/var/lib/perf-sentinel/acks.jsonl\"
api_key = \"a-long-enough-secret-key\"
toml_path = \"/etc/perf-sentinel/acknowledgments.toml\"
";
    let cfg = load_from_str(toml).unwrap();
    assert!(!cfg.daemon.ack.enabled);
    assert_eq!(
        cfg.daemon.ack.storage_path.as_deref(),
        Some("/var/lib/perf-sentinel/acks.jsonl")
    );
    assert_eq!(
        cfg.daemon.ack.api_key.as_deref(),
        Some("a-long-enough-secret-key")
    );
    assert_eq!(
        cfg.daemon.ack.toml_path.as_deref(),
        Some("/etc/perf-sentinel/acknowledgments.toml")
    );
}

#[test]
fn parse_daemon_queue_capacities_override_and_default() {
    let toml = "
[daemon]
ingest_queue_capacity = 4096
analysis_queue_capacity = 2048
";
    let cfg = load_from_str(toml).unwrap();
    assert_eq!(cfg.daemon.ingest_queue_capacity, 4096);
    assert_eq!(cfg.daemon.analysis_queue_capacity, 2048);

    // Both default to 1024 when the keys are absent.
    let cfg = load_from_str("").unwrap();
    assert_eq!(cfg.daemon.ingest_queue_capacity, 1024);
    assert_eq!(cfg.daemon.analysis_queue_capacity, 1024);
}

#[test]
fn validate_daemon_rejects_zero_queue_capacity() {
    let toml = "
[daemon]
analysis_queue_capacity = 0
";
    let err = load_from_str(toml).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("analysis_queue_capacity"), "{msg}");
}

#[test]
fn validate_daemon_ack_rejects_empty_api_key() {
    let toml = "
[daemon.ack]
api_key = \"\"
";
    let err = load_from_str(toml).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("must not be empty"), "{msg}");
}

#[test]
fn validate_daemon_ack_rejects_short_api_key() {
    let toml = "
[daemon.ack]
api_key = \"shortish\"
";
    let err = load_from_str(toml).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("too short"), "{msg}");
}

#[test]
fn validate_daemon_ack_accepts_twelve_char_api_key() {
    let toml = "
[daemon.ack]
api_key = \"short-enough\"
";
    let cfg = load_from_str(toml).unwrap();
    assert_eq!(cfg.daemon.ack.api_key.as_deref(), Some("short-enough"));
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_ack_rejects_control_chars_in_storage_path() {
    let mut cfg = Config::default();
    cfg.daemon.ack.storage_path = Some("/var/lib/acks\x00.jsonl".to_string());
    let err = cfg.validate_daemon_ack().unwrap_err();
    assert!(err.contains("control characters"), "{err}");
}

#[test]
fn validate_daemon_cors_accepts_empty_default() {
    let cfg = Config::default();
    assert!(cfg.validate_daemon_cors().is_ok());
    assert!(cfg.daemon.cors.allowed_origins.is_empty());
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_accepts_wildcard() {
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec!["*".to_string()];
    assert!(cfg.validate_daemon_cors().is_ok());
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_accepts_https_origin() {
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec!["https://reports.example.com".to_string()];
    assert!(cfg.validate_daemon_cors().is_ok());
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_rejects_origin_without_scheme() {
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec!["reports.example.com".to_string()];
    let err = cfg.validate_daemon_cors().unwrap_err();
    assert!(err.contains("must start with http://"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_rejects_trailing_slash() {
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec!["https://reports.example.com/".to_string()];
    let err = cfg.validate_daemon_cors().unwrap_err();
    assert!(err.contains("trailing slash"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_rejects_empty_entry() {
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec![String::new()];
    let err = cfg.validate_daemon_cors().unwrap_err();
    assert!(err.contains("is empty"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_rejects_control_chars() {
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec!["https://example.com\nattacker".to_string()];
    let err = cfg.validate_daemon_cors().unwrap_err();
    assert!(err.contains("control characters"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_rejects_wildcard_mixed_with_explicit_origins() {
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec!["*".to_string(), "https://example.com".to_string()];
    let err = cfg.validate_daemon_cors().unwrap_err();
    assert!(err.contains("cannot mix"), "{err}");
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn validate_daemon_cors_rejects_wildcard_with_api_key() {
    // X-API-Key is a header (not a cookie), so allow_credentials = false
    // does not block it, any browser origin can replay it under wildcard
    // CORS. Reject the combination at config load.
    let mut cfg = Config::default();
    cfg.daemon.cors.allowed_origins = vec!["*".to_string()];
    cfg.daemon.ack.api_key = Some("test-token-12chars".to_string());
    let err = cfg.validate_daemon_cors().unwrap_err();
    assert!(
        err.contains("incompatible with") && err.contains("api_key"),
        "{err}"
    );
}

#[test]
fn cors_section_round_trips_via_toml() {
    let toml = r#"
[daemon.cors]
allowed_origins = ["https://reports.example.com", "https://gitlab.example.com"]
"#;
    let cfg = load_from_str(toml).expect("valid TOML");
    assert_eq!(
        cfg.daemon.cors.allowed_origins,
        vec![
            "https://reports.example.com".to_string(),
            "https://gitlab.example.com".to_string(),
        ]
    );
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn cors_with_api_disabled_is_rejected() {
    // The CORS layer attaches to the `/api/*` sub-router only.
    // When `[daemon] api_enabled = false`, the sub-router is not
    // mounted, so a non-empty `allowed_origins` would silently do
    // nothing post-deploy. Reject at config load.
    let mut cfg = Config::default();
    cfg.daemon.api_enabled = false;
    cfg.daemon.cors.allowed_origins = vec!["https://reports.example.com".to_string()];
    let err = cfg.validate().unwrap_err();
    assert!(
        err.contains("api_enabled = false"),
        "expected mismatch error, got: {err}"
    );
}

#[test]
fn cors_disabled_with_api_disabled_is_accepted() {
    let cfg = Config {
        daemon: DaemonConfig {
            api_enabled: false,
            ..DaemonConfig::default()
        },
        ..Config::default()
    };
    // Empty CORS list = layer not wired = no inconsistency.
    assert!(cfg.daemon.cors.allowed_origins.is_empty());
    assert!(cfg.validate().is_ok());
}

#[test]
fn cors_section_rejects_mixed_wildcard_via_toml_load() {
    // The mixed-wildcard rule is enforced at validation time, not
    // at deserialization. Verify the full `load_from_str` path
    // surfaces the validation error rather than silently dropping
    // explicit origins on the way in.
    let toml = r#"
[daemon.cors]
allowed_origins = ["*", "https://reports.example.com"]
"#;
    let err = load_from_str(toml).expect_err("mixed wildcard must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("cannot mix"),
        "expected validation error to mention mixing: {msg}"
    );
}

#[test]
fn reporting_section_parses_and_validates() {
    let toml = r#"
[reporting]
intent = "official"
confidentiality_level = "public"
org_config_path = "/etc/perf-sentinel/org.toml"
disclose_output_path = "/var/lib/perf-sentinel/last.json"
disclose_period = "calendar-quarter"
"#;
    let cfg = load_from_str(toml).expect("valid reporting section");
    assert_eq!(cfg.reporting.intent.as_deref(), Some("official"));
    assert_eq!(
        cfg.reporting.confidentiality_level.as_deref(),
        Some("public")
    );
    assert_eq!(
        cfg.reporting.org_config_path.as_deref(),
        Some("/etc/perf-sentinel/org.toml")
    );
}

#[test]
fn reporting_unknown_intent_rejected() {
    let toml = r#"
[reporting]
intent = "draft"
"#;
    let err = load_from_str(toml).expect_err("unknown intent must be rejected");
    assert!(err.to_string().contains("intent must be one of"));
}

#[test]
fn reporting_unknown_confidentiality_rejected() {
    let toml = r#"
[reporting]
confidentiality_level = "restricted"
"#;
    let err = load_from_str(toml).expect_err("unknown confidentiality must be rejected");
    assert!(err.to_string().contains("confidentiality_level"));
}

#[test]
fn reporting_official_requires_org_config_path() {
    let toml = r#"
[reporting]
intent = "official"
"#;
    let err = load_from_str(toml).expect_err("missing org_config_path must be rejected");
    assert!(err.to_string().contains("org_config_path"));
}

#[test]
fn reporting_disclose_output_path_accepted_but_unused() {
    // disclose_output_path is reserved for daemon-triggered
    // periodic disclosures (planned for 0.8.0). Today it is
    // accepted by the parser but unused at runtime, and the
    // validator logs a tracing::warn so operators do not silently
    // depend on a no-op. The test confirms the field survives
    // round-trip parsing without error; the warning itself is
    // surfaced as an effect at validate time.
    let toml = r#"
[reporting]
disclose_output_path = "/var/lib/perf-sentinel/last.json"
"#;
    let cfg = load_from_str(toml).expect("disclose_output_path must parse without error");
    assert_eq!(
        cfg.reporting.disclose_output_path.as_deref(),
        Some("/var/lib/perf-sentinel/last.json")
    );
}

#[test]
fn reporting_sigstore_absent_defaults_to_public() {
    let cfg = load_from_str("[reporting]\n").expect("valid empty reporting");
    assert_eq!(cfg.reporting.sigstore.rekor_url, DEFAULT_REKOR_URL);
    assert_eq!(cfg.reporting.sigstore.fulcio_url, DEFAULT_FULCIO_URL);
}

#[test]
fn reporting_sigstore_section_overrides_defaults() {
    let toml = r#"
[reporting.sigstore]
rekor_url = "https://rekor.internal.example.fr"
fulcio_url = "https://fulcio.internal.example.fr"
"#;
    let cfg = load_from_str(toml).expect("valid sigstore section");
    assert_eq!(
        cfg.reporting.sigstore.rekor_url,
        "https://rekor.internal.example.fr"
    );
    assert_eq!(
        cfg.reporting.sigstore.fulcio_url,
        "https://fulcio.internal.example.fr"
    );
}

#[test]
fn daemon_archive_section_parses_with_defaults() {
    let toml = r#"
[daemon.archive]
path = "/var/lib/perf-sentinel/reports.ndjson"
"#;
    let cfg = load_from_str(toml).expect("valid archive section");
    let archive = cfg.daemon.archive.expect("archive must be Some");
    assert_eq!(archive.path, "/var/lib/perf-sentinel/reports.ndjson");
    assert_eq!(archive.max_size_mb, 100);
    assert_eq!(archive.max_files, 12);
}

#[test]
fn daemon_archive_zero_size_rejected() {
    let toml = r#"
[daemon.archive]
path = "/tmp/a.ndjson"
max_size_mb = 0
"#;
    let err = load_from_str(toml).expect_err("zero size must be rejected");
    assert!(err.to_string().contains("max_size_mb"));
}

#[test]
fn daemon_archive_zero_files_rejected() {
    let toml = r#"
[daemon.archive]
path = "/tmp/a.ndjson"
max_files = 0
"#;
    let err = load_from_str(toml).expect_err("zero files must be rejected");
    assert!(err.to_string().contains("max_files"));
}

#[test]
fn daemon_archive_absent_section_yields_none() {
    let cfg = load_from_str("").expect("empty config parses");
    assert!(cfg.daemon.archive.is_none());
}

// ---- Kepler / Redfish converters --------------------------------------

#[test]
fn parse_kepler_metric_kind_defaults_and_aliases() {
    use crate::score::kepler::config::KeplerMetricKind;
    assert_eq!(
        parse_kepler_metric_kind(None).unwrap(),
        KeplerMetricKind::Container
    );
    assert_eq!(
        parse_kepler_metric_kind(Some("container")).unwrap(),
        KeplerMetricKind::Container
    );
    assert_eq!(
        parse_kepler_metric_kind(Some("process")).unwrap(),
        KeplerMetricKind::Process
    );
    // Surrounding whitespace must be trimmed before matching.
    assert_eq!(
        parse_kepler_metric_kind(Some("  process  ")).unwrap(),
        KeplerMetricKind::Process
    );
}

#[test]
fn parse_kepler_metric_kind_is_case_insensitive() {
    use crate::score::kepler::config::KeplerMetricKind;
    for raw in ["Container", "CONTAINER", "  Container  "] {
        assert_eq!(
            parse_kepler_metric_kind(Some(raw)).unwrap(),
            KeplerMetricKind::Container,
            "case-insensitive match expected for {raw:?}"
        );
    }
    for raw in ["Process", "PROCESS", "ProCess"] {
        assert_eq!(
            parse_kepler_metric_kind(Some(raw)).unwrap(),
            KeplerMetricKind::Process,
            "case-insensitive match expected for {raw:?}"
        );
    }
}

#[test]
fn parse_kepler_metric_kind_empty_string_rejected() {
    for raw in ["", "   "] {
        let err = parse_kepler_metric_kind(Some(raw))
            .expect_err("explicit empty must error, not silently default");
        assert!(err.contains("is empty"));
        assert!(err.contains("'container' or 'process'"));
    }
}

#[test]
fn parse_kepler_metric_kind_unknown_value_errors() {
    let err = parse_kepler_metric_kind(Some("rapl")).expect_err("unknown variant must error");
    assert!(err.contains("metric_kind 'rapl'"));
    assert!(err.contains("'container'"));
    assert!(err.contains("'process'"));
}

#[test]
fn parse_kepler_metric_kind_legacy_values_rejected() {
    for legacy in ["process_package", "process_dram"] {
        let err = parse_kepler_metric_kind(Some(legacy))
            .expect_err("legacy variant must error with migration guidance");
        assert!(err.contains(legacy));
        assert!(err.contains("removed in v0.7.5"));
        assert!(err.contains("'process'"));
    }
}

#[test]
fn parse_kepler_metric_kind_error_preserves_raw_whitespace() {
    let err = parse_kepler_metric_kind(Some("  process_package  "))
        .expect_err("legacy variant must error");
    // The error preserves the operator's exact literal so `grep -F` on
    // the source TOML matches the message.
    assert!(
        err.contains("'  process_package  '"),
        "expected raw value in error, got: {err}"
    );
}

#[test]
fn parse_kepler_metric_kind_rejects_control_characters() {
    // A `.perf-sentinel.toml` with ANSI escapes in metric_kind must
    // be rejected before the literal lands in the error message, so
    // stderr stays clean.
    let raw = "container\u{001b}[31m";
    let err = parse_kepler_metric_kind(Some(raw)).expect_err("control characters must be rejected");
    assert!(err.contains("contains control characters"));
    // The raw payload must NOT be echoed back into the message.
    assert!(!err.contains('\u{001b}'));
}

#[test]
fn load_from_str_rejects_legacy_kepler_metric_kind_loudly() {
    // Round-trip: a legacy metric_kind in a real TOML must fail at
    // load time, not silently disable the Kepler section.
    let toml = r#"
[green.kepler]
endpoint = "http://kepler:9102/metrics"
metric_kind = "process_package"
"#;
    let err = load_from_str(toml)
        .expect_err("load_from_str must surface the migration error, not silently drop");
    let msg = err.to_string();
    assert!(msg.contains("process_package"));
    assert!(msg.contains("removed in v0.7.5"));
}

#[test]
fn convert_kepler_section_without_endpoint_yields_none() {
    let raw = KeplerSection::default();
    assert!(convert_kepler_section_with_env(&raw, || None).is_none());
}

#[test]
fn convert_kepler_section_env_overrides_file_auth_header() {
    let raw = KeplerSection {
        endpoint: Some("http://kepler:9102/metrics".to_string()),
        auth_header: Some("Bearer file-token".to_string()),
        ..Default::default()
    };
    let cfg = convert_kepler_section_with_env(&raw, || Some("Bearer env-token".to_string()))
        .expect("endpoint set, expected Some");
    assert_eq!(cfg.auth_header.as_deref(), Some("Bearer env-token"));
    assert_eq!(cfg.endpoint, "http://kepler:9102/metrics");
}

#[test]
fn convert_kepler_section_file_auth_used_when_env_absent() {
    let raw = KeplerSection {
        endpoint: Some("http://kepler:9102/metrics".to_string()),
        auth_header: Some("Bearer file".to_string()),
        ..Default::default()
    };
    let cfg = convert_kepler_section_with_env(&raw, || None).expect("endpoint set");
    assert_eq!(cfg.auth_header.as_deref(), Some("Bearer file"));
}

#[test]
fn convert_kepler_section_unknown_metric_kind_yields_none() {
    let raw = KeplerSection {
        endpoint: Some("http://kepler:9102/metrics".to_string()),
        metric_kind: Some("rapl".to_string()),
        ..Default::default()
    };
    assert!(convert_kepler_section_with_env(&raw, || None).is_none());
}

#[test]
fn convert_kepler_section_uses_default_scrape_interval() {
    let raw = KeplerSection {
        endpoint: Some("http://kepler:9102/metrics".to_string()),
        ..Default::default()
    };
    let cfg = convert_kepler_section_with_env(&raw, || None).expect("endpoint set");
    assert_eq!(cfg.scrape_interval, Duration::from_secs(5));
}

#[test]
fn convert_redfish_section_empty_endpoints_yields_none() {
    let raw = RedfishSection::default();
    assert!(convert_redfish_section_with_env(&raw, || None).is_none());
}

fn sample_redfish_endpoint() -> RedfishEndpoint {
    use crate::score::redfish::RedfishSchema;
    RedfishEndpoint {
        url: "https://bmc.local".to_string(),
        schema: RedfishSchema::LegacyPower,
    }
}

#[test]
fn convert_redfish_section_env_overrides_file_auth_header() {
    let mut endpoints = HashMap::new();
    endpoints.insert("rack1".to_string(), sample_redfish_endpoint());
    let raw = RedfishSection {
        endpoints,
        auth_header: Some("Basic file".to_string()),
        ..Default::default()
    };
    let cfg = convert_redfish_section_with_env(&raw, || Some("Basic env".to_string()))
        .expect("endpoints set, expected Some");
    assert_eq!(cfg.auth_header.as_deref(), Some("Basic env"));
    assert_eq!(cfg.endpoints.len(), 1);
}

#[test]
fn convert_redfish_section_file_auth_used_when_env_absent() {
    let mut endpoints = HashMap::new();
    endpoints.insert("rack1".to_string(), sample_redfish_endpoint());
    let raw = RedfishSection {
        endpoints,
        auth_header: Some("Basic file".to_string()),
        ..Default::default()
    };
    let cfg =
        convert_redfish_section_with_env(&raw, || None).expect("endpoints set, expected Some");
    assert_eq!(cfg.auth_header.as_deref(), Some("Basic file"));
}

#[test]
fn convert_redfish_section_applies_default_interval() {
    let mut endpoints = HashMap::new();
    endpoints.insert("rack1".to_string(), sample_redfish_endpoint());
    let raw = RedfishSection {
        endpoints,
        ..Default::default()
    };
    let cfg =
        convert_redfish_section_with_env(&raw, || None).expect("endpoints set, expected Some");
    assert_eq!(cfg.scrape_interval, Duration::from_mins(1));
}

// ---- validate_kepler ---------------------------------------------------

fn minimal_kepler_config() -> KeplerConfig {
    use crate::score::kepler::config::KeplerMetricKind;
    KeplerConfig {
        endpoint: "http://kepler:9102/metrics".to_string(),
        scrape_interval: Duration::from_secs(5),
        metric_kind: KeplerMetricKind::Container,
        service_mappings: HashMap::new(),
        auth_header: None,
    }
}

#[test]
fn validate_kepler_accepts_minimal_config() {
    let cfg = minimal_kepler_config();
    assert!(Config::validate_kepler(&cfg).is_ok());
}

#[test]
fn validate_kepler_rejects_empty_endpoint() {
    let mut cfg = minimal_kepler_config();
    cfg.endpoint = String::new();
    let err = Config::validate_kepler(&cfg).expect_err("empty endpoint must error");
    assert!(err.contains("endpoint is required"));
}

#[test]
fn validate_kepler_rejects_non_http_scheme() {
    let mut cfg = minimal_kepler_config();
    cfg.endpoint = "ftp://kepler/metrics".to_string();
    let err = Config::validate_kepler(&cfg).expect_err("non-http scheme must error");
    assert!(err.contains("must start with 'http://' or 'https://'"));
}

#[test]
fn validate_kepler_rejects_scrape_interval_zero() {
    let mut cfg = minimal_kepler_config();
    cfg.scrape_interval = Duration::from_secs(0);
    let err = Config::validate_kepler(&cfg).expect_err("zero interval must error");
    assert!(err.contains("scrape_interval_secs must be in [1, 3600]"));
}

#[test]
fn validate_kepler_rejects_scrape_interval_above_max() {
    let mut cfg = minimal_kepler_config();
    cfg.scrape_interval = Duration::from_secs(3601);
    let err = Config::validate_kepler(&cfg).expect_err("interval above 3600 must error");
    assert!(err.contains("3601"));
}

#[test]
fn validate_kepler_rejects_empty_service_name() {
    let mut cfg = minimal_kepler_config();
    cfg.service_mappings
        .insert(String::new(), "label".to_string());
    let err = Config::validate_kepler(&cfg).expect_err("empty service name must error");
    assert!(err.contains("service name") && err.contains("1-256"));
}

#[test]
fn validate_kepler_rejects_control_char_in_service_name() {
    let mut cfg = minimal_kepler_config();
    cfg.service_mappings
        .insert("svc\u{0007}".to_string(), "label".to_string());
    let err = Config::validate_kepler(&cfg).expect_err("control char must error");
    assert!(err.contains("control characters"));
}

#[test]
fn validate_kepler_rejects_empty_label() {
    let mut cfg = minimal_kepler_config();
    cfg.service_mappings
        .insert("svc".to_string(), String::new());
    let err = Config::validate_kepler(&cfg).expect_err("empty label must error");
    assert!(err.contains("label for service") && err.contains("1-256"));
}

#[test]
fn validate_kepler_rejects_control_char_in_label() {
    let mut cfg = minimal_kepler_config();
    cfg.service_mappings
        .insert("svc".to_string(), "lab\u{0007}el".to_string());
    let err = Config::validate_kepler(&cfg).expect_err("control char in label must error");
    assert!(err.contains("control characters"));
}

#[test]
fn validate_kepler_process_caps_label_at_kernel_truncation() {
    use crate::score::kepler::config::KeplerMetricKind;
    // Process metric_kind reads the kernel `comm` label, truncated
    // to 15 bytes. A 16-char label can never match a real sample.
    let mut cfg = minimal_kepler_config();
    cfg.metric_kind = KeplerMetricKind::Process;
    cfg.service_mappings
        .insert("svc".to_string(), "my-long-checkout".to_string()); // 16 chars
    let err = Config::validate_kepler(&cfg)
        .expect_err("16-char label must be rejected for Process metric_kind");
    assert!(err.contains("must be 1-15 chars"));
    assert!(err.contains("kernel truncates"));
}

#[test]
fn validate_kepler_process_accepts_15_char_label() {
    use crate::score::kepler::config::KeplerMetricKind;
    let mut cfg = minimal_kepler_config();
    cfg.metric_kind = KeplerMetricKind::Process;
    cfg.service_mappings
        .insert("svc".to_string(), "exactly-15char!".to_string()); // 15 chars
    assert!(Config::validate_kepler(&cfg).is_ok());
}

#[test]
fn validate_kepler_service_mappings_caps_cardinality() {
    // Bound the config-load memory footprint against fat-finger or
    // hostile configs. 1025 > MAX_KEPLER_SERVICE_MAPPINGS (1024).
    let mut cfg = minimal_kepler_config();
    for i in 0..1025 {
        cfg.service_mappings
            .insert(format!("svc-{i}"), format!("label-{i}"));
    }
    let err = Config::validate_kepler(&cfg).expect_err("1025 mappings must exceed the 1024 cap");
    assert!(err.contains("1025 entries"));
    assert!(err.contains("maximum is 1024"));
}

#[test]
fn convert_kepler_section_with_process_metric_kind() {
    use crate::score::kepler::config::KeplerMetricKind;
    let raw = KeplerSection {
        endpoint: Some("http://kepler:9102/metrics".to_string()),
        metric_kind: Some("process".to_string()),
        ..Default::default()
    };
    let cfg = convert_kepler_section_with_env(&raw, || None)
        .expect("endpoint set + valid metric_kind, expected Some");
    assert_eq!(cfg.metric_kind, KeplerMetricKind::Process);
}

// ---- validate_redfish --------------------------------------------------

fn redfish_endpoint(url: &str) -> RedfishEndpoint {
    use crate::score::redfish::RedfishSchema;
    RedfishEndpoint {
        url: url.to_string(),
        schema: RedfishSchema::LegacyPower,
    }
}

fn minimal_redfish_config() -> RedfishConfig {
    let mut endpoints = HashMap::new();
    endpoints.insert(
        "rack1".to_string(),
        redfish_endpoint("https://bmc.local/Power"),
    );
    RedfishConfig {
        endpoints,
        scrape_interval: Duration::from_mins(1),
        service_mappings: HashMap::new(),
        ca_bundle_path: None,
        auth_header: None,
    }
}

#[test]
fn validate_redfish_accepts_minimal_config() {
    let cfg = minimal_redfish_config();
    assert!(Config::validate_redfish(&cfg).is_ok());
}

#[test]
fn validate_redfish_rejects_empty_endpoints() {
    let mut cfg = minimal_redfish_config();
    cfg.endpoints.clear();
    let err = Config::validate_redfish(&cfg).expect_err("empty endpoints must error");
    assert!(err.contains("endpoints must contain at least one chassis"));
}

#[test]
fn validate_redfish_rejects_empty_chassis_id() {
    let mut cfg = minimal_redfish_config();
    cfg.endpoints.clear();
    cfg.endpoints
        .insert(String::new(), redfish_endpoint("https://bmc/Power"));
    let err = Config::validate_redfish(&cfg).expect_err("empty chassis id must error");
    assert!(err.contains("chassis id") && err.contains("1-256"));
}

#[test]
fn validate_redfish_rejects_control_char_in_chassis_id() {
    let mut cfg = minimal_redfish_config();
    cfg.endpoints.clear();
    cfg.endpoints.insert(
        "rack\u{0007}".to_string(),
        redfish_endpoint("https://bmc/Power"),
    );
    let err = Config::validate_redfish(&cfg).expect_err("control char must error");
    assert!(err.contains("control characters"));
}

#[test]
fn validate_redfish_rejects_non_http_endpoint() {
    let mut cfg = minimal_redfish_config();
    cfg.endpoints.clear();
    cfg.endpoints
        .insert("rack1".to_string(), redfish_endpoint("ftp://bmc/Power"));
    let err = Config::validate_redfish(&cfg).expect_err("non-http endpoint must error");
    assert!(err.contains("must start with 'http://' or 'https://'"));
}

#[test]
fn validate_redfish_rejects_scrape_interval_below_min() {
    let mut cfg = minimal_redfish_config();
    cfg.scrape_interval = Duration::from_secs(5);
    let err = Config::validate_redfish(&cfg).expect_err("scrape_interval below 15 must error");
    assert!(err.contains("scrape_interval_secs"));
    assert!(err.contains("rate-limit"));
}

#[test]
fn validate_redfish_rejects_scrape_interval_above_max() {
    let mut cfg = minimal_redfish_config();
    cfg.scrape_interval = Duration::from_secs(4000);
    let err = Config::validate_redfish(&cfg).expect_err("scrape_interval above MAX must error");
    assert!(err.contains("4000"));
}

#[test]
fn validate_redfish_rejects_unknown_chassis_in_mapping() {
    let mut cfg = minimal_redfish_config();
    cfg.service_mappings
        .insert("svc".to_string(), "rack-missing".to_string());
    let err = Config::validate_redfish(&cfg).expect_err("unknown chassis must error");
    assert!(err.contains("rack-missing"));
    assert!(err.contains("not declared"));
}

#[test]
fn validate_redfish_rejects_empty_service_name_in_mapping() {
    let mut cfg = minimal_redfish_config();
    cfg.service_mappings
        .insert(String::new(), "rack1".to_string());
    let err = Config::validate_redfish(&cfg).expect_err("empty service name must error");
    assert!(err.contains("service name") && err.contains("1-256"));
}

#[test]
fn validate_redfish_rejects_control_char_in_service_name() {
    let mut cfg = minimal_redfish_config();
    cfg.service_mappings
        .insert("svc\u{0001}".to_string(), "rack1".to_string());
    let err = Config::validate_redfish(&cfg).expect_err("control char must error");
    assert!(err.contains("control characters"));
}

#[test]
fn validate_redfish_rejects_empty_ca_bundle_path() {
    let mut cfg = minimal_redfish_config();
    cfg.ca_bundle_path = Some(String::new());
    let err = Config::validate_redfish(&cfg).expect_err("empty ca_bundle_path must error");
    assert!(err.contains("ca_bundle_path must be non-empty"));
}

// End-to-end TOML load exercises convert_*_section wrappers and the
// validate() dispatch lines for kepler/redfish.

#[test]
fn load_toml_with_kepler_and_redfish_sections() {
    use crate::score::redfish::RedfishSchema;
    let toml = r#"
[green.kepler]
endpoint = "http://kepler:9102/metrics"
scrape_interval_secs = 10
metric_kind = "container"

[green.redfish]
scrape_interval_secs = 60

[green.redfish.endpoints."rack-legacy"]
url = "https://bmc-legacy.local/redfish/v1/Chassis/1/Power"
schema = "legacy_power"

[green.redfish.endpoints."rack-modern"]
url = "https://bmc-modern.local/redfish/v1/Chassis/1/EnvironmentMetrics"
schema = "environment_metrics"
"#;
    let cfg = load_from_str(toml).expect("kepler+redfish toml parses and validates");
    let kepler = cfg.green.kepler.expect("kepler section produced a config");
    assert_eq!(kepler.endpoint, "http://kepler:9102/metrics");
    assert_eq!(kepler.scrape_interval, Duration::from_secs(10));
    let redfish = cfg
        .green
        .redfish
        .expect("redfish section produced a config");
    assert_eq!(redfish.endpoints.len(), 2);
    assert_eq!(
        redfish.endpoints.get("rack-legacy").unwrap().schema,
        RedfishSchema::LegacyPower
    );
    assert_eq!(
        redfish.endpoints.get("rack-modern").unwrap().schema,
        RedfishSchema::EnvironmentMetrics
    );
}

#[test]
fn load_toml_rejects_legacy_flat_endpoint_string() {
    // Pre-0.7.6 form was `"rack1" = "url"`. The new form requires
    // a table with `url` + `schema`, serde must reject the string.
    let toml = r#"
[green.redfish.endpoints]
"rack1" = "https://bmc.local/Power"
"#;
    let result = load_from_str(toml);
    assert!(
        result.is_err(),
        "legacy flat endpoint form must be rejected"
    );
}

#[test]
fn load_toml_rejects_unknown_redfish_schema() {
    let toml = r#"
[green.redfish.endpoints."rack1"]
url = "https://bmc.local/Power"
schema = "oem_custom"
"#;
    let result = load_from_str(toml);
    assert!(
        result.is_err(),
        "unknown schema variant must be rejected by serde"
    );
}

#[test]
fn load_toml_rejects_legacy_top_level_power_path() {
    // Pre-0.7.6 had `power_path = "/..."` at the [green.redfish]
    // top level, the new form moved schema selection to the
    // endpoint table. `deny_unknown_fields` on RedfishSection makes
    // a stale top-level `power_path` fail at load with a serde
    // error, no silent drop.
    let toml = r#"
[green.redfish]
power_path = "/PowerControl/0/PowerConsumedWatts"

[green.redfish.endpoints."rack1"]
url = "https://bmc.local/Power"
schema = "legacy_power"
"#;
    let result = load_from_str(toml);
    assert!(
        result.is_err(),
        "stale top-level power_path must be rejected"
    );
}

#[test]
fn load_toml_rejects_invalid_kepler_endpoint() {
    let toml = r#"
[green.kepler]
endpoint = "ftp://kepler/metrics"
"#;
    let err = load_from_str(toml).expect_err("invalid scheme must error at validate()");
    assert!(err.to_string().contains("[green.kepler]"));
}

#[test]
fn load_toml_rejects_invalid_redfish_scrape_interval() {
    let toml = r#"
[green.redfish]
scrape_interval_secs = 5

[green.redfish.endpoints."rack1"]
url = "https://bmc/Power"
schema = "legacy_power"
"#;
    let err = load_from_str(toml).expect_err("below-min interval must error at validate()");
    assert!(err.to_string().contains("[green.redfish]"));
}
