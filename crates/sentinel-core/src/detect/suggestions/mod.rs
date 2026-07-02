//! Framework-aware actionable fixes for findings.
//!
//! Enriches detected findings with a [`SuggestedFix`] when the
//! instrumentation scopes, `code_location` or service name reveal the
//! framework that produced the anti-pattern. Covers Java, C#, Rust,
//! Python, Go, Node.js/TypeScript, Ruby and PHP across all ten
//! anti-patterns, with a per-language `*Generic` fallback when no
//! framework-specific recommendation applies. Coverage history is in
//! `docs/design/04-DETECTION.md`.
//!
//! Detection is cheap and deterministic: only fields already present
//! on [`Finding`] are read (no span-level access, no hot-path
//! allocations), and missing information degrades to
//! `suggested_fix = None`.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::{Finding, FindingType};

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
    /// `csharp_ef_core`, `rust_diesel`). Stable enum-like string.
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
/// user-code conventions and generic last; the first match wins.
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
    // where the framework name never appears; the suffix patterns
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
// package path in `code.namespace`); detection relies on the ActiveRecord
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
    // One `helidon` scope covers both SE and MP; JAVA_RULES namespace
    // hints disambiguate when code_location is available.
    (Framework::JavaHelidonSe, &["helidon"]),
    (Framework::PythonDjango, &["django"]),
    (Framework::PythonSqlAlchemy, &["sqlalchemy"]),
    // Go and Node use ecosystem-native scope names (`gorm.io/...`,
    // `@prisma/instrumentation`) that the `scope_matches` prefixes never
    // match; they fall through to namespace hints (GO_RULES, JS_RULES)
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
    // Queue and Eloquent Model), so it rides every Laravel finding, which is
    // why PhpLaravelEloquent carries fixes for all ten anti-patterns while
    // PhpDoctrine only carries the SQL ones.
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
    // false-positive on Node package names (`pg` vs `...-pg-pool`),
    // which is why Go/Node are excluded from SCOPE_RULES.
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

/// Static mapping of `(finding_type, framework)` to a fix template.
///
/// Lookups missing from the table return `None` and the finding's
/// `suggested_fix` field stays `None`. This is the extension point for
/// future framework support: add entries here, no other wiring required.
static FIXES: LazyLock<HashMap<(FindingType, Framework), SuggestedFix>> = LazyLock::new(|| {
    use FindingType::{
        ChattyService, ExcessiveFanout, NPlusOneHttp, NPlusOneSql, PoolSaturation, RedundantHttp,
        RedundantSql, SerializedCalls, SlowHttp, SlowSql,
    };
    use Framework::{
        CsharpEfCore, CsharpGeneric, GoGeneric, GoGorm, JavaGeneric, JavaHelidonMp, JavaHelidonSe,
        JavaJpa, JavaQuarkus, JavaQuarkusReactive, JavaWebFlux, NodeGeneric, NodePrisma,
        PhpDoctrine, PhpGeneric, PhpLaravelEloquent, PythonDjango, PythonGeneric, PythonSqlAlchemy,
        RubyActiveRecord, RubyGeneric, RustDiesel, RustGeneric, RustSeaOrm,
    };
    let entries: &[((FindingType, Framework), &str, Option<&str>)] = &[
        // ── Java ───────────────────────────────────────────────────
        (
            (NPlusOneSql, JavaJpa),
            "Use `JOIN FETCH` on the relationship or annotate the repository \
             method with `@EntityGraph` to load associations in a single query.",
            Some(
                "https://docs.jboss.org/hibernate/orm/current/userguide/html_single/\
                 Hibernate_User_Guide.html#fetching-strategies-dynamic-fetching",
            ),
        ),
        (
            (RedundantSql, JavaJpa),
            "Add `Spring`'s `@Cacheable` on the repository or service method, \
             or share the `EntityManager` within the request via `@Transactional` \
             so `Hibernate`'s first-level cache deduplicates the read.",
            Some("https://docs.spring.io/spring-framework/reference/integration/cache.html"),
        ),
        (
            (NPlusOneSql, JavaQuarkusReactive),
            "Use `Mutiny`'s Hibernate Reactive `Session.fetch()` with `@NamedEntityGraph`, \
             or join the relation in a `Panache` reactive query, to load associations \
             in a single round-trip.",
            Some("https://quarkus.io/guides/hibernate-reactive"),
        ),
        (
            (NPlusOneHttp, JavaWebFlux),
            "Replace the sequential `.flatMap()` chain with `Flux.merge()` or `Flux.zip()` \
             for parallel execution, or call a batch endpoint that returns the \
             aggregated result in one round-trip.",
            Some("https://docs.spring.io/spring-framework/reference/web/webflux-functional.html"),
        ),
        (
            (NPlusOneHttp, JavaQuarkusReactive),
            "Replace chained `Uni.chain()` / `Multi.onItem().transformToUni()` calls with \
             `Uni.combine().all().unis(...)` for parallel execution, or call a batch \
             endpoint.",
            Some("https://smallrye.io/smallrye-mutiny/latest/guides/combining-items/"),
        ),
        (
            (NPlusOneHttp, JavaGeneric),
            "Coalesce the calls into a batch endpoint, or cache the per-request \
             results with `Spring`'s `@Cacheable` using a request-scoped cache.",
            Some("https://docs.spring.io/spring-framework/reference/integration/cache.html"),
        ),
        (
            (RedundantSql, JavaQuarkusReactive),
            "Use `Quarkus`' `@CacheResult` on the reactive method, or memoize the `Uni` \
             with `Mutiny`'s `.memoize().indefinitely()` to deduplicate within a request.",
            Some("https://quarkus.io/guides/cache"),
        ),
        (
            (NPlusOneSql, JavaQuarkus),
            "In Quarkus with Hibernate ORM, use a `JOIN FETCH` in your JPQL or `Panache` \
             query, annotate the repository method with `@EntityGraph`, or call \
             `entityManager.unwrap(Session.class).fetchProfile(...)` for a named fetch \
             plan.",
            Some("https://quarkus.io/guides/hibernate-orm-panache#fetching-and-loading"),
        ),
        (
            (NPlusOneHttp, JavaQuarkus),
            "Use `CompletableFuture.allOf(...)` on the Quarkus `ManagedExecutor` for \
             parallel calls, or invoke a batch endpoint via the Quarkus REST Client. \
             For repeated reads, add `@CacheResult` on the client method.",
            Some("https://quarkus.io/guides/rest-client-reactive"),
        ),
        (
            (RedundantSql, JavaQuarkus),
            "Add `@CacheResult` on the `@ApplicationScoped` service method (Quarkus \
             cache extension), or scope a `HashMap` on a `@RequestScoped` bean to \
             deduplicate the query within the request.",
            Some("https://quarkus.io/guides/cache"),
        ),
        (
            (NPlusOneSql, JavaHelidonSe),
            "Replace the per-id loop with a single named Helidon `DbClient` query \
             that performs `JOIN`, or pass a list of ids via the `:ids` JDBC parameter \
             binding. Helidon SE has no JPA layer: the fix happens at the \
             `DbClient` query level.",
            Some("https://helidon.io/docs/latest/se/dbclient"),
        ),
        (
            (NPlusOneHttp, JavaHelidonSe),
            "Fan out concurrent requests with Helidon `WebClient` using \
             `Single.zip(...)` or `Multi.merge(...)`. Or call a batch endpoint that \
             returns the aggregated result in one round-trip.",
            Some("https://helidon.io/docs/latest/se/webclient"),
        ),
        (
            (NPlusOneSql, JavaHelidonMp),
            "Helidon MP entities are JPA-managed under Hibernate. Use \
             `@EntityGraph` on the repository method or JPQL `JOIN FETCH` on the \
             relationship to load associations in a single query.",
            Some("https://helidon.io/docs/latest/mp/persistence"),
        ),
        (
            (NPlusOneHttp, JavaHelidonMp),
            "Use the MicroProfile Rest Client with `CompletableFuture.allOf(...)` \
             on the `@ManagedExecutorConfig` executor for parallel calls. Or call \
             a batch endpoint that returns the aggregated result in one \
             round-trip.",
            Some(
                "https://download.eclipse.org/microprofile/microprofile-rest-client-3.0/microprofile-rest-client-spec-3.0.html",
            ),
        ),
        (
            (RedundantSql, JavaGeneric),
            "Add a service-level cache (`Caffeine`, `Spring Cache`) or deduplicate the \
             query within the request scope.",
            Some("https://docs.spring.io/spring-framework/reference/integration/cache.html"),
        ),
        // ── C# (.NET 8 to 10) ──────────────────────────────────────
        (
            (NPlusOneSql, CsharpEfCore),
            "Use `.Include()` (and `.ThenInclude()` for nested relations) to eager-load. \
             Add `.AsSplitQuery()` when `Include` causes Cartesian explosion. Consider \
             `.AsNoTracking()` for read-only queries.",
            Some("https://learn.microsoft.com/en-us/ef/core/querying/related-data/eager"),
        ),
        (
            (RedundantSql, CsharpEfCore),
            "Use `IMemoryCache` from `Microsoft.Extensions.Caching.Memory`, or add EF \
             Core's second-level cache via a community extension. Within a request, \
             scope the `DbContext` so identical reads short-circuit through the change \
             tracker.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/performance/caching/memory"),
        ),
        (
            (NPlusOneHttp, CsharpGeneric),
            "Use `Task.WhenAll` for parallel independent calls, or call a batch \
             endpoint. For repeated identical calls, configure response caching on \
             `HttpClient` via `DelegatingHandler`.",
            Some(
                "https://learn.microsoft.com/en-us/dotnet/api/system.threading.tasks.task.whenall",
            ),
        ),
        // ── Rust ───────────────────────────────────────────────────
        (
            (NPlusOneSql, RustDiesel),
            "Load associations with Diesel's `belonging_to` + `grouped_by` pattern \
             (two queries instead of N+1), or use `.inner_join()` / `.left_join()` to \
             fetch parent + children in a single query.",
            Some("https://docs.diesel.rs/master/diesel/associations/index.html"),
        ),
        (
            (NPlusOneSql, RustSeaOrm),
            "Use `Entity::find().find_with_related(...)` or `.find_also_related(...)` to \
             fetch related entities in a single query, or load with a JOIN via \
             `QuerySelect::join()`.",
            Some("https://www.sea-ql.org/SeaORM/docs/relation/select-related/"),
        ),
        (
            (RedundantSql, RustDiesel),
            "Cache the result with the `moka` crate, or scope-deduplicate via a \
             request-local `OnceCell` stored in `axum`/`actix-web` extensions.",
            Some("https://docs.rs/moka"),
        ),
        (
            (RedundantSql, RustSeaOrm),
            "Cache the result with the `moka` crate, or memoize per-request via a \
             `OnceCell` stored in your handler state.",
            Some("https://docs.rs/moka"),
        ),
        (
            (NPlusOneHttp, RustGeneric),
            "Use `tokio::join!` or `futures::future::join_all` for parallel independent \
             calls. Switch to a batch endpoint when the calls fan out from the same \
             upstream input.",
            Some("https://docs.rs/tokio/latest/tokio/macro.join.html"),
        ),
        // ── Python ────────────────────────────────────────────────
        (
            (NPlusOneSql, PythonDjango),
            "Use `select_related()` for foreign-key joins or `prefetch_related()` for \
             reverse/M2M relations to eager-load associations in one or two queries \
             instead of N+1.",
            Some("https://docs.djangoproject.com/en/5.2/ref/models/querysets/#select-related"),
        ),
        (
            (NPlusOneSql, PythonSqlAlchemy),
            "Add `joinedload()` or `subqueryload()` in the query options to eager-load \
             the relationship, or rewrite with an explicit `join()`.",
            Some(
                "https://docs.sqlalchemy.org/en/21/orm/queryguide/relationships.html#joined-eager-loading",
            ),
        ),
        (
            (RedundantSql, PythonDjango),
            "Cache the queryset result with Django's cache framework (`@cache_page` \
             for views, `cache.get`/`set` for manual memoization), or share the result \
             via a request-local variable to deduplicate within the request.",
            Some("https://docs.djangoproject.com/en/5.2/topics/cache/"),
        ),
        (
            (RedundantSql, PythonSqlAlchemy),
            "Use `dogpile.cache` or a scoped-session memoization pattern to \
             deduplicate identical queries within the request. Share the result \
             via the session's identity map when the same row is loaded twice.",
            Some(
                "https://docs.sqlalchemy.org/en/21/orm/session_basics.html#is-the-session-a-cache",
            ),
        ),
        (
            (NPlusOneHttp, PythonGeneric),
            "Use `asyncio.gather()` or `concurrent.futures.ThreadPoolExecutor` for \
             parallel independent calls. Switch to a batch endpoint when the calls \
             fan out from the same upstream input.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.gather"),
        ),
        // ── redundant_http ─────────────────────────────────────────
        (
            (RedundantHttp, JavaGeneric),
            "Wrap the HTTP client in a request-scoped memoization layer (`Caffeine`, \
             a `HashMap` on a request-scoped bean, or `Spring`'s `@Cacheable` with a \
             request-scoped key) so identical calls return the cached response \
             within the request.",
            Some("https://github.com/ben-manes/caffeine"),
        ),
        (
            (RedundantHttp, CsharpEfCore),
            "Insert a `DelegatingHandler` on `HttpClient` that memoizes responses by \
             request key (URL + headers) inside `IMemoryCache` for the request's \
             lifetime. Be explicit about the cache key to avoid serving stale \
             data across users.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/performance/caching/memory"),
        ),
        (
            (RedundantHttp, CsharpGeneric),
            "Add a `DelegatingHandler` on `HttpClient` that memoizes by request URI \
             inside `IMemoryCache` for the request scope, or share the response via \
             `AsyncLazy<T>` when the duplication is concurrent.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/performance/caching/memory"),
        ),
        (
            (RedundantHttp, RustGeneric),
            "Memoize per-request with the `moka` crate or a request-local `OnceCell` \
             stored in your handler state. For concurrent duplicates within one \
             handler, share the in-flight future via `futures::future::Shared`.",
            Some("https://docs.rs/moka"),
        ),
        (
            (RedundantHttp, PythonGeneric),
            "Memoize per-request with `functools.lru_cache` (sync) or an async \
             memoization decorator. For Django, wrap the view with `@cache_page` or \
             share the response via a request-local dict.",
            Some("https://docs.python.org/3/library/functools.html#functools.lru_cache"),
        ),
        // ── slow_sql ───────────────────────────────────────────────
        (
            (SlowSql, JavaJpa),
            "Profile the rendered SQL with `EXPLAIN ANALYZE`. Common JPA fixes: add \
             a composite index matching the `WHERE` + `ORDER BY` columns, switch to \
             a DTO projection via `@Query(\"select new ...\")` to avoid hydrating \
             the full entity graph, or paginate with `Slice`/keyset pagination when \
             `OFFSET` grows linearly.",
            Some("https://docs.spring.io/spring-data/jpa/reference/jpa/query-methods.html"),
        ),
        (
            (SlowSql, JavaQuarkus),
            "Enable `hibernate.generate_statistics` + the slow query log to confirm \
             the offender. Add a composite index, project to a DTO via `Panache`'s \
             `.project(Class)` or a native query, or paginate with `.range(...)` on \
             a keyset-friendly column.",
            Some("https://quarkus.io/guides/hibernate-orm-panache"),
        ),
        (
            (SlowSql, JavaGeneric),
            "Profile with `EXPLAIN ANALYZE`. Add an index on the columns used in \
             `WHERE` + `ORDER BY` when selectivity justifies it. If the plan is \
             dominated by a nested loop on a large outer, rewrite to push the \
             selective predicate first.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowSql, CsharpEfCore),
            "Inspect the generated SQL via `.ToQueryString()` and `EXPLAIN ANALYZE` \
             it. Add an index via Fluent API (`HasIndex`). For `Include` explosion, \
             switch to `.AsSplitQuery()`. For read-only paths add `.AsNoTracking()` \
             to remove change-tracker overhead.",
            Some("https://learn.microsoft.com/en-us/ef/core/performance/efficient-querying"),
        ),
        (
            (SlowSql, CsharpGeneric),
            "Run `EXPLAIN ANALYZE` against the rendered query and add an index when \
             the plan shows a sequential scan over a large table. Prefer parameterized \
             queries so the plan cache stays hot.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowSql, RustDiesel),
            "Print the rendered SQL with `diesel::debug_query`, `EXPLAIN ANALYZE` it, \
             and add the missing index. For wide rows, use a `.select((col_a, col_b))` \
             projection so postgres can use an index-only scan.",
            Some("https://docs.diesel.rs/master/diesel/fn.debug_query.html"),
        ),
        (
            (SlowSql, RustSeaOrm),
            "Capture the query via the SQL logger feature, `EXPLAIN ANALYZE` the \
             output, and add an index when the plan does a sequential scan. \
             Project to a partial model with `.select_only().column(...)` when \
             only a few columns are read.",
            Some("https://www.sea-ql.org/SeaORM/docs/index/"),
        ),
        (
            (SlowSql, RustGeneric),
            "Run `EXPLAIN ANALYZE` on the slow query. Add a composite index \
             matching the `WHERE` + `ORDER BY` columns, or rewrite to push the \
             selective predicate before any join.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowSql, PythonDjango),
            "Run `EXPLAIN ANALYZE` on the rendered query (`django-debug-toolbar` or \
             `connection.queries`). Add a `db_index=True` on the model field, or use \
             `.only()`/`.defer()` to limit fetched columns.",
            Some("https://docs.djangoproject.com/en/5.2/ref/models/options/#indexes"),
        ),
        (
            (SlowSql, PythonSqlAlchemy),
            "Capture the rendered SQL via `echo=True` and `EXPLAIN ANALYZE` it. Add \
             an index via `Index()` in the table metadata, or project with \
             `.with_only_columns()` to reduce transferred data.",
            Some("https://docs.sqlalchemy.org/en/21/core/metadata.html#sqlalchemy.schema.Index"),
        ),
        (
            (SlowSql, PythonGeneric),
            "Run `EXPLAIN ANALYZE` on the slow query. Add an index on the `WHERE` + \
             `ORDER BY` columns, or rewrite to push the selective predicate first.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        // ── slow_http ──────────────────────────────────────────────
        (
            (SlowHttp, JavaGeneric),
            "Profile the upstream's own `slow_sql` / `slow_http` findings first \
             (the latency is usually upstream-side). On the client, wrap the \
             call in a `Resilience4j` circuit breaker with a tight timeout, and \
             cache the response when staleness tolerance allows.",
            Some("https://resilience4j.readme.io/docs/circuitbreaker"),
        ),
        (
            (SlowHttp, CsharpGeneric),
            "Set a tight `HttpClient.Timeout` and wrap calls in a `Polly` retry + \
             circuit breaker. If the latency is structural (slow upstream), \
             cache the response with `IMemoryCache` or move the call off the \
             request hot path via background hosted services.",
            Some(
                "https://learn.microsoft.com/en-us/dotnet/architecture/microservices/implement-resilient-applications/",
            ),
        ),
        (
            (SlowHttp, RustGeneric),
            "Set a per-request timeout on `reqwest` / `hyper` / `surf`, wrap with the \
             `tower` circuit-breaker layer, and cache the response with `moka` when \
             staleness is acceptable.",
            Some("https://docs.rs/tower"),
        ),
        (
            (SlowHttp, PythonGeneric),
            "Set a per-request timeout on `httpx` / `aiohttp`, add a `tenacity` retry \
             with exponential backoff, and cache the response with Django cache \
             or `dogpile.cache` when staleness is acceptable.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.wait_for"),
        ),
        // ── excessive_fanout ───────────────────────────────────────
        (
            (ExcessiveFanout, JavaWebFlux),
            "Bound the fan-out width with `Flux.flatMap(concurrency = N)` (default \
             concurrency is unbounded). Add a `Resilience4j` bulkhead so a slow \
             upstream cannot saturate the reactor scheduler.",
            Some("https://projectreactor.io/docs/core/release/reference/#which-operator"),
        ),
        (
            (ExcessiveFanout, JavaQuarkusReactive),
            "Bound the fan-out with `Multi.onItem().transformToUniAndConcatenate()` \
             when ordering matters, or `.transformToUniAndMerge(concurrency = N)` \
             when independent. Add a `Resilience4j` bulkhead on the downstream \
             client.",
            Some("https://smallrye.io/smallrye-mutiny/latest/guides/combining-items/"),
        ),
        (
            (ExcessiveFanout, JavaGeneric),
            "Replace the fan-out with a single bulk endpoint when the downstream \
             supports it. Otherwise apply the bulkhead pattern (`Resilience4j` or a \
             dedicated thread pool) to bound the blast radius of a slow \
             dependency.",
            Some("https://resilience4j.readme.io/docs/bulkhead"),
        ),
        (
            (ExcessiveFanout, CsharpGeneric),
            "Cap parallelism with `Parallel.ForEachAsync(MaxDegreeOfParallelism = N)` \
             or a `SemaphoreSlim`, and prefer a batch endpoint when the upstream \
             offers one. Layer `Polly`'s `RateLimiter` strategy on the `HttpClient` \
             to enforce a hard ceiling (`Polly` v8 subsumes the v7 `Bulkhead`).",
            Some("https://www.pollydocs.org/strategies/rate-limiter.html"),
        ),
        (
            (ExcessiveFanout, RustGeneric),
            "Use `futures::stream::iter(...).buffer_unordered(N)` to cap concurrent \
             requests, or replace the fan-out with a batch endpoint. Layer a \
             `tower::ConcurrencyLimit` on the downstream client to enforce a hard \
             ceiling.",
            Some(
                "https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html#method.buffer_unordered",
            ),
        ),
        (
            (ExcessiveFanout, PythonGeneric),
            "Cap parallelism with `asyncio.Semaphore` or \
             `concurrent.futures.ThreadPoolExecutor(max_workers=N)`, and prefer a \
             batch endpoint when the upstream offers one.",
            Some("https://docs.python.org/3/library/asyncio-sync.html#asyncio.Semaphore"),
        ),
        // ── chatty_service ─────────────────────────────────────────
        (
            (ChattyService, JavaGeneric),
            "Coalesce the chatty interactions into a single bulk endpoint, or \
             move the orchestration upstream so the calls happen inside one \
             service. When the chattiness is between services and a bulk \
             endpoint is impossible, add a CQRS-style read model populated \
             asynchronously.",
            Some("https://martinfowler.com/articles/microservices.html#SmartEndpointsAndDumbPipes"),
        ),
        (
            (ChattyService, CsharpGeneric),
            "Combine the calls into a single bulk endpoint, or introduce a \
             gRPC streaming RPC that returns the aggregated payload in one \
             round-trip. As a stopgap, fan-in with `Task.WhenAll` and an \
             `AsyncLazy<T>` per-key cache.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/grpc/protobuf"),
        ),
        (
            (ChattyService, RustGeneric),
            "Coalesce the calls behind a single bulk endpoint, or expose a \
             `tonic` streaming RPC that returns the aggregated payload. As an \
             intermediate, batch with `futures::future::join_all` and a `moka` \
             per-key cache.",
            Some("https://docs.rs/tonic"),
        ),
        (
            (ChattyService, PythonGeneric),
            "Coalesce the chatty interactions into a single bulk endpoint, or \
             fan-in with `asyncio.gather` and a per-key cache (Django cache or \
             `dogpile.cache`). Reduce round-trips by moving orchestration upstream.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.gather"),
        ),
        // ── pool_saturation ────────────────────────────────────────
        (
            (PoolSaturation, JavaQuarkus),
            "Raise `quarkus.datasource.jdbc.max-size` only after confirming the \
             root cause: usually slow queries hold connections too long. Profile \
             `slow_sql` findings on the same service first, then size the pool to \
             handle the corrected workload.",
            Some("https://quarkus.io/guides/datasource#jdbc-configuration-reference"),
        ),
        (
            (PoolSaturation, JavaGeneric),
            "Inspect `slow_sql` findings on the same service: pool saturation is \
             usually a symptom, not the disease. After speeding up the slow \
             queries, raise the `HikariCP` `maximumPoolSize` only if the corrected \
             workload still saturates.",
            Some("https://github.com/brettwooldridge/HikariCP/wiki/About-Pool-Sizing"),
        ),
        (
            (PoolSaturation, CsharpEfCore),
            "Check `Npgsql` `Max Pool Size` in the connection string and the \
             `DbContext` lifetime (a per-request `DbContext` should release \
             connections to the pool on `Dispose`). Slow queries on the same \
             service usually drive the saturation, fix those first.",
            Some("https://www.npgsql.org/doc/connection-string-parameters.html"),
        ),
        (
            (PoolSaturation, CsharpGeneric),
            "Profile the `slow_sql` findings on the same service: pool saturation \
             follows from connections being held during slow work. Raise the \
             pool size only after the slow path is corrected.",
            Some(
                "https://learn.microsoft.com/en-us/sql/connect/ado-net/sql-server-connection-pooling",
            ),
        ),
        (
            (PoolSaturation, RustGeneric),
            "Inspect `slow_sql` findings first: `sqlx` and `deadpool` pools usually \
             saturate because connections are held during slow work. Tune \
             `max_connections` on `PoolOptions` only after the slow queries are \
             addressed.",
            Some("https://docs.rs/sqlx/latest/sqlx/pool/struct.PoolOptions.html"),
        ),
        (
            (PoolSaturation, PythonGeneric),
            "Inspect `slow_sql` findings on the same service: pool saturation usually \
             follows from connections held during slow work. Tune `pool_size` on \
             SQLAlchemy's `create_engine` or Django's `CONN_MAX_AGE` only after the \
             slow path is corrected.",
            Some(
                "https://docs.sqlalchemy.org/en/21/core/engines.html#sqlalchemy.create_engine.params.pool_size",
            ),
        ),
        // ── serialized_calls ───────────────────────────────────────
        (
            (SerializedCalls, JavaWebFlux),
            "Replace the sequential `Mono.flatMap` chain with `Mono.zip(m1, m2, ...)` \
             or `Flux.merge(...)` when the calls are independent. Keep the \
             sequential chain only when one call's output feeds the next.",
            Some("https://projectreactor.io/docs/core/release/reference/#which.combining"),
        ),
        (
            (SerializedCalls, JavaQuarkusReactive),
            "Switch the sequential `Uni.chain()` to \
             `Uni.combine().all().unis(u1, u2, ...).asTuple()` when the calls do \
             not depend on each other. Drop back to chain only when the \
             next call needs the previous result.",
            Some("https://smallrye.io/smallrye-mutiny/latest/guides/combining-items/"),
        ),
        (
            (SerializedCalls, JavaGeneric),
            "Parallelize independent calls with `CompletableFuture.allOf(...)` on \
             the `ManagedExecutor` (Quarkus) or `@Async` (Spring). Keep the calls \
             serial only when one's output feeds the next.",
            Some(
                "https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/concurrent/CompletableFuture.html",
            ),
        ),
        (
            (SerializedCalls, CsharpGeneric),
            "Replace the sequential await chain with `Task.WhenAll(t1, t2, ...)` \
             when the calls do not depend on each other. Use `ValueTask` for \
             completed-synchronously paths to avoid allocating `Task` wrappers.",
            Some(
                "https://learn.microsoft.com/en-us/dotnet/api/system.threading.tasks.task.whenall",
            ),
        ),
        (
            (SerializedCalls, RustGeneric),
            "Replace sequential `.await` chains with `tokio::join!(f1, f2, ...)` or \
             `futures::future::join_all(iter)` when the calls are independent. \
             Use `try_join!` when any failure should short-circuit the rest.",
            Some("https://docs.rs/tokio/latest/tokio/macro.join.html"),
        ),
        (
            (SerializedCalls, PythonGeneric),
            "Replace sequential awaits with `asyncio.gather(t1, t2, ...)` when the \
             calls are independent. For sync code, use \
             `concurrent.futures.ThreadPoolExecutor` to run independent calls in \
             parallel. Keep the sequential form only when one call's output feeds \
             the next.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.gather"),
        ),
        // ── Go ────────────────────────────────────────────────────
        (
            (NPlusOneSql, GoGorm),
            "Use `Preload()` or `Joins()` to eager-load the association in a \
             single query instead of N+1 lazy loads.",
            Some("https://gorm.io/docs/preload.html"),
        ),
        (
            (NPlusOneSql, GoGeneric),
            "Rewrite the per-id loop as a single query with a `JOIN` or `WHERE id \
             IN ($1, $2, ...)`. With `pgx`, use `pgx.NamedArgs` for named parameters \
             or pass an array via `ANY($1::int[])` for bulk lookups.",
            Some("https://pkg.go.dev/github.com/jackc/pgx/v5"),
        ),
        (
            (RedundantSql, GoGorm),
            "Cache the result with `go-cache`, `sync.Map`, or a request-scoped map \
             stored in the context via `context.WithValue`. GORM does not deduplicate \
             reads automatically, the caching layer must be explicit.",
            Some("https://pkg.go.dev/github.com/patrickmn/go-cache"),
        ),
        (
            (RedundantSql, GoGeneric),
            "Cache the result with `go-cache` or a `sync.Map`, or pass a \
             request-scoped map via `context.WithValue` to deduplicate within \
             the request.",
            Some("https://pkg.go.dev/github.com/patrickmn/go-cache"),
        ),
        (
            (NPlusOneHttp, GoGeneric),
            "Use `errgroup.Go` for parallel independent calls, or call a batch \
             endpoint that returns the aggregated result in one round-trip.",
            Some("https://pkg.go.dev/golang.org/x/sync/errgroup"),
        ),
        (
            (RedundantHttp, GoGeneric),
            "Memoize per-request with `singleflight.Do` (for concurrent identical \
             calls) or a `sync.Map` keyed by request URL stored in the context.",
            Some("https://pkg.go.dev/golang.org/x/sync/singleflight"),
        ),
        (
            (SlowSql, GoGorm),
            "Enable GORM's `Logger` in Info mode, capture the rendered SQL, and \
             `EXPLAIN ANALYZE` it. Add an index via `AutoMigrate` or a raw migration. \
             Use `.Select()` to limit fetched columns.",
            Some("https://gorm.io/docs/performance.html"),
        ),
        (
            (SlowSql, GoGeneric),
            "Run `EXPLAIN ANALYZE` on the slow query. Add a composite index \
             matching the `WHERE` + `ORDER BY` columns. With `pgx`, use \
             `.QueryRow().Scan()` with only the needed columns.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowHttp, GoGeneric),
            "Set a per-request timeout via `http.Client.Timeout` or \
             `context.WithTimeout`. Add a circuit breaker (`sony/gobreaker`) and \
             cache the response with `go-cache` when staleness is acceptable.",
            Some("https://pkg.go.dev/github.com/sony/gobreaker"),
        ),
        (
            (ExcessiveFanout, GoGeneric),
            "Bound goroutine count with a semaphore channel (`make(chan struct{}, N)`) \
             or `errgroup` with `SetLimit(N)`. Prefer a batch endpoint when the \
             downstream supports it.",
            Some("https://pkg.go.dev/golang.org/x/sync/errgroup#Group.SetLimit"),
        ),
        (
            (ChattyService, GoGeneric),
            "Coalesce the chatty interactions into a single bulk endpoint, or \
             fan-in with `errgroup` and a per-key `go-cache`. Reduce round-trips by \
             moving orchestration upstream.",
            Some("https://pkg.go.dev/golang.org/x/sync/errgroup"),
        ),
        (
            (PoolSaturation, GoGeneric),
            "Inspect `slow_sql` findings first: `pgxpool` usually saturates because \
             connections are held during slow work. Tune `MaxConns` on \
             `pgxpool.Config` only after the slow queries are addressed.",
            Some("https://pkg.go.dev/github.com/jackc/pgx/v5/pgxpool#Config"),
        ),
        (
            (SerializedCalls, GoGeneric),
            "Replace sequential calls with `errgroup.Go` for parallel execution. \
             Keep the sequential form only when one call's output feeds the next.",
            Some("https://pkg.go.dev/golang.org/x/sync/errgroup"),
        ),
        // ── Node.js / TypeScript ──────────────────────────────────
        (
            (NPlusOneSql, NodePrisma),
            "Use `include:{}` for eager loading, or rewrite with a `findMany()` \
             that uses a `WHERE id IN` filter instead of N separate `findUnique()` \
             calls.",
            Some("https://www.prisma.io/docs/orm/prisma-client/queries/relation-queries"),
        ),
        (
            (NPlusOneSql, NodeGeneric),
            "Rewrite the per-id loop as a single query with a `JOIN` or `WHERE id \
             IN ($1, $2, ...)`. With the `pg` client, use a parameterized query \
             with `ANY($1::int[])`.",
            Some("https://node-postgres.com/features/queries"),
        ),
        (
            (RedundantSql, NodePrisma),
            "Wrap queries in a request-scoped `Map` to deduplicate identical reads \
             within the request, or use a `Prisma` client extension that memoizes \
             by query key.",
            Some("https://www.prisma.io/docs/orm/prisma-client/client-extensions"),
        ),
        (
            (RedundantSql, NodeGeneric),
            "Cache the result with `node-cache` or a request-scoped `Map` stored \
             in `Express`/`Fastify` request locals. For concurrent identical \
             queries, use `p-memoize`.",
            Some("https://www.npmjs.com/package/node-cache"),
        ),
        (
            (NPlusOneHttp, NodeGeneric),
            "Use `Promise.all` for parallel independent calls, or call a batch \
             endpoint that returns the aggregated result in one round-trip.",
            Some(
                "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Promise/all",
            ),
        ),
        (
            (RedundantHttp, NodeGeneric),
            "Memoize per-request with `p-memoize` or a `Map` stored in request \
             locals. For concurrent identical calls, share the in-flight \
             `Promise`.",
            Some("https://www.npmjs.com/package/p-memoize"),
        ),
        (
            (SlowSql, NodePrisma),
            "Enable Prisma's query logging, capture the rendered SQL, and \
             `EXPLAIN ANALYZE` it. Add an `@@index` in the `schema.prisma` model. \
             Use `.select()` to limit fetched columns.",
            Some("https://www.prisma.io/docs/orm/prisma-schema/data-model/indexes"),
        ),
        (
            (SlowSql, NodeGeneric),
            "Run `EXPLAIN ANALYZE` on the slow query. Add a composite index \
             matching the `WHERE` + `ORDER BY` columns.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowHttp, NodeGeneric),
            "Set a per-request timeout via `AbortController` + `setTimeout`. Add a \
             circuit breaker (`opossum`) and cache the response with `node-cache` \
             when staleness is acceptable.",
            Some("https://www.npmjs.com/package/opossum"),
        ),
        (
            (ExcessiveFanout, NodeGeneric),
            "Use `p-limit` to cap concurrency (`const limit = pLimit(N); \
             await Promise.all(urls.map(u => limit(() => fetch(u))))`). Prefer \
             a batch endpoint when the downstream supports it.",
            Some("https://www.npmjs.com/package/p-limit"),
        ),
        (
            (ChattyService, NodeGeneric),
            "Coalesce the chatty interactions into a single bulk endpoint, or \
             fan-in with `Promise.all` and a per-key `Map` cache. For GraphQL, use \
             `DataLoader` to batch and deduplicate.",
            Some("https://www.npmjs.com/package/dataloader"),
        ),
        (
            (PoolSaturation, NodeGeneric),
            "Inspect `slow_sql` findings first: the `pg` `Pool` usually saturates \
             because connections are held during slow work. Tune `max` on \
             `new Pool({ max: N })` only after the slow queries are addressed.",
            Some("https://node-postgres.com/apis/pool"),
        ),
        (
            (SerializedCalls, NodeGeneric),
            "Replace sequential awaits with `Promise.all([p1, p2, ...])` when \
             the calls are independent. Keep the sequential form only when one \
             call's output feeds the next.",
            Some(
                "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Promise/all",
            ),
        ),
        // ── Ruby ───────────────────────────────────────────────────
        (
            (NPlusOneSql, RubyActiveRecord),
            "Eager-load the association with `includes(:assoc)` (or `preload` / \
             `eager_load`) so Active Record loads it in one query instead of \
             one per record.",
            Some(
                "https://guides.rubyonrails.org/active_record_querying.html#eager-loading-associations",
            ),
        ),
        (
            (RedundantSql, RubyActiveRecord),
            "Rails wraps each request in an Active Record query cache that dedups \
             identical SELECTs. If it is bypassed, memoize the result within the \
             request with `@value ||= ...`.",
            Some("https://guides.rubyonrails.org/caching_with_rails.html#sql-caching"),
        ),
        (
            (SlowSql, RubyActiveRecord),
            "Inspect the plan with `.explain` on the relation, then add an index \
             through a migration (`add_index`). Narrow the row with `.select(...)` \
             to fetch only needed columns.",
            Some("https://guides.rubyonrails.org/active_record_querying.html#running-explain"),
        ),
        (
            (NPlusOneSql, RubyGeneric),
            "Batch the per-record lookups into a single `where(id: ids)` query, or \
             eager-load the association if an ORM is in use.",
            Some("https://guides.rubyonrails.org/active_record_querying.html"),
        ),
        (
            (NPlusOneHttp, RubyGeneric),
            "Coalesce the per-item HTTP calls into one batch request, or run them \
             concurrently with a bounded thread pool from `concurrent-ruby`. Cache \
             repeated reads within the request.",
            Some("https://github.com/ruby-concurrency/concurrent-ruby"),
        ),
        (
            (RedundantSql, RubyGeneric),
            "Memoize the read within the request with `@value ||= ...`, or rely on \
             the Active Record query cache when running under Rails.",
            Some("https://guides.rubyonrails.org/caching_with_rails.html#sql-caching"),
        ),
        (
            (RedundantHttp, RubyGeneric),
            "Memoize the response per request with `@value ||= ...`, or cache it \
             with `Rails.cache.fetch`. Share the in-flight call when several run \
             concurrently.",
            Some("https://guides.rubyonrails.org/caching_with_rails.html"),
        ),
        (
            (SlowSql, RubyGeneric),
            "Run the query through `.explain`, then add a composite index matching \
             the `WHERE` and `ORDER BY` columns via a migration.",
            Some("https://guides.rubyonrails.org/active_record_querying.html#running-explain"),
        ),
        (
            (SlowHttp, RubyGeneric),
            "Set an explicit timeout on the HTTP client (`Net::HTTP#read_timeout`, \
             Faraday `options.timeout`). Add a circuit breaker and cache the \
             response with `Rails.cache` when staleness is acceptable.",
            Some("https://lostisland.github.io/faraday/#/customization/request-options"),
        ),
        (
            (ExcessiveFanout, RubyGeneric),
            "Cap concurrency with a bounded `Concurrent::FixedThreadPool` instead \
             of unbounded fan-out, or call a batch endpoint when the downstream \
             supports it.",
            Some("https://github.com/ruby-concurrency/concurrent-ruby"),
        ),
        (
            (ChattyService, RubyGeneric),
            "Coalesce the chatty calls into a single bulk endpoint, or batch and \
             deduplicate reads per request with a memoization `Hash`.",
            Some("https://guides.rubyonrails.org/caching_with_rails.html"),
        ),
        (
            (PoolSaturation, RubyGeneric),
            "Inspect `slow_sql` findings first: the Active Record connection pool \
             usually saturates because connections are held during slow work. Tune \
             `pool:` in `database.yml` only after the slow queries are addressed.",
            Some("https://guides.rubyonrails.org/configuring.html#database-pooling"),
        ),
        (
            (SerializedCalls, RubyGeneric),
            "Run independent calls concurrently with `Concurrent::Promises.zip(...)` \
             or the async gem. Keep them sequential only when one call's output \
             feeds the next.",
            Some("https://github.com/ruby-concurrency/concurrent-ruby"),
        ),
        // ── PHP ────────────────────────────────────────────────────
        // Laravel/Eloquent carries all ten anti-patterns because the
        // `io.opentelemetry.contrib.php.laravel` scope is app-wide, so a
        // Laravel finding of any type gets framework-idiomatic advice
        // rather than the PhpGeneric text.
        (
            (NPlusOneSql, PhpLaravelEloquent),
            "Eager-load the relation with `with('relation')` (or `load(...)` on an \
             already-fetched collection) so Eloquent runs one query instead of one \
             per model. In a query-builder loop, batch with `whereIn('id', $ids)`.",
            Some("https://laravel.com/docs/eloquent-relationships#eager-loading"),
        ),
        (
            (RedundantSql, PhpLaravelEloquent),
            "Memoize the read within the request (`$this->cached ??= ...`), or cache \
             it with `Cache::remember(...)` when the value is stable across requests.",
            Some("https://laravel.com/docs/cache"),
        ),
        (
            (SlowSql, PhpLaravelEloquent),
            "Inspect the plan with `EXPLAIN`, add an index through a migration \
             (`$table->index([...])`), and narrow the row with `->select([...])` to \
             fetch only the needed columns.",
            Some("https://laravel.com/docs/migrations#indexes"),
        ),
        (
            (NPlusOneHttp, PhpLaravelEloquent),
            "Coalesce the per-item calls into one batch request, or run them \
             concurrently with `Http::pool(...)`. Cache repeated reads within the \
             request.",
            Some("https://laravel.com/docs/http-client"),
        ),
        (
            (RedundantHttp, PhpLaravelEloquent),
            "Memoize the response per request (`$this->cached ??= ...`), or cache it \
             with `Cache::remember(...)`. Share the in-flight call when several run \
             concurrently.",
            Some("https://laravel.com/docs/cache"),
        ),
        (
            (SlowHttp, PhpLaravelEloquent),
            "Set an explicit timeout on the client (`Http::timeout(...)`), add retries \
             with backoff (`->retry(...)`), and cache the response when staleness is \
             acceptable.",
            Some("https://laravel.com/docs/http-client"),
        ),
        (
            (ExcessiveFanout, PhpLaravelEloquent),
            "Cap concurrency with a bounded `Http::pool(...)` batch instead of \
             unbounded fan-out, or call a batch endpoint when the downstream \
             supports it.",
            Some("https://laravel.com/docs/http-client"),
        ),
        (
            (ChattyService, PhpLaravelEloquent),
            "Coalesce the chatty calls into a single bulk endpoint, or batch and \
             deduplicate reads per request with a memoization array.",
            None,
        ),
        (
            (PoolSaturation, PhpLaravelEloquent),
            "Inspect `slow_sql` findings first. Under PHP-FPM each worker holds one \
             database connection, so saturation usually means connections are held \
             during slow work. Resolve the slow queries, then size `pm.max_children` \
             and the database `max_connections` together.",
            None,
        ),
        (
            (SerializedCalls, PhpLaravelEloquent),
            "Run independent calls concurrently with `Http::pool(...)` (or Guzzle \
             promises `Promise\\all`). Keep them sequential only when one call's \
             output feeds the next.",
            Some("https://laravel.com/docs/http-client"),
        ),
        (
            (NPlusOneSql, PhpDoctrine),
            "Add a DQL fetch-join (`->leftJoin('e.assoc', 'a')->addSelect('a')`) to \
             hydrate the association in one query, or map it `fetch=\"EAGER\"` when it \
             is always needed.",
            Some(
                "https://www.doctrine-project.org/projects/doctrine-orm/en/current/\
                 reference/dql-doctrine-query-language.html",
            ),
        ),
        (
            (RedundantSql, PhpDoctrine),
            "Enable the Doctrine result cache on the query (`->enableResultCache(...)`), \
             or reuse the already-managed entity from the identity map instead of \
             re-querying it within the same request.",
            Some(
                "https://www.doctrine-project.org/projects/doctrine-orm/en/current/\
                 reference/caching.html",
            ),
        ),
        (
            (SlowSql, PhpDoctrine),
            "Inspect the plan with `EXPLAIN`, then add an index via the mapping \
             (`@ORM\\Index`) or a migration. Fetch only needed fields with a partial \
             DQL `SELECT`.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (NPlusOneSql, PhpGeneric),
            "Batch the per-row lookups into one prepared statement with an `IN (...)` \
             list bound through placeholders, instead of one query per row.",
            Some("https://www.php.net/manual/en/pdo.prepared-statements.php"),
        ),
        (
            (NPlusOneHttp, PhpGeneric),
            "Coalesce the per-item HTTP calls into one batch request, or run them \
             concurrently (Symfony HttpClient is async by default, or a Guzzle pool / \
             `Promise\\all`). Cache repeated reads within the request.",
            Some("https://symfony.com/doc/current/http_client.html"),
        ),
        (
            (RedundantSql, PhpGeneric),
            "Memoize the read within the request (`$cache[$key] ??= ...`), or reuse a \
             shared prepared statement so the driver reuses the plan.",
            Some("https://www.php.net/manual/en/pdo.prepared-statements.php"),
        ),
        (
            (RedundantHttp, PhpGeneric),
            "Memoize the response per request, or cache it in APCu or Redis. Share \
             the in-flight call when several run concurrently.",
            None,
        ),
        (
            (SlowSql, PhpGeneric),
            "Run the query through `EXPLAIN`, then add a composite index matching the \
             `WHERE` and `ORDER BY` columns.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowHttp, PhpGeneric),
            "Set explicit connect and total timeouts on the client (Guzzle \
             `connect_timeout` / `timeout`, or `CURLOPT_TIMEOUT`). Add a circuit \
             breaker and cache the response when staleness is acceptable.",
            Some("https://www.php.net/manual/en/function.curl-setopt.php"),
        ),
        (
            (ExcessiveFanout, PhpGeneric),
            "Cap concurrency with a bounded Guzzle pool (`GuzzleHttp\\Pool` with a \
             concurrency limit) instead of unbounded fan-out, or call a batch \
             endpoint when the downstream supports it.",
            Some("https://symfony.com/doc/current/http_client.html"),
        ),
        (
            (ChattyService, PhpGeneric),
            "Coalesce the chatty calls into a single bulk endpoint, or batch and \
             deduplicate reads per request with a memoization array.",
            None,
        ),
        (
            (PoolSaturation, PhpGeneric),
            "Under PHP-FPM each worker holds one database connection, so saturation \
             usually means connections held during slow work or too many workers per \
             database. Resolve `slow_sql` findings first, then size `pm.max_children` \
             and the database `max_connections` together.",
            None,
        ),
        (
            (SerializedCalls, PhpGeneric),
            "Run independent calls concurrently with a Guzzle pool or `Promise\\all`. \
             Keep them sequential only when one call's output feeds the next.",
            Some("https://symfony.com/doc/current/http_client.html"),
        ),
    ];
    let mut m = HashMap::with_capacity(entries.len());
    for ((ft, fw), recommendation, url) in entries {
        m.insert(
            (ft.clone(), *fw),
            SuggestedFix {
                pattern: ft.as_str().to_string(),
                framework: fw.as_str().to_string(),
                recommendation: (*recommendation).to_string(),
                reference_url: url.map(ToString::to_string),
            },
        );
    }
    // Catch duplicate (FindingType, Framework) keys: HashMap.insert
    // silently overwrites, so without this check a copy-paste error in
    // the entries slice would land unnoticed.
    debug_assert_eq!(
        entries.len(),
        m.len(),
        "duplicate (FindingType, Framework) key in FIXES entries"
    );
    m
});

/// Enrich findings in place with a [`SuggestedFix`] when the framework
/// can be inferred and a mapping exists. No-op for findings where the
/// framework is unknown or the lookup misses.
///
/// Called by [`super::detect`] after the per-trace detectors have run.
pub(crate) fn enrich(findings: &mut [Finding]) {
    for finding in findings.iter_mut() {
        if let Some(fix) = lookup_fix(finding) {
            finding.suggested_fix = Some(fix.clone());
        }
    }
}

fn lookup_fix(finding: &Finding) -> Option<&'static SuggestedFix> {
    let framework = detect_framework(finding)?;
    FIXES.get(&(finding.finding_type.clone(), framework))
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
        return Some(match_namespace_against_language(ns, language).unwrap_or(language.generic()));
    }
    if let Some(loc) = finding.code_location.as_ref() {
        let ns = loc.namespace.as_deref().unwrap_or("");
        if let Some(language) = loc.filepath.as_deref().and_then(language_from_filepath) {
            return Some(
                match_namespace_against_language(ns, language).unwrap_or(language.generic()),
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

/// Deduce the language from ecosystem-native scope prefixes that
/// `SCOPE_RULES` cannot handle: `github.com/` (Go module path),
/// `@opentelemetry/instrumentation-` or `@prisma/` (npm),
/// `Microsoft.EntityFrameworkCore` / `OpenTelemetry.Instrumentation.*`
/// (`NuGet`). Lower confidence than `SCOPE_RULES`, fires only on
/// prefixes that unambiguously identify the language. Java and Python
/// use the `OTel` convention; Rust tracer names have no usable prefix.
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
        // VENDOR_SCOPE_RULES catches these for CsharpEfCore; this arm
        // is the fallback that routes other .NET scopes to CsharpGeneric.
        if scope == "Microsoft.EntityFrameworkCore"
            || scope.starts_with("Microsoft.EntityFrameworkCore.")
            || scope.starts_with("OpenTelemetry.Instrumentation.")
        {
            return Some(Language::Csharp);
        }
        // Ruby gems emit `OpenTelemetry::Instrumentation::<Lib>` (`::`).
        // ActiveRecord is caught earlier by VENDOR_SCOPE_RULES; this routes
        // the other Ruby scopes (pg/mysql2 drivers, Rack) to RubyGeneric.
        if scope.starts_with("OpenTelemetry::Instrumentation::") {
            return Some(Language::Ruby);
        }
        // PHP native OTel scopes are `io.opentelemetry.contrib.php.<lib>`.
        // Laravel/Doctrine are caught earlier by VENDOR_SCOPE_RULES, this
        // routes the rest (pdo, mongodb, curl, guzzle, ...) to PhpGeneric.
        if scope.starts_with("io.opentelemetry.contrib.php.") {
            return Some(Language::Php);
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
/// empty input; returns the whole string when no separator is present.
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
