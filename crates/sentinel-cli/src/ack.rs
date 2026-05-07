//! `perf-sentinel ack` subcommand: terminal helper for the daemon ack
//! API. Wraps `POST/DELETE /api/findings/{sig}/ack` and `GET /api/acks`
//! into three subactions (`create`, `revoke`, `list`).
//!
//! Auth: opt-in server-side via `[daemon.ack] api_key`. Client-side,
//! the API key is resolved in priority order:
//! 1. `PERF_SENTINEL_DAEMON_API_KEY` env var
//! 2. `--api-key-file <path>` flag (file content, trailing newline stripped)
//! 3. Interactive `rpassword` prompt if a 401 is received and stdin is a TTY
//!
//! Exit codes follow Unix convention:
//! - 0: success
//! - 1: generic error (network failure, parse error, file IO)
//! - 2: client error (HTTP 4xx)
//! - 3: server error (HTTP 5xx)

#![cfg(feature = "daemon")]

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::QueryOutputFormat;
use crate::render::{AnsiColors, ansi_colors, no_colors};

const ENV_DAEMON_API_KEY: &str = "PERF_SENTINEL_DAEMON_API_KEY";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = "perf-sentinel-ack";
/// Per-page cap for `query inspect` boot fetch and post-submit
/// refetch. Re-exports the daemon's cap so the two sides cannot drift.
#[cfg(feature = "tui")]
pub(crate) const FINDINGS_FETCH_LIMIT: usize = sentinel_core::daemon::query_api::MAX_FINDINGS_LIMIT;
/// Re-export of the daemon-side cap so the `list` footer cannot drift.
/// Same name as the upstream constant in
/// [`sentinel_core::daemon::query_api`] so a `grep` finds both sides.
const MAX_ACKS_RESPONSE: usize = sentinel_core::daemon::query_api::MAX_ACKS_RESPONSE;

/// Subactions for the `perf-sentinel ack` command. Defined here rather
/// than in `main.rs` so the dispatch surface stays cohesive with the
/// HTTP plumbing in this module.
#[derive(clap::Subcommand)]
pub(crate) enum AckAction {
    /// Create a new acknowledgment for a finding signature.
    Create {
        /// Signature of the finding to acknowledge. If omitted, read from stdin.
        #[arg(short, long)]
        signature: Option<String>,
        /// Reason for the acknowledgment. Required.
        #[arg(short, long)]
        reason: String,
        /// Expiration: ISO8601 datetime ("2026-05-11T00:00:00Z") or relative
        /// duration ("7d", "24h", "30m"). If omitted, the ack never expires.
        #[arg(long, value_name = "ISO8601_OR_DURATION")]
        expires: Option<String>,
        /// Acknowledger identity. Falls back to $USER then "anonymous".
        #[arg(long)]
        by: Option<String>,
        /// Path to a file containing the daemon API key.
        #[arg(long, value_name = "PATH")]
        api_key_file: Option<PathBuf>,
    },
    /// Revoke an existing acknowledgment.
    Revoke {
        /// Signature to revoke. If omitted, read from stdin.
        #[arg(short, long)]
        signature: Option<String>,
        /// Path to a file containing the daemon API key.
        #[arg(long, value_name = "PATH")]
        api_key_file: Option<PathBuf>,
    },
    /// List active daemon acknowledgments.
    ///
    /// TOML CI acks (`.perf-sentinel-acknowledgments.toml`) are not
    /// listed here, consult the file directly.
    List {
        /// Output format.
        #[arg(long, value_enum, default_value = "text")]
        output: QueryOutputFormat,
        /// Path to a file containing the daemon API key.
        #[arg(long, value_name = "PATH")]
        api_key_file: Option<PathBuf>,
    },
}

#[derive(Serialize)]
struct AckRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    by: Option<String>,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<DateTime<Utc>>,
}

/// Mirror of the daemon's `AckEntry` for the subset of fields the CLI
/// renders. `action` and any future field are ignored on deserialize
/// (serde default behavior).
#[derive(Deserialize)]
struct AckListEntry {
    signature: String,
    by: String,
    #[serde(default)]
    reason: Option<String>,
    at: DateTime<Utc>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

/// Entry point dispatched from `main.rs`. Returns an exit code rather
/// than panicking so the caller can emit it via `std::process::exit`.
pub(crate) async fn cmd_ack(daemon_url: &str, action: AckAction) -> i32 {
    let base = match validate_url(daemon_url) {
        Ok(s) => s,
        Err(e) => {
            // Sanitize: the string echoes the env-var URL verbatim and
            // could otherwise replay terminal escapes from a hostile env.
            eprintln!("{}", sentinel_core::text_safety::sanitize_for_terminal(&e));
            return 1;
        }
    };
    match action {
        AckAction::Create {
            signature,
            reason,
            expires,
            by,
            api_key_file,
        } => {
            run_create(
                &base,
                signature,
                reason,
                expires,
                by,
                api_key_file.as_deref(),
            )
            .await
        }
        AckAction::Revoke {
            signature,
            api_key_file,
        } => run_revoke(&base, signature, api_key_file.as_deref()).await,
        AckAction::List {
            output,
            api_key_file,
        } => run_list(&base, output, api_key_file.as_deref()).await,
    }
}

async fn run_create(
    base: &str,
    signature: Option<String>,
    reason: String,
    expires: Option<String>,
    by: Option<String>,
    api_key_file: Option<&Path>,
) -> i32 {
    let signature = match resolve_signature(signature) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let expires_at = match expires.as_deref().map(parse_expires).transpose() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: invalid --expires value, {e}");
            return 1;
        }
    };
    let resolved_by = resolve_by(by);
    let api_key = match resolve_api_key(api_key_file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let body = AckRequestBody {
        by: Some(resolved_by.clone()),
        reason: reason.clone(),
        expires_at,
    };
    let payload = match serde_json::to_vec(&body) {
        Ok(v) => bytes::Bytes::from(v),
        Err(e) => {
            eprintln!("Error: cannot encode request body, {e}");
            return 1;
        }
    };

    let client = sentinel_core::http_client::build_client_with_body();
    let encoded_sig = percent_encode_signature_segment(&signature);
    let url = format!("{base}/api/findings/{encoded_sig}/ack");
    let (status, _body) = match call_with_tty_retry(
        &client,
        hyper::Method::POST,
        &url,
        api_key.as_deref(),
        payload,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            eprint_network_error(base, &e);
            return 1;
        }
    };

    finish_create(status, &signature, &resolved_by, &reason, expires_at, base)
}

fn finish_create(
    status: hyper::StatusCode,
    signature: &str,
    by: &str,
    reason: &str,
    expires_at: Option<DateTime<Utc>>,
    daemon_url: &str,
) -> i32 {
    if status.as_u16() == 201 {
        print_create_summary(signature, by, reason, expires_at);
        0
    } else {
        eprint_status_error(status, "create", signature, daemon_url);
        exit_code_for_status(status)
    }
}

async fn run_revoke(base: &str, signature: Option<String>, api_key_file: Option<&Path>) -> i32 {
    let signature = match resolve_signature(signature) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let api_key = match resolve_api_key(api_key_file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let client = sentinel_core::http_client::build_client_with_body();
    let encoded_sig = percent_encode_signature_segment(&signature);
    let url = format!("{base}/api/findings/{encoded_sig}/ack");
    let Some((status, _body)) = call_no_body_or_print_error(
        &client,
        hyper::Method::DELETE,
        &url,
        api_key.as_deref(),
        base,
    )
    .await
    else {
        return 1;
    };
    finish_revoke(status, &signature, base)
}

fn finish_revoke(status: hyper::StatusCode, signature: &str, daemon_url: &str) -> i32 {
    if status.as_u16() == 204 {
        print_revoke_summary(signature);
        0
    } else {
        eprint_status_error(status, "revoke", signature, daemon_url);
        exit_code_for_status(status)
    }
}

async fn run_list(base: &str, format: QueryOutputFormat, api_key_file: Option<&Path>) -> i32 {
    let api_key = match resolve_api_key(api_key_file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let client = sentinel_core::http_client::build_client_with_body();
    let url = format!("{base}/api/acks");

    let Some((status, body)) =
        call_no_body_or_print_error(&client, hyper::Method::GET, &url, api_key.as_deref(), base)
            .await
    else {
        return 1;
    };

    if status.as_u16() != 200 {
        eprint_status_error(status, "list", "", base);
        return exit_code_for_status(status);
    }

    match format {
        QueryOutputFormat::Json => {
            if let Err(e) = print_pretty_json(&body) {
                eprintln!("Error: cannot parse daemon response, {e}");
                return 1;
            }
        }
        QueryOutputFormat::Text => {
            let entries: Vec<AckListEntry> = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Error: cannot parse daemon response, {e}");
                    return 1;
                }
            };
            print!(
                "{}",
                format_ack_table(&entries, std::io::stdout().is_terminal())
            );
        }
    }
    0
}

/// Issue a single HTTP request to the daemon and return the status +
/// body. Shared between `cmd_ack` (CLI subcommand) and the TUI
/// `submit_ack_modal` / `refetch_acks_from_daemon` write paths so the
/// two consumers see the same timeout, user agent, and X-API-Key
/// handling.
pub(crate) async fn http_call(
    client: &sentinel_core::http_client::HttpClientWithBody,
    method: hyper::Method,
    url: &str,
    api_key: Option<&str>,
    body: bytes::Bytes,
) -> Result<(hyper::StatusCode, bytes::Bytes), sentinel_core::http_client::FetchError> {
    let uri: sentinel_core::http_client::Uri =
        url.parse().map_err(|e: hyper::http::uri::InvalidUri| {
            sentinel_core::http_client::FetchError::BodyRead(format!("invalid URL `{url}`: {e}"))
        })?;
    sentinel_core::http_client::fetch_with_body(
        client,
        method,
        &uri,
        USER_AGENT,
        REQUEST_TIMEOUT,
        api_key,
        body,
    )
    .await
}

/// Issue an HTTP call and, if the daemon answers 401 with no API key
/// configured and stdin is a TTY, prompt for one and retry once. Both
/// attempts go through the same client so we pay the TLS init cost
/// only once. Used by `run_create`, `run_revoke` and `run_list` to
/// keep the auth-prompt UX in a single place.
async fn call_with_tty_retry(
    client: &sentinel_core::http_client::HttpClientWithBody,
    method: hyper::Method,
    url: &str,
    api_key: Option<&str>,
    body: bytes::Bytes,
) -> Result<(hyper::StatusCode, bytes::Bytes), sentinel_core::http_client::FetchError> {
    let (status, response_body) =
        http_call(client, method.clone(), url, api_key, body.clone()).await?;
    // `prompt_api_key` short-circuits to `None` when stdin is not a
    // TTY, so we don't repeat the `is_terminal()` check here.
    if status.as_u16() == 401
        && api_key.is_none()
        && let Some(prompted) = prompt_api_key()
    {
        return http_call(client, method, url, Some(&prompted), body).await;
    }
    Ok((status, response_body))
}

/// Invoke `call_with_tty_retry` with an empty body and convert a
/// `FetchError` into a printed network-error message. Returns `None`
/// on failure so the caller can `let-else` the success path. Used by
/// `run_revoke` (DELETE) and `run_list` (GET), which share the same
/// error-handling shape.
async fn call_no_body_or_print_error(
    client: &sentinel_core::http_client::HttpClientWithBody,
    method: hyper::Method,
    url: &str,
    api_key: Option<&str>,
    daemon_url: &str,
) -> Option<(hyper::StatusCode, bytes::Bytes)> {
    match call_with_tty_retry(client, method, url, api_key, bytes::Bytes::new()).await {
        Ok(v) => Some(v),
        Err(e) => {
            eprint_network_error(daemon_url, &e);
            None
        }
    }
}

/// Validate and normalize a daemon URL. Rejects empty input,
/// non-http(s) schemes, missing hosts, port-without-host, embedded
/// userinfo, path components, and query strings. Trailing slashes on
/// the authority are trimmed for uniformity, the rest is preserved
/// verbatim.
pub(crate) fn validate_url(daemon_url: &str) -> Result<String, String> {
    if daemon_url.is_empty() {
        return Err(format!("Invalid daemon URL `{daemon_url}`: empty"));
    }
    // Trim trailing slashes only on the post-scheme portion so a
    // friendly `http://localhost:4318/` becomes `http://localhost:4318`
    // without also eating the `//` of the scheme separator (which
    // `trim_end_matches('/')` on the whole string would do for inputs
    // like `http://`, leaving `http:` and a misleading scheme error).
    let (scheme_part, rest) = daemon_url.split_once("://").ok_or_else(|| {
        format!("Invalid daemon URL `{daemon_url}`: scheme must be http or https")
    })?;
    let trimmed_rest = rest.trim_end_matches('/');
    if trimmed_rest.is_empty() {
        return Err(format!("Invalid daemon URL `{daemon_url}`: missing host"));
    }
    let normalized = format!("{scheme_part}://{trimmed_rest}");
    let parsed: sentinel_core::http_client::Uri = normalized
        .parse()
        .map_err(|e| format!("Invalid daemon URL `{daemon_url}`: {e}"))?;
    if !matches!(parsed.scheme_str(), Some("http" | "https")) {
        return Err(format!(
            "Invalid daemon URL `{daemon_url}`: scheme must be http or https"
        ));
    }
    // `hyper::Uri` accepts `http://:8080` (port without host) so reject
    // empty hosts explicitly. Closes the rust-reviewer's "loose hostname
    // validation" nit without pulling in `url::Url` as a dependency.
    if parsed.host().is_none_or(str::is_empty) {
        return Err(format!("Invalid daemon URL `{daemon_url}`: missing host"));
    }
    // Reject `http://user@host` style: the daemon never wants
    // credentials in the URL, and a typo here would silently send
    // `Authorization: Basic` shaped values for every request. The CLI
    // routes auth through `--api-key-file` / env var instead.
    if let Some(authority) = parsed.authority()
        && authority.as_str().contains('@')
    {
        return Err(format!(
            "Invalid daemon URL `{daemon_url}`: userinfo (user@host) is not allowed, use --api-key-file or PERF_SENTINEL_DAEMON_API_KEY for auth"
        ));
    }
    // Reject path components: the CLI builds `/api/...` URLs from the
    // base, a user-supplied path would create `https://host/v1/api/...`
    // which is almost never what the operator intends and silently
    // mismatches the daemon's route table. `parsed.path()` is `"/"` or
    // `""` for a bare authority, anything longer is a real path.
    if !matches!(parsed.path(), "" | "/") {
        return Err(format!(
            "Invalid daemon URL `{daemon_url}`: path component is not allowed, drop the suffix after the host"
        ));
    }
    // Reject query strings: the CLI appends `/api/...` to the base, so
    // any `?key=val` the user added would land in the wrong slot of the
    // resulting URL. Fragments (`#frag`) are not part of an absolute URI
    // per RFC 3986 and `hyper::Uri::parse` either fails or strips them
    // depending on the input shape, both safe outcomes.
    if parsed.query().is_some() {
        return Err(format!(
            "Invalid daemon URL `{daemon_url}`: query string is not allowed, drop the `?...` suffix"
        ));
    }
    Ok(normalized)
}

fn resolve_signature(arg: Option<String>) -> Result<String, String> {
    if let Some(s) = arg {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err("Error: --signature must not be empty".to_string());
        }
        return Ok(trimmed.to_string());
    }
    if std::io::stdin().is_terminal() {
        return Err(
            "Error: --signature is required (stdin is a TTY, cannot read signature from it)"
                .to_string(),
        );
    }
    // Cap stdin at MAX_SIGNATURE_LEN+1 so a `cat /dev/urandom` pipe
    // cannot exhaust memory; oversize input is rejected post-trim.
    let cap = sentinel_core::daemon::ack::MAX_SIGNATURE_LEN + 1;
    let mut buf = String::new();
    let stdin = std::io::stdin();
    stdin
        .lock()
        .take(cap as u64)
        .read_to_string(&mut buf)
        .map_err(|e| format!("Error: cannot read signature from stdin, {e}"))?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Err("Error: stdin contained no signature".to_string());
    }
    if trimmed.len() > sentinel_core::daemon::ack::MAX_SIGNATURE_LEN {
        return Err(format!(
            "Error: signature on stdin exceeds {}-byte cap",
            sentinel_core::daemon::ack::MAX_SIGNATURE_LEN
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn resolve_api_key(file: Option<&Path>) -> Result<Option<String>, String> {
    if let Ok(v) = std::env::var(ENV_DAEMON_API_KEY) {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
    if let Some(path) = file {
        return read_api_key_file(path).map(Some);
    }
    Ok(None)
}

pub(crate) fn read_api_key_file(path: &Path) -> Result<String, String> {
    use std::fs::OpenOptions;
    use std::io::Read as _;

    let mut opts = OpenOptions::new();
    opts.read(true);
    // Refuse to follow symlinks on Unix so an attacker who flips the
    // path target after the user typed --api-key-file cannot trick the
    // CLI into reading an unrelated secret. Mirrors the daemon's
    // O_NOFOLLOW posture in `crates/sentinel-core/src/daemon/ack.rs`.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(path).map_err(|e| {
        format!(
            "Error: cannot read --api-key-file `{}`, {e}",
            path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // Only warn on interactive runs: CI pipelines usually pass
        // `--api-key-file /etc/secrets/key` with a fixed mode and would
        // otherwise spam the build log on every invocation.
        if std::io::stderr().is_terminal()
            && let Ok(meta) = file.metadata()
            && meta.mode() & 0o077 != 0
        {
            eprintln!(
                "Warning: --api-key-file `{}` is group/world readable (mode {:o}), consider 'chmod 600'",
                path.display(),
                meta.mode() & 0o777
            );
        }
    }

    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|e| {
        format!(
            "Error: cannot read --api-key-file `{}`, {e}",
            path.display()
        )
    })?;
    let stripped = raw.trim_end_matches(['\n', '\r']).to_string();
    if stripped.is_empty() {
        return Err(format!(
            "Error: --api-key-file `{}` is empty",
            path.display()
        ));
    }
    // Embedded ASCII control characters would be rejected by hyper's
    // `HeaderValue::from_str` later with a generic build error. Catch
    // them here so the user gets an actionable message naming the file.
    if stripped.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(format!(
            "Error: --api-key-file `{}` contains control characters",
            path.display()
        ));
    }
    Ok(stripped)
}

/// 1 KiB matches the daemon-side `MAX_AUTH_HEADER_INPUT_BYTES`-style
/// caps and any realistic API key width.
const MAX_PROMPT_API_KEY_LEN: usize = 1024;

fn prompt_api_key() -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    eprintln!("Daemon requires authentication.");
    let raw = rpassword::prompt_password("API key (will not echo): ").ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_PROMPT_API_KEY_LEN {
        eprintln!("Error: API key exceeds {MAX_PROMPT_API_KEY_LEN}-byte cap");
        return None;
    }
    Some(trimmed)
}

/// Parse an `--expires` argument: ISO8601 datetime first, fall back to
/// a relative duration parsed by humantime ("7d", "24h", "30m").
pub(crate) fn parse_expires(s: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty value".to_string());
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    let dur = humantime::parse_duration(trimmed).map_err(|e| {
        format!(
            "expected ISO8601 datetime (e.g. 2026-05-11T00:00:00Z) or relative duration (e.g. 7d, 24h, 30m); got `{trimmed}` ({e})"
        )
    })?;
    let chrono_dur =
        chrono::Duration::from_std(dur).map_err(|_| "duration overflow".to_string())?;
    Utc::now()
        .checked_add_signed(chrono_dur)
        .ok_or_else(|| "duration overflows DateTime range".to_string())
}

fn resolve_by(arg: Option<String>) -> String {
    arg.filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("USER").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "anonymous".to_string())
}

fn print_create_summary(
    signature: &str,
    by: &str,
    reason: &str,
    expires_at: Option<DateTime<Utc>>,
) {
    let colors = if std::io::stdout().is_terminal() {
        ansi_colors(false)
    } else {
        no_colors()
    };
    let AnsiColors {
        bold,
        green,
        dim,
        reset,
        ..
    } = colors;
    use sentinel_core::text_safety::sanitize_for_terminal;
    println!("{bold}{green}Acknowledgment created{reset}");
    println!(
        "  {dim}Signature:{reset} {}",
        sanitize_for_terminal(signature)
    );
    println!("  {dim}By:{reset}        {}", sanitize_for_terminal(by));
    println!("  {dim}Reason:{reset}    {}", sanitize_for_terminal(reason));
    match expires_at {
        Some(dt) => {
            let delta = dt - Utc::now();
            let pretty = format_relative(delta);
            println!(
                "  {dim}Expires:{reset}   {} ({})",
                dt.format("%Y-%m-%dT%H:%M:%SZ"),
                pretty
            );
        }
        None => println!("  {dim}Expires:{reset}   never"),
    }
}

fn print_revoke_summary(signature: &str) {
    let colors = if std::io::stdout().is_terminal() {
        ansi_colors(false)
    } else {
        no_colors()
    };
    let AnsiColors {
        bold,
        green,
        dim,
        reset,
        ..
    } = colors;
    use sentinel_core::text_safety::sanitize_for_terminal;
    println!("{bold}{green}Acknowledgment revoked{reset}");
    println!(
        "  {dim}Signature:{reset} {}",
        sanitize_for_terminal(signature)
    );
}

fn print_pretty_json(body: &[u8]) -> Result<(), serde_json::Error> {
    let json: serde_json::Value = serde_json::from_slice(body)?;
    let pretty = serde_json::to_string_pretty(&json)?;
    println!("{pretty}");
    Ok(())
}

/// Render the active acks as a flat ASCII table. `colored` controls
/// whether ANSI escape sequences are emitted.
fn format_ack_table(entries: &[AckListEntry], colored: bool) -> String {
    use sentinel_core::text_safety::sanitize_for_terminal;
    use std::fmt::Write as _;

    let colors = if colored {
        ansi_colors(true)
    } else {
        no_colors()
    };
    let AnsiColors {
        bold, dim, reset, ..
    } = colors;

    let mut out = String::new();
    if entries.is_empty() {
        let _ = writeln!(out, "{dim}No active daemon acknowledgments.{reset}");
        let _ = writeln!(
            out,
            "Note: TOML CI acks are not listed, see .perf-sentinel-acknowledgments.toml"
        );
        return out;
    }

    // Materialize each row once so the column-width pass and the
    // render pass walk the same owned strings without parallel-vector
    // foot-guns (length drift, off-by-one indexing).
    struct Row {
        signature: String,
        by: String,
        at: String,
        expires: String,
        reason: String,
    }
    let rows: Vec<Row> = entries
        .iter()
        .map(|e| Row {
            signature: sanitize_for_terminal(&e.signature).into_owned(),
            by: sanitize_for_terminal(&e.by).into_owned(),
            at: e.at.format("%Y-%m-%dT%H:%MZ").to_string(),
            expires: match e.expires_at {
                Some(dt) => dt.format("%Y-%m-%dT%H:%MZ").to_string(),
                None => "never".to_string(),
            },
            reason: e
                .reason
                .as_deref()
                .map(|r| sanitize_for_terminal(r).into_owned())
                .unwrap_or_default(),
        })
        .collect();

    let sig_w = column_width("SIGNATURE", rows.iter().map(|r| r.signature.as_str()));
    let by_w = column_width("BY", rows.iter().map(|r| r.by.as_str()));
    let at_w = column_width("AT", rows.iter().map(|r| r.at.as_str()));
    let exp_w = column_width("EXPIRES_AT", rows.iter().map(|r| r.expires.as_str()));

    let _ = writeln!(
        out,
        "{bold}{:<sig_w$}  {:<by_w$}  {:<at_w$}  {:<exp_w$}  REASON{reset}",
        "SIGNATURE", "BY", "AT", "EXPIRES_AT"
    );
    for row in &rows {
        let _ = writeln!(
            out,
            "{:<sig_w$}  {:<by_w$}  {:<at_w$}  {:<exp_w$}  {}",
            row.signature, row.by, row.at, row.expires, row.reason
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{} daemon acknowledgments active (showing up to {})",
        entries.len(),
        MAX_ACKS_RESPONSE
    );
    let _ = writeln!(
        out,
        "{dim}Note: TOML CI acks are not listed, see .perf-sentinel-acknowledgments.toml{reset}"
    );
    out
}

fn column_width<'a>(header: &str, values: impl IntoIterator<Item = &'a str>) -> usize {
    // Use char count rather than byte length so multi-byte UTF-8
    // payloads (e.g. accented `reason`) line up visually. This is an
    // approximation: emoji and CJK chars have wider visual cells, but
    // the alternative crates (`unicode-width`) add a dependency for
    // ack list which is operator-only and rarely contains exotic glyphs.
    let header_w = header.chars().count();
    values
        .into_iter()
        .map(|v| v.chars().count())
        .max()
        .map_or(header_w, |max_v| max_v.max(header_w))
}

fn exit_code_for_status(status: hyper::StatusCode) -> i32 {
    match status.as_u16() {
        s if (400..500).contains(&s) => 2,
        s if (500..600).contains(&s) => 3,
        _ => 1,
    }
}

fn eprint_status_error(status: hyper::StatusCode, op: &str, signature: &str, daemon_url: &str) {
    use sentinel_core::text_safety::sanitize_for_terminal;
    let code = status.as_u16();
    // Signatures and daemon URLs may have been read from stdin/env; pipe
    // through the terminal sanitizer to avoid escape-sequence injection.
    let safe_sig = sanitize_for_terminal(signature);
    let safe_url = sanitize_for_terminal(daemon_url);
    match (code, op) {
        (401, _) => {
            eprintln!("Error: daemon requires authentication (HTTP 401)");
            eprintln!("hint: set {ENV_DAEMON_API_KEY} environment variable");
            eprintln!("hint: or use --api-key-file <path>");
        }
        (409, "create") => {
            eprintln!("Error: signature already acknowledged (HTTP 409)");
            if safe_sig.is_empty() {
                eprintln!("hint: use 'perf-sentinel ack revoke --signature <SIG>' first");
            } else {
                eprintln!("hint: use 'perf-sentinel ack revoke --signature {safe_sig}' first");
            }
        }
        (404, "revoke") => {
            eprintln!("Error: no active acknowledgment for this signature (HTTP 404)");
        }
        (400, _) => {
            eprintln!("Error: invalid signature format (HTTP 400)");
        }
        (507, "create") => {
            eprintln!("Error: daemon ack store is full (HTTP 507)");
            eprintln!("hint: revoke expired acks or increase [daemon.ack] limits");
        }
        (503, _) => {
            eprintln!("Error: daemon ack store is disabled (HTTP 503)");
            eprintln!("hint: set [daemon.ack] enabled = true in the daemon config");
        }
        _ => {
            eprintln!("Error: daemon returned HTTP {code} on {op} (daemon at {safe_url})");
        }
    }
}

fn eprint_network_error(daemon_url: &str, err: &sentinel_core::http_client::FetchError) {
    eprintln!("Error: cannot reach daemon at {daemon_url}");
    eprintln!("caused by: {err}");
    eprintln!("hint: is `perf-sentinel watch` running?");
}

/// Format a `chrono::Duration` as a coarse "in N days/hours/minutes"
/// or "expired N days ago" hint for the create summary.
fn format_relative(delta: chrono::Duration) -> String {
    let total = delta.num_seconds();
    if total <= 0 {
        return "expired".to_string();
    }
    let days = total / 86_400;
    let hours = (total % 86_400) / 3600;
    let minutes = (total % 3600) / 60;
    if days > 1 {
        format!("in {days} days")
    } else if days == 1 {
        "in 1 day".to_string()
    } else if hours >= 1 {
        format!("in {hours}h")
    } else if minutes >= 1 {
        format!("in {minutes}min")
    } else {
        "in less than a minute".to_string()
    }
}

/// Error type returned by [`post_ack_via_daemon`] and
/// [`delete_ack_via_daemon`]. The TUI maps these into modal error
/// messages (no `eprintln!` from a raw-mode terminal).
///
/// **Sanitization contract**: the `String` payloads on `Conflict`,
/// `Validation`, `Http` and `Transport` are NOT pre-sanitized for
/// terminal control sequences. Consumers that render the `Display`
/// output to a terminal MUST pipe it through
/// [`sentinel_core::text_safety::sanitize_for_terminal`] first. The
/// modal footer at `tui::render_modal_footer` does this. Any future
/// `tracing::error!` or stdout writer reusing this error must do the
/// same. The bidi/control filter on the modal input bounds what the
/// `Validation` payload can contain (the parser echoes user input),
/// but daemon-supplied bodies (`Conflict`, `Http`) are untrusted.
///
/// `Display` produces a human-readable message that NEVER includes the
/// API key, defensive against accidental leak from a future logging
/// path that might `format!` the error.
#[cfg(feature = "tui")]
#[derive(Debug)]
pub(crate) enum AckSubmitError {
    Unauthorized,
    Conflict(String),
    NotFound,
    StoreFull,
    Disabled,
    Validation(String),
    Http(hyper::StatusCode, String),
    Transport(String),
}

#[cfg(feature = "tui")]
impl std::fmt::Display for AckSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "daemon requires authentication (HTTP 401)"),
            Self::Conflict(msg) => write!(f, "conflict: {msg}"),
            Self::NotFound => write!(f, "no active acknowledgment for this signature"),
            Self::StoreFull => write!(
                f,
                "daemon ack store is full (HTTP 507), revoke expired acks or raise [daemon.ack] limits"
            ),
            Self::Disabled => write!(f, "daemon ack store is disabled"),
            Self::Validation(msg) => write!(f, "{msg}"),
            Self::Http(status, msg) => write!(f, "HTTP {} {msg}", status.as_u16()),
            Self::Transport(msg) => write!(f, "network error: {msg}"),
        }
    }
}

/// POST `/api/findings/{signature}/ack` and map status codes into
/// [`AckSubmitError`] variants. Used by the TUI submit flow, mirrors
/// the body shape of `run_create`. The signature must be the
/// already-resolved finding signature (no stdin fallback).
#[cfg(feature = "tui")]
pub(crate) async fn post_ack_via_daemon(
    daemon_url: &str,
    signature: &str,
    by: &str,
    reason: &str,
    expires_at: Option<DateTime<Utc>>,
    api_key: Option<&str>,
) -> Result<(), AckSubmitError> {
    let body = AckRequestBody {
        by: Some(by.to_string()),
        reason: reason.to_string(),
        expires_at,
    };
    // Encode failure is unreachable on the current AckRequestBody
    // shape, kept as Validation defense-in-depth in case a future
    // schema change adds a fallible field.
    let payload = serde_json::to_vec(&body)
        .map(bytes::Bytes::from)
        .map_err(|e| AckSubmitError::Validation(format!("encode body: {e}")))?;
    let client = sentinel_core::http_client::build_client_with_body();
    let encoded_sig = percent_encode_signature_segment(signature);
    let url = format!("{daemon_url}/api/findings/{encoded_sig}/ack");
    let (status, body) = http_call(&client, hyper::Method::POST, &url, api_key, payload)
        .await
        .map_err(|e| AckSubmitError::Transport(e.to_string()))?;
    // POST never produces 404 from the route table, so the `NotFound`
    // variant is reserved for DELETE. 400 surfaces an
    // invalid-signature body from the daemon, mirror the CLI's
    // dedicated 400 hint via `Validation`. 507 signals the ack store
    // cap is reached.
    match status.as_u16() {
        201 => Ok(()),
        400 => Err(AckSubmitError::Validation(format!(
            "daemon rejected request: {}",
            decode_body_message(&body)
        ))),
        401 => Err(AckSubmitError::Unauthorized),
        409 => Err(AckSubmitError::Conflict(decode_body_message(&body))),
        503 => Err(AckSubmitError::Disabled),
        507 => Err(AckSubmitError::StoreFull),
        _ => Err(AckSubmitError::Http(status, decode_body_message(&body))),
    }
}

/// DELETE `/api/findings/{signature}/ack` and map status codes into
/// [`AckSubmitError`] variants. The empty body is sent through the
/// same `http_call` helper that the create path uses, no separate
/// no-body client.
#[cfg(feature = "tui")]
pub(crate) async fn delete_ack_via_daemon(
    daemon_url: &str,
    signature: &str,
    api_key: Option<&str>,
) -> Result<(), AckSubmitError> {
    let client = sentinel_core::http_client::build_client_with_body();
    let encoded_sig = percent_encode_signature_segment(signature);
    let url = format!("{daemon_url}/api/findings/{encoded_sig}/ack");
    let (status, body) = http_call(
        &client,
        hyper::Method::DELETE,
        &url,
        api_key,
        bytes::Bytes::new(),
    )
    .await
    .map_err(|e| AckSubmitError::Transport(e.to_string()))?;
    match status.as_u16() {
        204 => Ok(()),
        // Same 400 mapping as the POST path: daemon-rejected
        // signature lands in `Validation` so the modal footer shows
        // the daemon's hint rather than the generic `HTTP 400 ...`.
        400 => Err(AckSubmitError::Validation(format!(
            "daemon rejected request: {}",
            decode_body_message(&body)
        ))),
        401 => Err(AckSubmitError::Unauthorized),
        404 => Err(AckSubmitError::NotFound),
        503 => Err(AckSubmitError::Disabled),
        _ => Err(AckSubmitError::Http(status, decode_body_message(&body))),
    }
}

#[cfg(feature = "tui")]
fn decode_body_message(body: &bytes::Bytes) -> String {
    use sentinel_core::text_safety::sanitize_for_terminal;
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let max = 200;
    let truncated = if trimmed.chars().count() > max {
        let prefix: String = trimmed.chars().take(max).collect();
        format!("{prefix}…")
    } else {
        trimmed.to_string()
    };
    // Eager sanitize: per the AckSubmitError contract, every consumer
    // must scrub before display; do it once at the source.
    sanitize_for_terminal(&truncated).into_owned()
}

/// Percent-encode a signature for safe interpolation into the daemon
/// URL path. The daemon validates the signature shape on its side and
/// rejects malformed ones with HTTP 400, so this is defense-in-depth
/// against a future daemon shipping a less strict regex or against a
/// malicious daemon returning an exotic signature in `FindingResponse`.
///
/// Encodes everything that is not in the unreserved set (RFC 3986)
/// or `:`, which is allowed in path segments. Real sentinel signatures
/// are `[A-Za-z0-9_:.-]+`, the common case probes the input and
/// returns `Cow::Borrowed` zero-allocation.
pub(crate) fn percent_encode_signature_segment(s: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    use std::fmt::Write as _;
    if s.bytes().all(is_signature_segment_safe) {
        return Cow::Borrowed(s);
    }
    // Each unsafe byte expands to 3 chars. `+ s.len() / 4` is a soft
    // upper bound for typical input where most bytes are safe and only
    // a few escape, prevents the realloc dance on heavy-escape inputs
    // without overcommitting memory on signatures with one stray byte.
    let mut out = String::with_capacity(s.len() + s.len() / 4);
    for byte in s.bytes() {
        if is_signature_segment_safe(byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    Cow::Owned(out)
}

#[inline]
fn is_signature_segment_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_signature_passes_through_safe_chars() {
        let s = "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef0123456789abcdef";
        assert_eq!(percent_encode_signature_segment(s), s);
    }

    #[test]
    fn percent_encode_signature_escapes_path_meta_chars() {
        // `?`, `#`, `/` and space must encode so they cannot break out
        // of the segment and steer the request elsewhere.
        let s = "evil/foo?bar#baz qux";
        let encoded = percent_encode_signature_segment(s);
        assert_eq!(encoded, "evil%2Ffoo%3Fbar%23baz%20qux");
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('?'));
        assert!(!encoded.contains('#'));
        assert!(!encoded.contains(' '));
    }

    #[test]
    fn percent_encode_signature_handles_utf8() {
        // Non-ASCII bytes encode as the UTF-8 byte sequence, RFC 3986.
        let s = "café";
        let encoded = percent_encode_signature_segment(s);
        assert_eq!(encoded, "caf%C3%A9");
    }

    #[test]
    fn parse_expires_iso8601_returns_absolute() {
        let dt = parse_expires("2026-05-11T00:00:00Z").expect("valid");
        assert_eq!(
            dt.format("%Y-%m-%dT%H:%MZ").to_string(),
            "2026-05-11T00:00Z"
        );
    }

    #[test]
    fn parse_expires_7d_returns_now_plus_seven_days() {
        let dt = parse_expires("7d").expect("valid");
        let delta = (dt - Utc::now()).num_seconds();
        let seven_days = 7 * 86_400_i64;
        assert!(
            (seven_days - 5..=seven_days + 5).contains(&delta),
            "delta {delta} not within 5s of {seven_days}"
        );
    }

    #[test]
    fn parse_expires_24h_relative() {
        let dt = parse_expires("24h").expect("valid");
        let delta = (dt - Utc::now()).num_seconds();
        assert!((86_395..=86_405).contains(&delta));
    }

    #[test]
    fn parse_expires_30m_relative() {
        let dt = parse_expires("30m").expect("valid");
        let delta = (dt - Utc::now()).num_seconds();
        assert!((1795..=1805).contains(&delta));
    }

    #[test]
    fn parse_expires_invalid_returns_error() {
        let err = parse_expires("not a date").unwrap_err();
        assert!(err.contains("expected ISO8601 datetime"));
        assert!(err.contains("relative duration"));
        assert!(err.contains("not a date"));
    }

    #[test]
    fn parse_expires_empty_returns_error() {
        let err = parse_expires("   ").unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_url_strips_trailing_slash() {
        let s = validate_url("http://localhost:4318/").unwrap();
        assert_eq!(s, "http://localhost:4318");
    }

    #[test]
    fn validate_url_rejects_non_http_scheme() {
        let err = validate_url("ftp://localhost:4318").unwrap_err();
        assert!(err.contains("scheme must be http or https"));
    }

    #[test]
    fn validate_url_rejects_missing_host() {
        let err = validate_url("http://").unwrap_err();
        assert!(err.contains("missing host"));
    }

    #[test]
    fn validate_url_rejects_port_without_host() {
        let err = validate_url("http://:8080").unwrap_err();
        assert!(err.contains("missing host"));
    }

    #[test]
    fn validate_url_rejects_userinfo() {
        let err = validate_url("http://alice@daemon.local").unwrap_err();
        assert!(err.contains("userinfo"));
        assert!(err.contains("--api-key-file") || err.contains("PERF_SENTINEL_DAEMON_API_KEY"));
    }

    #[test]
    fn validate_url_rejects_path_component() {
        let err = validate_url("https://api.example.com/v1/").unwrap_err();
        assert!(err.contains("path component is not allowed"));
    }

    #[test]
    fn validate_url_rejects_query_string() {
        let err = validate_url("http://localhost:4318?debug=1").unwrap_err();
        assert!(err.contains("query string is not allowed"));
    }

    #[test]
    fn validate_url_accepts_ipv6_literal() {
        let ok = validate_url("http://[::1]:8080").unwrap();
        assert_eq!(ok, "http://[::1]:8080");
    }

    #[test]
    fn validate_url_accepts_https() {
        let s = validate_url("https://daemon.example.com/").unwrap();
        assert_eq!(s, "https://daemon.example.com");
    }

    #[test]
    fn read_api_key_file_strips_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "secret123\n").unwrap();
        let key = read_api_key_file(&path).unwrap();
        assert_eq!(key, "secret123");
    }

    #[test]
    fn read_api_key_file_strips_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "secret123\r\n").unwrap();
        let key = read_api_key_file(&path).unwrap();
        assert_eq!(key, "secret123");
    }

    #[test]
    fn read_api_key_file_rejects_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "").unwrap();
        let err = read_api_key_file(&path).unwrap_err();
        assert!(err.contains("is empty"));
    }

    #[test]
    fn read_api_key_file_returns_error_on_missing_file() {
        let path = Path::new("/nonexistent/path/to/key");
        let err = read_api_key_file(path).unwrap_err();
        assert!(err.contains("cannot read"));
    }

    #[test]
    fn read_api_key_file_rejects_embedded_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "secret\nwith newline").unwrap();
        let err = read_api_key_file(&path).unwrap_err();
        assert!(err.contains("control characters"));
    }

    #[cfg(unix)]
    #[test]
    fn read_api_key_file_refuses_to_follow_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real_key");
        std::fs::write(&target, "secret\n").unwrap();
        let link = dir.path().join("link_to_key");
        symlink(&target, &link).unwrap();
        let err = read_api_key_file(&link).unwrap_err();
        assert!(err.contains("cannot read --api-key-file"));
    }

    #[test]
    fn format_ack_table_handles_empty_list() {
        let out = format_ack_table(&[], false);
        assert!(out.contains("No active daemon acknowledgments"));
        assert!(out.contains(".perf-sentinel-acknowledgments.toml"));
    }

    #[test]
    fn format_ack_table_renders_columns_and_count() {
        let entries = vec![
            AckListEntry {
                signature: "n_plus_one_sql:svc:_api:0123456789abcdef0123456789abcdef".to_string(),
                by: "alice".to_string(),
                reason: Some("deferred".to_string()),
                at: DateTime::parse_from_rfc3339("2026-05-05T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                expires_at: Some(
                    DateTime::parse_from_rfc3339("2026-05-12T13:30:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
            },
            AckListEntry {
                signature: "slow_http:other:_api:fedcba9876543210fedcba9876543210".to_string(),
                by: "bob".to_string(),
                reason: None,
                at: DateTime::parse_from_rfc3339("2026-05-04T09:15:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                expires_at: None,
            },
        ];
        let out = format_ack_table(&entries, false);
        assert!(out.contains("SIGNATURE"));
        assert!(out.contains("BY"));
        assert!(out.contains("AT"));
        assert!(out.contains("EXPIRES_AT"));
        assert!(out.contains("REASON"));
        assert!(out.contains("alice"));
        assert!(out.contains("bob"));
        assert!(out.contains("never"));
        assert!(out.contains("2026-05-12T13:30Z"));
        assert!(out.contains("2 daemon acknowledgments active (showing up to 1000)"));
    }

    #[test]
    fn exit_code_for_status_maps_4xx_to_2() {
        assert_eq!(
            exit_code_for_status(hyper::StatusCode::from_u16(401).unwrap()),
            2
        );
        assert_eq!(
            exit_code_for_status(hyper::StatusCode::from_u16(409).unwrap()),
            2
        );
        assert_eq!(
            exit_code_for_status(hyper::StatusCode::from_u16(404).unwrap()),
            2
        );
        assert_eq!(
            exit_code_for_status(hyper::StatusCode::from_u16(400).unwrap()),
            2
        );
    }

    #[test]
    fn exit_code_for_status_maps_5xx_to_3() {
        // 507 Insufficient Storage is in the 5xx range and follows the
        // canonical rule, even though the hint message points the user at
        // operator-side actions (revoke expired acks).
        assert_eq!(
            exit_code_for_status(hyper::StatusCode::from_u16(500).unwrap()),
            3
        );
        assert_eq!(
            exit_code_for_status(hyper::StatusCode::from_u16(503).unwrap()),
            3
        );
        assert_eq!(
            exit_code_for_status(hyper::StatusCode::from_u16(507).unwrap()),
            3
        );
    }

    #[test]
    fn resolve_by_uses_arg_when_present() {
        let resolved = resolve_by(Some("alice@example.com".to_string()));
        assert_eq!(resolved, "alice@example.com");
    }

    #[test]
    fn resolve_by_treats_blank_arg_as_unset() {
        // Whitespace-only arg should fall through to env or "anonymous",
        // never be returned as-is.
        let resolved = resolve_by(Some("   ".to_string()));
        assert_ne!(resolved, "   ");
    }

    #[test]
    fn format_relative_handles_days_hours_minutes() {
        assert_eq!(format_relative(chrono::Duration::days(7)), "in 7 days");
        assert_eq!(format_relative(chrono::Duration::days(1)), "in 1 day");
        assert_eq!(format_relative(chrono::Duration::hours(3)), "in 3h");
        assert_eq!(format_relative(chrono::Duration::minutes(15)), "in 15min");
        assert_eq!(format_relative(chrono::Duration::seconds(-1)), "expired");
    }
}
