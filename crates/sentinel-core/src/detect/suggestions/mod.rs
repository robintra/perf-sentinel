//! Framework-aware actionable fixes for findings.
//!
//! Enriches detected findings with a [`SuggestedFix`] when the
//! instrumentation scopes, `code_location`, SQL statement or service name
//! reveal the framework that produced the anti-pattern. Covers Java, C#, Rust,
//! Python, Go, Node.js/TypeScript, Ruby and PHP across all ten
//! protocol anti-patterns, with a per-language `*Generic` fallback when
//! no framework-specific recommendation applies. The two messaging
//! anti-patterns are keyed by broker technology instead (Kafka,
//! `RabbitMQ`, SQS, Pulsar, NATS, JMS): their remediation lives in the
//! broker client's batching API, which the application framework does
//! not name. Coverage history is in `docs/design/04-DETECTION.md`.
//!
//! Detection is cheap and deterministic: only fields already present
//! on [`Finding`] are read (no span-level access, no hot-path
//! allocations), and missing information degrades to
//! `suggested_fix = None`.

use std::sync::LazyLock;

use regex::Regex;

use serde::{Deserialize, Serialize};

use super::{Finding, FindingType};

mod fixes;

use fixes::{FIXES, MESSAGING_FIXES};

/// A framework-specific actionable fix attached to a [`Finding`].
///
/// Stable JSON shape: field names will not be renamed or removed in a
/// minor release. New optional fields may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedFix {
    /// Mirrors the parent finding's `type` in `snake_case` (e.g.
    /// `n_plus_one_sql`). Lets downstream consumers route fixes without
    /// re-reading the parent.
    pub pattern: String,
    /// Framework tag this fix applies to (e.g. `java_jpa`,
    /// `csharp_ef_core`, `rust_diesel`), or the broker technology for
    /// the messaging finding types (e.g. `kafka`, `aws_sqs`). Stable
    /// enum-like string.
    pub framework: String,
    /// Short, imperative remediation sentence.
    pub recommendation: String,
    /// Documentation URL backing the recommendation. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_url: Option<String>,
}

/// Internal framework tag, used as a lookup key for the static fixes
/// table. Kept private. The public surface is the `framework` string on
/// [`SuggestedFix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Framework {
    JavaJpa,
    JavaWebFlux,
    JavaQuarkusReactive,
    JavaQuarkus,
    JavaHelidonMp,
    JavaHelidonSe,
    JavaGeneric,
    CsharpEfCore,
    CsharpGeneric,
    PythonDjango,
    PythonSqlAlchemy,
    PythonGeneric,
    RustDiesel,
    RustSeaOrm,
    RustGeneric,
    GoGorm,
    GoGeneric,
    NodePrisma,
    NodeGeneric,
    RubyActiveRecord,
    RubyGeneric,
    PhpLaravelEloquent,
    PhpDoctrine,
    PhpGeneric,
}

impl Framework {
    const fn as_str(self) -> &'static str {
        match self {
            Self::JavaJpa => "java_jpa",
            Self::JavaWebFlux => "java_webflux",
            Self::JavaQuarkusReactive => "java_quarkus_reactive",
            Self::JavaQuarkus => "java_quarkus",
            Self::JavaHelidonMp => "java_helidon_mp",
            Self::JavaHelidonSe => "java_helidon_se",
            Self::JavaGeneric => "java_generic",
            Self::CsharpEfCore => "csharp_ef_core",
            Self::CsharpGeneric => "csharp_generic",
            Self::PythonDjango => "python_django",
            Self::PythonSqlAlchemy => "python_sqlalchemy",
            Self::PythonGeneric => "python_generic",
            Self::RustDiesel => "rust_diesel",
            Self::RustSeaOrm => "rust_sea_orm",
            Self::RustGeneric => "rust_generic",
            Self::GoGorm => "go_gorm",
            Self::GoGeneric => "go_generic",
            Self::NodePrisma => "node_prisma",
            Self::NodeGeneric => "node_generic",
            Self::RubyActiveRecord => "ruby_active_record",
            Self::RubyGeneric => "ruby_generic",
            Self::PhpLaravelEloquent => "php_laravel_eloquent",
            Self::PhpDoctrine => "php_doctrine",
            Self::PhpGeneric => "php_generic",
        }
    }

    /// The language generic a missing `(type, framework)` fix falls back to.
    const fn generic(self) -> Self {
        match self {
            Self::JavaJpa
            | Self::JavaWebFlux
            | Self::JavaQuarkusReactive
            | Self::JavaQuarkus
            | Self::JavaHelidonMp
            | Self::JavaHelidonSe
            | Self::JavaGeneric => Self::JavaGeneric,
            Self::CsharpEfCore | Self::CsharpGeneric => Self::CsharpGeneric,
            Self::PythonDjango | Self::PythonSqlAlchemy | Self::PythonGeneric => {
                Self::PythonGeneric
            }
            Self::RustDiesel | Self::RustSeaOrm | Self::RustGeneric => Self::RustGeneric,
            Self::GoGorm | Self::GoGeneric => Self::GoGeneric,
            Self::NodePrisma | Self::NodeGeneric => Self::NodeGeneric,
            Self::RubyActiveRecord | Self::RubyGeneric => Self::RubyGeneric,
            Self::PhpLaravelEloquent | Self::PhpDoctrine | Self::PhpGeneric => Self::PhpGeneric,
        }
    }
}

/// Broker technology tag for the messaging fixes table, private like
/// [`Framework`]. The axis differs because a publish anti-pattern is
/// fixed in the broker client's batching API, which the framework does
/// not name. Rationale in `docs/design/04-DETECTION.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MessagingSystem {
    Kafka,
    RabbitMq,
    AwsSqs,
    Pulsar,
    Nats,
    Jms,
}

impl MessagingSystem {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Kafka => "kafka",
            Self::RabbitMq => "rabbitmq",
            Self::AwsSqs => "aws_sqs",
            Self::Pulsar => "pulsar",
            Self::Nats => "nats",
            Self::Jms => "jms",
        }
    }

    /// Semconv `messaging.system` value to fixes-table key. Separators
    /// are folded (`.`/`-` to `_`) because emitters spell SQS both
    /// `aws_sqs` and `aws.sqs`. `activemq` maps to JMS, the API the
    /// advice targets. Unlisted systems keep the generic suggestion.
    fn from_semconv(value: &str) -> Option<Self> {
        let folded = value.to_ascii_lowercase().replace(['.', '-'], "_");
        match folded.as_str() {
            "kafka" => Some(Self::Kafka),
            "rabbitmq" => Some(Self::RabbitMq),
            "aws_sqs" | "sqs" | "amazonsqs" => Some(Self::AwsSqs),
            "pulsar" => Some(Self::Pulsar),
            "nats" => Some(Self::Nats),
            "jms" | "activemq" => Some(Self::Jms),
            _ => None,
        }
    }
}

/// Pattern for matching a hint against a namespace string.
///
/// `Substring` is segment-boundary-aware: the hint must sit between
/// segment delimiters (`.` for Java and C#, `::` for Rust).
/// `LastSegmentEndsWith` matches the suffix of the last segment only,
/// for user-code naming conventions like Spring Data's `*Repository`
/// where the framework package never appears in `code.namespace`.
#[derive(Clone, Copy)]
enum Hint {
    Substring(&'static str),
    LastSegmentEndsWith(&'static str),
}

/// Per-language detection tables: `(framework, namespace hints)`.
/// Order matters within a language: more-specific frameworks first,
/// user-code conventions and generic last. The first match wins.
/// `Substring` hints embed enough of the package path to keep false
/// positives rare (Rust hints anchor on `::` so `diesel::` does not
/// match user crates containing `diesel` in a name).
const JAVA_RULES: &[(Framework, &[Hint])] = &[
    // Helidon MP must come before Helidon SE: `io.helidon.microprofile`
    // is a sub-package of `io.helidon`, so the catch-all SE hint would
    // otherwise win on MP code.
    (
        Framework::JavaHelidonMp,
        &[Hint::Substring("io.helidon.microprofile")],
    ),
    (Framework::JavaHelidonSe, &[Hint::Substring("io.helidon")]),
    // Quarkus reactive must come before JavaQuarkus and JavaJpa: `io.quarkus.hibernate.reactive`
    // also contains `io.quarkus.hibernate.orm` ancestors and `org.hibernate.reactive` contains
    // `org.hibernate`. The catch-all `io.quarkus` belongs to non-reactive Quarkus, so reactive
    // must enumerate the explicitly reactive sub-packages.
    (
        Framework::JavaQuarkusReactive,
        &[
            Hint::Substring("io.quarkus.hibernate.reactive"),
            Hint::Substring("io.quarkus.panache.reactive"),
            Hint::Substring("io.quarkus.reactive"),
            Hint::Substring("org.hibernate.reactive"),
            Hint::Substring("io.smallrye.mutiny"),
        ],
    ),
    // Non-reactive Quarkus: ORM (Hibernate ORM under Quarkus), imperative Panache, then any
    // remaining `io.quarkus` namespace. Place AFTER reactive so reactive wins on overlap.
    (
        Framework::JavaQuarkus,
        &[
            Hint::Substring("io.quarkus.hibernate.orm"),
            Hint::Substring("io.quarkus.panache.common"),
            Hint::Substring("io.quarkus"),
        ],
    ),
    (
        Framework::JavaWebFlux,
        &[
            Hint::Substring("org.springframework.web.reactive"),
            Hint::Substring("reactor.core"),
        ],
    ),
    // JPA framework packages first, then user-code conventions. The
    // OTel Java agent often attaches `code.namespace` to the user's
    // Spring Data repository (e.g. `com.example.OrderRepository`)
    // where the framework name never appears. The suffix patterns
    // catch those cases without matching `org.hibernate` style spans
    // (handled by the substrings above) more aggressively.
    (
        Framework::JavaJpa,
        &[
            Hint::Substring("jakarta.persistence"),
            Hint::Substring("javax.persistence"),
            Hint::Substring("org.hibernate"),
            Hint::Substring("org.springframework.data.jpa"),
            Hint::LastSegmentEndsWith("Repository"),
            Hint::LastSegmentEndsWith("Repo"),
            Hint::LastSegmentEndsWith("Dao"),
        ],
    ),
];

const CSHARP_RULES: &[(Framework, &[Hint])] = &[(
    Framework::CsharpEfCore,
    &[
        Hint::Substring("Microsoft.EntityFrameworkCore"),
        Hint::Substring("Pomelo.EntityFrameworkCore"),
    ],
)];

const PYTHON_RULES: &[(Framework, &[Hint])] = &[
    (Framework::PythonDjango, &[Hint::Substring("django")]),
    (
        Framework::PythonSqlAlchemy,
        &[Hint::Substring("sqlalchemy")],
    ),
];

const RUST_RULES: &[(Framework, &[Hint])] = &[
    (Framework::RustDiesel, &[Hint::Substring("diesel::")]),
    (Framework::RustSeaOrm, &[Hint::Substring("sea_orm::")]),
];

const GO_RULES: &[(Framework, &[Hint])] = &[(Framework::GoGorm, &[Hint::Substring("gorm")])];

const JS_RULES: &[(Framework, &[Hint])] = &[(Framework::NodePrisma, &[Hint::Substring("prisma")])];

// Ruby has no reliable namespace convention (no `*Repository` suffix, no
// package path in `code.namespace`). Detection relies on the ActiveRecord
// scope and the `.rb` filepath, so there are no namespace rules.
const RUBY_RULES: &[(Framework, &[Hint])] = &[];

// PHP namespaces use `\` separators. These are the secondary signal: the
// primary one is the native OTel scope (VENDOR_SCOPE_RULES below), since the
// Eloquent SQL leaf span is PDO-scoped (`code.function.name = "PDO::query"`)
// and shadows any app namespace. Doctrine's own SQL span does carry a
// `Doctrine\DBAL\...` namespace, so the namespace hints stay useful for it.
const PHP_RULES: &[(Framework, &[Hint])] = &[
    (
        Framework::PhpLaravelEloquent,
        &[
            Hint::Substring("Illuminate\\Database\\Eloquent"),
            Hint::Substring("App\\Models"),
        ],
    ),
    (
        Framework::PhpDoctrine,
        &[
            Hint::Substring("Doctrine\\ORM"),
            Hint::Substring("Doctrine\\DBAL"),
        ],
    ),
];

/// Last-resort service-name rules. Scanned only when all `OTel`-based
/// signals (scopes, `code_location`, filepath) are absent. Only
/// framework names distinctive enough to avoid false positives in
/// arbitrary service names are included. Order: more-specific first.
const SERVICE_NAME_RULES: &[(Framework, &[&str])] = &[
    (Framework::JavaHelidonMp, &["helidon-mp", "helidon.mp"]),
    (Framework::JavaHelidonSe, &["helidon"]),
];

/// OpenTelemetry instrumentation scope rules. Agent-emitted scope
/// names (e.g. `io.opentelemetry.spring-data-3.0`) are immune to user
/// naming quirks, making this the most reliable framework signal.
/// Matched against every scope in the leaf-to-root chain.
///
/// Order matters: `hibernate-reactive` must win over `hibernate`, and
/// `quarkus` over `hibernate` (a non-reactive Quarkus app gets Quarkus
/// advice, not raw JPA). Helidon SE/MP and Rust ORMs are disambiguated
/// by namespace hints instead: the agent emits one `helidon` scope for
/// both variants, and Rust tracer names are user-defined.
const SCOPE_RULES: &[(Framework, &[&str])] = &[
    (Framework::JavaQuarkusReactive, &["hibernate-reactive"]),
    (Framework::JavaQuarkus, &["quarkus"]),
    (Framework::JavaWebFlux, &["spring-webflux", "r2dbc"]),
    (Framework::JavaJpa, &["spring-data", "hibernate"]),
    // One `helidon` scope covers both SE and MP. JAVA_RULES namespace
    // hints disambiguate when code_location is available.
    (Framework::JavaHelidonSe, &["helidon"]),
    (Framework::PythonDjango, &["django"]),
    (Framework::PythonSqlAlchemy, &["sqlalchemy"]),
    // Go and Node use ecosystem-native scope names (`gorm.io/...`,
    // `@prisma/instrumentation`) that the `scope_matches` prefixes never
    // match. They fall through to namespace hints (GO_RULES, JS_RULES)
    // and the language-from-scope-prefix fallback.
];

/// `OTel` scopes matched as exact prefixes (via `vendor_prefix_matches`)
/// for names `SCOPE_RULES` cannot express: either off-convention
/// (`io.quarkus.*`, `Microsoft.EntityFrameworkCore`, Ruby's
/// `OpenTelemetry::Instrumentation::ActiveRecord`), or convention-prefixed
/// but with a dotted multi-segment suffix (`io.opentelemetry.contrib.php.*`)
/// that `scope_matches`' single-segment needle cannot capture. Checked
/// before `SCOPE_RULES` in `detect_framework_from_scopes`. Order matters
/// within a vendor: more-specific entries first (reactive before Quarkus).
const VENDOR_SCOPE_RULES: &[(Framework, &[&str])] = &[
    // .NET: EF Core via the OTel wrapper or the raw NuGet scope
    (
        Framework::CsharpEfCore,
        &[
            "OpenTelemetry.Instrumentation.EntityFrameworkCore",
            "Microsoft.EntityFrameworkCore",
        ],
    ),
    // Quarkus: `io.quarkus.<module>`. Reactive sub-packages first so
    // they win over the catch-all `io.quarkus` entry.
    (
        Framework::JavaQuarkusReactive,
        &[
            "io.quarkus.hibernate.reactive",
            "io.quarkus.panache.reactive",
            "io.quarkus.reactive",
        ],
    ),
    (Framework::JavaQuarkus, &["io.quarkus"]),
    // Ruby: the active_record gem emits the tracer name
    // `OpenTelemetry::Instrumentation::ActiveRecord` (`::` separators, not
    // the lowercase OTel convention), so it needs a vendor rule. Exact
    // match via `vendor_prefix_matches` (`len == prefix.len()` branch).
    (
        Framework::RubyActiveRecord,
        &["OpenTelemetry::Instrumentation::ActiveRecord"],
    ),
    // PHP native OTel instrumentations (opentelemetry-php-contrib). The
    // Doctrine scope is DB-specific (only on DBAL ops), so it tags only DB
    // findings. The Laravel scope is app-wide (it hooks HTTP Kernel, Console,
    // Queue and Eloquent Model), so it appears on every Laravel finding.
    // PhpLaravelEloquent therefore carries fixes for all ten anti-patterns
    // while PhpDoctrine only carries the SQL ones.
    (
        Framework::PhpDoctrine,
        &["io.opentelemetry.contrib.php.doctrine"],
    ),
    (
        Framework::PhpLaravelEloquent,
        &["io.opentelemetry.contrib.php.laravel"],
    ),
];

/// Segment-boundary prefix match for vendor scopes. The prefix must
/// end at a `.` boundary or consume the entire scope string.
fn vendor_prefix_matches(scope: &str, prefix: &str) -> bool {
    scope.starts_with(prefix)
        && (scope.len() == prefix.len() || scope.as_bytes()[prefix.len()] == b'.')
}

/// Last-resort framework detection from the service name. Only reached
/// when all OTel-based signal paths return `None`.
fn detect_framework_from_service_name(service: &str) -> Option<Framework> {
    let lower = service.to_ascii_lowercase();
    for (framework, needles) in SERVICE_NAME_RULES {
        if needles.iter().any(|n| lower.contains(n)) {
            return Some(*framework);
        }
    }
    None
}

/// Match any scope in the chain against any rule. Returns the first
/// rule's framework whose substring list intersects the scope chain.
fn detect_framework_from_scopes(scopes: &[String]) -> Option<Framework> {
    // Vendor-specific scopes (not io.opentelemetry.* convention)
    for (framework, prefixes) in VENDOR_SCOPE_RULES {
        if scopes
            .iter()
            .any(|scope| prefixes.iter().any(|p| vendor_prefix_matches(scope, p)))
        {
            return Some(*framework);
        }
    }
    // Standard OTel convention scopes
    for (framework, needles) in SCOPE_RULES {
        if scopes
            .iter()
            .any(|scope| needles.iter().any(|needle| scope_matches(scope, needle)))
        {
            return Some(*framework);
        }
    }
    None
}

/// Boundary-aware match against an OpenTelemetry scope name.
///
/// Only the canonical SDK prefixes match (Java `io.opentelemetry.`,
/// Python `opentelemetry.instrumentation.`, Node
/// `@opentelemetry/instrumentation-`), with optional version (`-3.0`)
/// or sub-scope (`-client`) suffix. Rejects third-party tracer names
/// that merely contain a needle (e.g. `com.acme.quarkus-monitoring`).
fn scope_matches(scope: &str, needle: &str) -> bool {
    let Some(rest) = scope
        .strip_prefix("io.opentelemetry.")
        .or_else(|| scope.strip_prefix("opentelemetry.instrumentation."))
        .or_else(|| scope.strip_prefix("@opentelemetry/instrumentation-"))
    else {
        return false;
    };
    let Some(after) = rest.strip_prefix(needle) else {
        return false;
    };
    // The needle must end at a segment boundary (end of string or `-`),
    // rejecting partial-segment matches. The `-` boundary would
    // false-positive on Node package names (`pg` vs `...-pg-pool`), so
    // Go/Node are excluded from SCOPE_RULES.
    after.is_empty() || after.starts_with('-')
}

#[derive(Debug, Clone, Copy)]
enum Language {
    Java,
    Csharp,
    Python,
    Rust,
    Go,
    JavaScript,
    Ruby,
    Php,
}

impl Language {
    const fn rules(self) -> &'static [(Framework, &'static [Hint])] {
        match self {
            Self::Java => JAVA_RULES,
            Self::Csharp => CSHARP_RULES,
            Self::Python => PYTHON_RULES,
            Self::Rust => RUST_RULES,
            Self::Go => GO_RULES,
            Self::JavaScript => JS_RULES,
            Self::Ruby => RUBY_RULES,
            Self::Php => PHP_RULES,
        }
    }

    const fn generic(self) -> Framework {
        match self {
            Self::Java => Framework::JavaGeneric,
            Self::Csharp => Framework::CsharpGeneric,
            Self::Python => Framework::PythonGeneric,
            Self::Rust => Framework::RustGeneric,
            Self::Go => Framework::GoGeneric,
            Self::JavaScript => Framework::NodeGeneric,
            Self::Ruby => Framework::RubyGeneric,
            Self::Php => Framework::PhpGeneric,
        }
    }
}

fn language_from_filepath(fp: &str) -> Option<Language> {
    let ext = std::path::Path::new(fp).extension()?;
    if ext.eq_ignore_ascii_case("java") {
        Some(Language::Java)
    } else if ext.eq_ignore_ascii_case("cs") {
        Some(Language::Csharp)
    } else if ext.eq_ignore_ascii_case("py") {
        Some(Language::Python)
    } else if ext.eq_ignore_ascii_case("rs") {
        Some(Language::Rust)
    } else if ext.eq_ignore_ascii_case("go") {
        Some(Language::Go)
    } else if ext.eq_ignore_ascii_case("rb") {
        Some(Language::Ruby)
    } else if ext.eq_ignore_ascii_case("php") {
        Some(Language::Php)
    } else if ext.eq_ignore_ascii_case("js")
        || ext.eq_ignore_ascii_case("ts")
        || ext.eq_ignore_ascii_case("jsx")
        || ext.eq_ignore_ascii_case("tsx")
        || ext.eq_ignore_ascii_case("mjs")
        || ext.eq_ignore_ascii_case("mts")
        || ext.eq_ignore_ascii_case("cjs")
        || ext.eq_ignore_ascii_case("cts")
    {
        Some(Language::JavaScript)
    } else {
        None
    }
}

/// Enrich findings in place with a [`SuggestedFix`] when the framework
/// can be inferred and a mapping exists. No-op for findings where the
/// framework is unknown or the lookup misses.
///
/// Called by [`super::detect`] after the per-trace detectors have run,
/// and by [`super::slow::build_cross_trace_finding`] for the batch and
/// daemon cross-trace slow findings.
pub(crate) fn enrich(findings: &mut [Finding]) {
    for finding in findings.iter_mut() {
        if let Some(fix) = lookup_fix(finding) {
            finding.suggested_fix = Some(fix.clone());
        }
    }
}

fn lookup_fix(finding: &Finding) -> Option<&'static SuggestedFix> {
    match finding.finding_type {
        FindingType::NPlusOneMessaging | FindingType::SlowMessaging => {
            let system = messaging_system_of(finding)?;
            MESSAGING_FIXES.get(&(finding.finding_type.clone(), system))
        }
        _ => {
            let framework = detect_framework(finding)?;
            FIXES
                .get(&(finding.finding_type.clone(), framework))
                .or_else(|| FIXES.get(&(finding.finding_type.clone(), framework.generic())))
        }
    }
}

/// The broker behind a messaging finding, from the first token of the
/// template (`normalize` builds it as `{operation} {target}`).
///
/// Only the OTLP path puts `messaging.system` in `operation`. A
/// hand-written JSON input can put anything there, so an unknown token
/// degrades to the generic suggestion.
fn messaging_system_of(finding: &Finding) -> Option<MessagingSystem> {
    let system = finding.pattern.template.split_whitespace().next()?;
    MessagingSystem::from_semconv(system)
}

/// Pure framework detector. Inspects five signals in order, most
/// reliable first (full rationale in `docs/design/04-DETECTION.md`):
///
/// 1. Instrumentation scope chain (agent-emitted, naming-quirk-immune).
/// 2. Language from ecosystem-native scope prefix, then namespace rules
///    or the language-generic fallback.
/// 3. `code_location` namespace with filepath-derived language, falling
///    back to the language generic.
/// 4. `code_location` namespace alone: first hit across all languages,
///    no generic fallback (language unknown).
/// 5. Service name substrings, lowest confidence.
///
/// Where 2 and 3 fall back to the Java generic, a SELECT Hibernate
/// generated still yields `JavaJpa` (see [`language_fallback`]).
///
/// `None` when no signal is available.
fn detect_framework(finding: &Finding) -> Option<Framework> {
    if let Some(framework) = detect_framework_from_scopes(&finding.instrumentation_scopes) {
        return Some(framework);
    }
    if let Some(language) = language_from_scope_prefix(&finding.instrumentation_scopes) {
        let ns = finding
            .code_location
            .as_ref()
            .and_then(|loc| loc.namespace.as_deref())
            .unwrap_or("");
        // A scope names the language only, so a service-name framework of
        // that language still beats its generic.
        return Some(
            match_namespace_against_language(ns, language)
                .or_else(|| {
                    detect_framework_from_service_name(&finding.service)
                        .filter(|fw| fw.generic() == language.generic())
                })
                .unwrap_or_else(|| language_fallback(finding, language)),
        );
    }
    if let Some(loc) = finding.code_location.as_ref() {
        let ns = loc.namespace.as_deref().unwrap_or("");
        if let Some(language) = loc.filepath.as_deref().and_then(language_from_filepath) {
            return Some(
                match_namespace_against_language(ns, language)
                    .unwrap_or_else(|| language_fallback(finding, language)),
            );
        }
        if let Some(fw) = (!ns.is_empty())
            .then(|| {
                // `\` is exclusive to PHP namespaces. Gate on it so the
                // dot/colon languages' separator-agnostic suffix rules
                // (Java's `*Repository`) never claim a PHP namespace, and
                // PHP's `\`-anchored hints never claim a Java/etc one.
                let languages: &[Language] = if ns.contains('\\') {
                    &[Language::Php]
                } else {
                    &[
                        Language::Java,
                        Language::Csharp,
                        Language::Python,
                        Language::Rust,
                        Language::Go,
                        Language::JavaScript,
                    ]
                };
                languages
                    .iter()
                    .copied()
                    .find_map(|language| match_namespace_against_language(ns, language))
            })
            .flatten()
        {
            return Some(fw);
        }
    }
    detect_framework_from_service_name(&finding.service)
}

/// Hibernate 6 and later alias each table of a query it generates as
/// `<stem><n>_<m>` (`select d1_0.id from dossier d1_0`).
static HIBERNATE_ALIAS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([a-z]+\d+_\d+)\.").expect("static regex"));

/// A column qualified by a Hibernate-shaped alias that the query also
/// declares after its table (`from dossier d1_0`). The declaration rules
/// out a schema or table merely named that way (`tenant1_0.users`), which
/// never stands alone.
fn has_hibernate_alias(template: &str) -> bool {
    HIBERNATE_ALIAS_RE.captures_iter(template).any(|caps| {
        let alias = &caps[1];
        template.match_indices(alias).any(|(at, _)| {
            let declared_after_table = template[..at].ends_with(char::is_whitespace);
            let end = template[at + alias.len()..].chars().next();
            declared_after_table && end.is_none_or(|c| c.is_whitespace() || matches!(c, ',' | ')'))
        })
    })
}

/// The generic of `language`, unless the statement itself names the ORM.
/// A span may name neither Hibernate nor the repository that called it
/// (Micrometer Observation spans never do, the Java agent's JDBC span does
/// not when no Hibernate span wraps it), yet Hibernate still signs the SQL
/// it generated with its aliases.
///
/// Only a SELECT qualifies: the JPA fixes are about fetching, and a bulk
/// UPDATE or DELETE carries the same aliases. Hibernate Reactive generates
/// them too, over the Vert.x SQL client, where the blocking JPA advice
/// does not apply.
fn language_fallback(finding: &Finding, language: Language) -> Framework {
    let template = finding.pattern.template.trim_start();
    // `hibernate.use_sql_comments` opens the statement with a block comment.
    let mut statement = template;
    while let Some(rest) = statement.strip_prefix("/*") {
        statement = rest
            .split_once("*/")
            .map_or("", |(_, after)| after)
            .trim_start();
    }
    let is_select = statement
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("select"));
    let reactive = finding
        .instrumentation_scopes
        .iter()
        .any(|scope| scope.contains("vertx-sql-client"));
    if matches!(language, Language::Java) && is_select && !reactive && has_hibernate_alias(template)
    {
        return Framework::JavaJpa;
    }
    language.generic()
}

/// Deduce the language from ecosystem-native scope prefixes that
/// `SCOPE_RULES` cannot handle: `github.com/` (Go module path),
/// `@opentelemetry/instrumentation-` or `@prisma/` (npm),
/// `Microsoft.EntityFrameworkCore` / `OpenTelemetry.Instrumentation.*`
/// (`NuGet`), `OpenTelemetry::Instrumentation::` (Ruby gem),
/// `io.opentelemetry.contrib.php.` (PHP), then any other
/// `io.opentelemetry.` scope (Java agent) and `org.springframework`
/// (Spring Boot through Micrometer Observation). Lower confidence than
/// `SCOPE_RULES`, fires only on prefixes that unambiguously identify the
/// language. Python's `opentelemetry.instrumentation.` is not claimed.
/// Rust tracer names have no usable prefix.
fn language_from_scope_prefix(scopes: &[String]) -> Option<Language> {
    for scope in scopes {
        if scope.starts_with("github.com/") {
            return Some(Language::Go);
        }
        if scope.starts_with("@opentelemetry/instrumentation-")
            || scope.starts_with("@prisma/")
            || scope.starts_with("@nestjs/")
        {
            return Some(Language::JavaScript);
        }
        // VENDOR_SCOPE_RULES catches these for CsharpEfCore. This arm
        // is the fallback that routes other .NET scopes to CsharpGeneric.
        if scope == "Microsoft.EntityFrameworkCore"
            || scope.starts_with("Microsoft.EntityFrameworkCore.")
            || scope.starts_with("OpenTelemetry.Instrumentation.")
        {
            return Some(Language::Csharp);
        }
        // Ruby gems emit `OpenTelemetry::Instrumentation::<Lib>` (`::`).
        // ActiveRecord is caught earlier by VENDOR_SCOPE_RULES. This routes
        // the other Ruby scopes (pg/mysql2 drivers, Rack) to RubyGeneric.
        if scope.starts_with("OpenTelemetry::Instrumentation::") {
            return Some(Language::Ruby);
        }
        // PHP native OTel scopes are `io.opentelemetry.contrib.php.<lib>`.
        // Laravel/Doctrine are caught earlier by VENDOR_SCOPE_RULES. This
        // routes the rest (pdo, mongodb, curl, guzzle, ...) to PhpGeneric.
        if scope.starts_with("io.opentelemetry.contrib.php.") {
            return Some(Language::Php);
        }
        // Java agent scopes are `io.opentelemetry.<library>` (`jdbc`,
        // `apache-httpclient-5.0`). SCOPE_RULES catch the framework ones first.
        if scope.starts_with("io.opentelemetry.") {
            return Some(Language::Java);
        }
        // Spring Boot names its Micrometer Observation tracer
        // `org.springframework.boot` and puts every span under it, with no
        // `code.namespace`: the language is all these spans reveal.
        if vendor_prefix_matches(scope, "org.springframework") {
            return Some(Language::Java);
        }
    }
    None
}

/// Try each rule of `language` against `ns`. Returns the first matching
/// framework, or `None` when no rule matches.
fn match_namespace_against_language(ns: &str, language: Language) -> Option<Framework> {
    for (framework, hints) in language.rules() {
        if hints.iter().any(|hint| hint_matches(ns, *hint)) {
            return Some(*framework);
        }
    }
    None
}

/// Dispatch a hint against the namespace.
fn hint_matches(ns: &str, hint: Hint) -> bool {
    match hint {
        Hint::Substring(needle) => namespace_contains_segment(ns, needle),
        Hint::LastSegmentEndsWith(suffix) => last_segment(ns).ends_with(suffix),
    }
}

/// Last segment of a `.` or `::` separated namespace. Empty for an
/// empty input, the whole string when no separator is present.
fn last_segment(ns: &str) -> &str {
    let last_dot = ns.rfind('.').map(|i| i + 1);
    let last_colon = ns.rfind("::").map(|i| i + 2);
    match (last_dot, last_colon) {
        (Some(a), Some(b)) => &ns[a.max(b)..],
        (Some(a), None) => &ns[a..],
        (None, Some(b)) => &ns[b..],
        (None, None) => ns,
    }
}

/// Segment-boundary-aware substring match: `hint` must start at `ns`
/// start or right after a `.`/`::`/`\` delimiter, and end at `ns` end or
/// right before another delimiter. Rejects `orders::mydiesel::query`
/// for `diesel::` (leading) and `io.helidongrpc.Foo` for `io.helidon`
/// (trailing). The `\` arm handles PHP namespaces (`Doctrine\ORM`). It
/// is inert for every other language, since `\` never appears in their
/// namespace strings.
///
/// `start` advances by `hint.len()` after a miss: skips overlapping
/// re-scans and always lands on a `char` boundary (`str::find` returns
/// match-aligned indices).
fn namespace_contains_segment(ns: &str, hint: &str) -> bool {
    let bytes = ns.as_bytes();
    let mut start = 0;
    while let Some(found) = ns[start..].find(hint) {
        let abs = start + found;
        let end = abs + hint.len();

        let leading_ok = abs == 0
            || bytes[abs - 1] == b'.'
            || bytes[abs - 1] == b'\\'
            // Rust `::`: the byte preceding the hint is `:` and the one
            // before that is also `:`.
            || (bytes[abs - 1] == b':' && abs >= 2 && bytes[abs - 2] == b':');

        // Trailing boundary: either the hint already ended at a
        // separator (e.g. Rust `diesel::`), or the next byte starts a
        // new segment. Without this, `io.helidon` would match
        // `io.helidongrpc.Foo`.
        let trailing_ok = end == ns.len()
            || bytes[end - 1] == b':'
            || bytes[end] == b'.'
            || bytes[end] == b'\\'
            || (bytes[end] == b':' && end + 1 < ns.len() && bytes[end + 1] == b':');

        if leading_ok && trailing_ok {
            return true;
        }
        start = end;
    }
    false
}

#[cfg(test)]
mod tests;
