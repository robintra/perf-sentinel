//! Bounded `reason` label enums for the daemon Prometheus counters.

/// Reason an OTLP request was rejected by the daemon.
///
/// Used as the `reason` label of `perf_sentinel_otlp_rejected_total`.
/// Variants are pre-warmed to 0 at startup so dashboards can plot
/// zero-values before any rejection occurs.
///
/// There is no `payload_too_large` variant: tower-http's
/// `RequestBodyLimitLayer` (HTTP) and tonic's `max_decoding_message_size`
/// (gRPC) reject oversized payloads before the application handler
/// runs. Operators concerned with payload size should monitor the
/// upstream proxy or wire a tower-http rejection counter in their
/// own stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtlpRejectReason {
    /// HTTP only: Content-Type is not `application/x-protobuf`.
    UnsupportedMediaType,
    /// HTTP only: protobuf decode failed.
    ParseError,
    /// HTTP and gRPC: the event channel is saturated or closed.
    ChannelFull,
    /// HTTP and gRPC: cgroup memory crossed the configured high-water
    /// mark, so ingest is rejected to bound RSS (memory admission control).
    MemoryPressure,
}

impl OtlpRejectReason {
    /// Every variant in declaration order. Fixed-size array so adding a
    /// variant without bumping the count is a compile-time error,
    /// keeping the pre-warm loop in `MetricsState::new` exhaustive.
    pub const ALL: [Self; 4] = [
        Self::UnsupportedMediaType,
        Self::ParseError,
        Self::ChannelFull,
        Self::MemoryPressure,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::ParseError => "parse_error",
            Self::ChannelFull => "channel_full",
            Self::MemoryPressure => "memory_pressure",
        }
    }
}

/// Reason a per-window report archive entry was dropped instead of
/// written, the `reason` label of
/// `perf_sentinel_archive_windows_dropped_total`. The archive chain
/// (`daemon/archive.rs`) stays contiguous across a drop, so this counter
/// and the paired warn log are the only record of the loss.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveDropReason {
    /// The bounded writer channel was full (writer behind on disk I/O
    /// or sustained window pressure).
    ChannelFull,
    /// The writer task has already exited, so the channel is closed.
    WriterExited,
    /// Serializing the window envelope failed.
    SerializeError,
    /// Writing the line to the archive file failed.
    WriteError,
}

impl ArchiveDropReason {
    /// Every variant in declaration order, driving the pre-warm loop in
    /// `MetricsState::new`. Nothing forces a new variant into this array
    /// (`as_str`'s exhaustive match is the compile-time reminder that it
    /// exists), so keep it in sync or the new reason skips pre-warming.
    pub const ALL: [Self; 4] = [
        Self::ChannelFull,
        Self::WriterExited,
        Self::SerializeError,
        Self::WriteError,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChannelFull => "channel_full",
            Self::WriterExited => "writer_exited",
            Self::SerializeError => "serialize_error",
            Self::WriteError => "write_error",
        }
    }
}

/// Reason an OTLP span was skipped by conversion instead of becoming a
/// `SpanEvent`.
///
/// Used as the `reason` label of `perf_sentinel_otlp_spans_filtered_total`.
/// Variants are pre-warmed to 0 at startup. Span-level filtering is
/// expected because only SQL and client-side outbound spans are
/// analyzable (see docs/LIMITATIONS.md). The counter exists so a fleet
/// whose spans all filter out is visible instead of silently yielding no
/// findings while every OTLP request keeps returning success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtlpSpanFilterReason {
    /// Span carries no `db.*` statement and no HTTP url or method, or is a
    /// SERVER span whose URL describes its own inbound request.
    NotIo,
    /// Span has `db.system` but no `db.statement`/`db.query.text` to analyze.
    MissingDbStatement,
    /// Non-SERVER span with an HTTP method but no `http.url`/`url.full`.
    MissingHttpUrl,
    /// Span names a non-SQL datastore in `db.system` (Redis, `MongoDB`, ...).
    /// An expected drop, not an instrumentation gap, so it is excluded
    /// from the daemon zero-retention warning.
    NonSqlDatastore,
    /// DB span merged into the single event of a query that layered
    /// instrumentation split across spans (statement on one, duration on
    /// another, e.g. PHP Doctrine + PDO). The query is still analyzed, so
    /// this is excluded from the daemon zero-retention warning.
    MergedDbSpan,
}

impl OtlpSpanFilterReason {
    /// Every variant in declaration order. Fixed-size array so adding a
    /// variant without bumping the count is a compile-time error,
    /// keeping the pre-warm loop in `MetricsState::new` exhaustive.
    pub const ALL: [Self; 5] = [
        Self::NotIo,
        Self::MissingDbStatement,
        Self::MissingHttpUrl,
        Self::NonSqlDatastore,
        Self::MergedDbSpan,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotIo => "not_io",
            Self::MissingDbStatement => "missing_db_statement",
            Self::MissingHttpUrl => "missing_http_url",
            Self::NonSqlDatastore => "non_sql_datastore",
            Self::MergedDbSpan => "merged_db_span",
        }
    }
}

/// Reason a daemon ack or unack operation failed.
///
/// Used as the `reason` label of
/// `perf_sentinel_ack_operations_failed_total`. Documented combinations
/// with `AckAction` are pre-warmed to 0 at startup so dashboards can
/// plot zero-values before the first failure occurs.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AckFailureReason {
    /// HTTP 409, action=ack only: signature is already acked, either by
    /// the daemon JSONL or by an active CI TOML baseline.
    AlreadyAcked,
    /// HTTP 404, action=unack only: signature has no active daemon ack.
    NotAcked,
    /// HTTP 401: `[daemon.ack] api_key` is set, request missing or
    /// has an invalid `X-API-Key` header.
    Unauthorized,
    /// HTTP 503: ack store disabled (`enabled = false`, or default
    /// storage path could not be resolved at startup).
    NoStore,
    /// HTTP 400: `{signature}` path segment fails canonical format
    /// validation.
    InvalidSignature,
    /// HTTP 507, action=ack only: `MAX_ACTIVE_ACKS` reached.
    LimitReached,
    /// HTTP 507, action=ack only: append would push the JSONL above
    /// `MAX_ACKS_FILE_BYTES` (per-daemon saturation, indicates
    /// compaction is needed at next restart or the cap should be
    /// raised).
    FileTooLarge,
    /// HTTP 507, action=ack only: a single record exceeds
    /// `MAX_ACK_ENTRY_BYTES` after serialization, typically because
    /// the caller-supplied `by` or `reason` field is oversized
    /// (per-request misuse, indicates client-side validation should
    /// be tightened).
    EntryTooLarge,
    /// HTTP 500: IO failure, serialization error, symlink refused,
    /// insecure permissions, or no default storage location at write
    /// time. Also absorbs `AckError::FileTooLarge` and
    /// `AckError::EntryTooLarge` on the unack path: the unack flow
    /// surfaces those two cases under `internal_error` with HTTP 500
    /// rather than HTTP 507, since the ack endpoints do not
    /// differentiate the cap on the unack write today.
    InternalError,
}

#[cfg(feature = "daemon")]
impl AckFailureReason {
    /// Stable Prometheus label string for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyAcked => "already_acked",
            Self::NotAcked => "not_acked",
            Self::Unauthorized => "unauthorized",
            Self::NoStore => "no_store",
            Self::InvalidSignature => "invalid_signature",
            Self::LimitReached => "limit_reached",
            Self::FileTooLarge => "file_too_large",
            Self::EntryTooLarge => "entry_too_large",
            Self::InternalError => "internal_error",
        }
    }
}

/// `reason` label of `perf_sentinel_incidents_rejected_total`. Bounded
/// by construction: a delivery or an alert is refused for one of these
/// and nothing the caller sends reaches the label. Pre-warmed at zero.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncidentRejection {
    /// HTTP 401 on `POST` or `GET`: missing or wrong `X-API-Key`.
    Unauthorized,
    /// One alert without the configured service label.
    NoService,
    /// One alert whose `startsAt` is not RFC 3339.
    UnparsableTime,
    /// One alert past the per-delivery cap.
    Overflow,
}

#[cfg(feature = "daemon")]
impl IncidentRejection {
    /// Every variant, so the pre-warm loop cannot miss one.
    pub const ALL: [Self; 4] = [
        Self::Unauthorized,
        Self::NoService,
        Self::UnparsableTime,
        Self::Overflow,
    ];

    /// Stable Prometheus label string for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::NoService => "no_service",
            Self::UnparsableTime => "unparsable_time",
            Self::Overflow => "overflow",
        }
    }
}

/// `reason` label of `perf_sentinel_scaphandre_scrape_failed_total`.
/// Pre-warmed to 0 at startup. No dedicated `invalid_uri` variant:
/// `ScraperError::InvalidUri` folds into `RequestError` since
/// production aborts before reaching the counter path.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScaphandreScrapeReason {
    /// `FetchError::Transport`: endpoint unreachable from the daemon.
    Unreachable,
    /// `FetchError::Timeout`: 3-second deadline on `fetch_metrics_once` elapsed.
    Timeout,
    /// `FetchError::HttpStatus`: endpoint replied with non-2xx.
    HttpError,
    /// `FetchError::BodyRead`: transport error while streaming the body.
    BodyReadError,
    /// `FetchError::RequestBuild` or `ScraperError::InvalidUri`: configuration edge case.
    RequestError,
    /// `ScraperError::Utf8`: body was not valid UTF-8 (likely not a Scaphandre endpoint).
    InvalidUtf8,
}

#[cfg(feature = "daemon")]
impl ScaphandreScrapeReason {
    /// Every variant in declaration order. Fixed-size array so adding a
    /// variant without bumping the count is a compile-time error.
    pub(crate) const ALL: [Self; 6] = [
        Self::Unreachable,
        Self::Timeout,
        Self::HttpError,
        Self::BodyReadError,
        Self::RequestError,
        Self::InvalidUtf8,
    ];

    /// Stable Prometheus label string for this variant.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Timeout => "timeout",
            Self::HttpError => "http_error",
            Self::BodyReadError => "body_read_error",
            Self::RequestError => "request_error",
            Self::InvalidUtf8 => "invalid_utf8",
        }
    }
}

/// `reason` label of `perf_sentinel_kepler_scrape_failed_total`. Aliased
/// to [`ScaphandreScrapeReason`] because Kepler reuses the same six HTTP
/// failure modes verbatim, so dashboards can build a single panel that
/// union-rates both sources.
#[cfg(feature = "daemon")]
pub(crate) type KeplerScrapeReason = ScaphandreScrapeReason;

/// `reason` label of `perf_sentinel_alumet_scrape_failed_total`. Aliased
/// to [`ScaphandreScrapeReason`] for the same reason as Kepler: Alumet is
/// scraped over plain HTTP with the identical six failure modes.
#[cfg(feature = "daemon")]
pub(crate) type AlumetScrapeReason = ScaphandreScrapeReason;

/// `reason` label of `perf_sentinel_redfish_scrape_failed_total`. Adds
/// three Redfish-specific variants on top of the shared HTTP set:
/// `InvalidJson`, `PathMissing`, and `InvalidValue` cover the BMC
/// vendor-variance failure modes.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RedfishScrapeReason {
    Unreachable,
    Timeout,
    HttpError,
    BodyReadError,
    RequestError,
    InvalidUtf8,
    InvalidJson,
    PathMissing,
    InvalidValue,
}

#[cfg(feature = "daemon")]
impl RedfishScrapeReason {
    pub(crate) const ALL: [Self; 9] = [
        Self::Unreachable,
        Self::Timeout,
        Self::HttpError,
        Self::BodyReadError,
        Self::RequestError,
        Self::InvalidUtf8,
        Self::InvalidJson,
        Self::PathMissing,
        Self::InvalidValue,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Timeout => "timeout",
            Self::HttpError => "http_error",
            Self::BodyReadError => "body_read_error",
            Self::RequestError => "request_error",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::InvalidJson => "invalid_json",
            Self::PathMissing => "path_missing",
            Self::InvalidValue => "invalid_value",
        }
    }
}
