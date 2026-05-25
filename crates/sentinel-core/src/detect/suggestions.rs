//! Framework-aware actionable fixes for findings.
//!
//! Enriches detected findings with a [`SuggestedFix`] when the finding's
//! `code_location` lets us infer which framework produced the
//! anti-pattern. v1 covered Java/JPA. v2 added Quarkus reactive +
//! non-reactive, `WebFlux`, Helidon SE/MP, EF Core, Diesel, `SeaORM`. v3
//! (this module's current state) broadens coverage to the seven
//! anti-patterns that previously returned `suggested_fix = None`:
//! `redundant_http`, `slow_sql`, `slow_http`, `excessive_fanout`,
//! `chatty_service`, `pool_saturation`, `serialized_calls`. v3 also
//! adds Python support (`PythonDjango`, `PythonSqlAlchemy`,
//! `PythonGeneric`) with scope detection via the
//! `opentelemetry.instrumentation.*` prefix and the `django.db` /
//! `sqlalchemy` namespace hints. New entries lean on the per-language
//! `*Generic` framework tag when the recommendation is largely
//! framework-agnostic, and reuse a framework-specific tag (e.g.
//! `JavaQuarkus`, `JavaWebFlux`, `PythonDjango`) when the ecosystem
//! ships a canonical primitive worth pointing at.
//!
//! The detection is intentionally cheap and deterministic: we only look
//! at fields already present on [`Finding`] (no span-level
//! access, no extra heap allocations on the hot path), and missing
//! information always degrades gracefully to `suggested_fix = None`.

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
        }
    }
}

/// Pattern for matching a hint against a namespace string.
///
/// `Substring` keeps the existing segment-boundary-aware substring
/// match: the hint must sit between segment delimiters (`.` for Java
/// and C#, `::` for Rust). `LastSegmentEndsWith` matches the suffix of
/// the namespace's last segment only, used for user-code naming
/// conventions like Spring Data's `*Repository` where the framework
/// package itself does not appear in the span's `code.namespace`.
#[derive(Clone, Copy)]
enum Hint {
    Substring(&'static str),
    LastSegmentEndsWith(&'static str),
}

/// Per-language detection tables. Each entry is `(framework, namespace
/// hints)`. Order matters within a language: more-specific frameworks
/// first, user-code conventions and generic last. The detector returns
/// the first match.
///
/// `Substring` hints embed enough of the package path to keep false
/// positives rare. For Rust we anchor on the `::` separator to
/// distinguish `diesel::` from user crates that happen to contain
/// `diesel` in a name. `LastSegmentEndsWith` hints recognise user-code
/// naming conventions when the framework package is not in the
/// span's `code.namespace` (e.g. an OpenTelemetry agent attaches `code.*` to
/// the user's Spring Data repository class).
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

/// OpenTelemetry instrumentation scope rules. The scope name string
/// (e.g. `io.opentelemetry.spring-data-3.0`) is emitted by the agent
/// regardless of how the user names their classes, so this is the most
/// reliable framework signal when available. We match short-name
/// substrings against any scope in the leaf-to-root chain captured by
/// the OTLP ingest walker.
///
/// Order matters: more-specific scopes first. `hibernate-reactive`
/// must win over `hibernate`. `quarkus` (the Quarkus REST/JAX-RS
/// short name) must win over `hibernate` so a Quarkus non-reactive
/// app gets Quarkus-specific advice rather than raw JPA. Helidon and
/// Rust ORMs are not listed here because the upstream agent emits
/// the same `helidon` scope for both SE and MP, and Rust apps name
/// their tracers themselves. Both are better disambiguated through
/// the namespace heuristics below.
const SCOPE_RULES: &[(Framework, &[&str])] = &[
    (Framework::JavaQuarkusReactive, &["hibernate-reactive"]),
    (Framework::JavaQuarkus, &["quarkus"]),
    (Framework::JavaWebFlux, &["spring-webflux", "r2dbc"]),
    (Framework::JavaJpa, &["spring-data", "hibernate"]),
    (Framework::PythonDjango, &["django"]),
    (Framework::PythonSqlAlchemy, &["sqlalchemy"]),
];

/// Match any scope in the chain against any rule. Returns the first
/// rule's framework whose substring list intersects the scope chain.
fn detect_framework_from_scopes(scopes: &[String]) -> Option<Framework> {
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
/// Restricts matches to the canonical `io.opentelemetry.<short>` form
/// emitted by the upstream agent, with optional version suffix
/// (`-1.8`, `-3.0`, ...) and optional sub-scope (`-server`, `-client`,
/// `-mutiny`, ...). Rejects third-party tracer names that happen to
/// contain a needle as a substring (e.g. `com.acme.quarkus-monitoring`
/// would no longer match the `quarkus` rule).
fn scope_matches(scope: &str, needle: &str) -> bool {
    // Java agents emit `io.opentelemetry.<short>`, Python SDK emits
    // `opentelemetry.instrumentation.<short>`. Support both prefixes.
    let Some(rest) = scope
        .strip_prefix("io.opentelemetry.")
        .or_else(|| scope.strip_prefix("opentelemetry.instrumentation."))
    else {
        return false;
    };
    let Some(after) = rest.strip_prefix(needle) else {
        return false;
    };
    // The needle must end at a segment boundary: either the end of
    // the scope name, a version suffix `-...`, or another sub-scope
    // separator. This rejects partial-segment matches like `quarkus`
    // against `quarkus-resteasy-classic` if we ever decide we want
    // only the reactive variant. Today every entry is a full segment,
    // so end-of-string and `-` are the natural anchors.
    after.is_empty() || after.starts_with('-')
}

#[derive(Debug, Clone, Copy)]
enum Language {
    Java,
    Csharp,
    Python,
    Rust,
}

impl Language {
    const fn rules(self) -> &'static [(Framework, &'static [Hint])] {
        match self {
            Self::Java => JAVA_RULES,
            Self::Csharp => CSHARP_RULES,
            Self::Python => PYTHON_RULES,
            Self::Rust => RUST_RULES,
        }
    }

    const fn generic(self) -> Framework {
        match self {
            Self::Java => Framework::JavaGeneric,
            Self::Csharp => Framework::CsharpGeneric,
            Self::Python => Framework::PythonGeneric,
            Self::Rust => Framework::RustGeneric,
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
        CsharpEfCore, CsharpGeneric, JavaGeneric, JavaHelidonMp, JavaHelidonSe, JavaJpa,
        JavaQuarkus, JavaQuarkusReactive, JavaWebFlux, PythonDjango, PythonGeneric,
        PythonSqlAlchemy, RustDiesel, RustGeneric, RustSeaOrm,
    };
    let entries: &[((FindingType, Framework), &str, Option<&str>)] = &[
        // ── Java ───────────────────────────────────────────────────
        (
            (NPlusOneSql, JavaJpa),
            "Use JOIN FETCH on the relationship or annotate the repository \
             method with @EntityGraph to load associations in a single query.",
            Some(
                "https://docs.jboss.org/hibernate/orm/current/userguide/html_single/\
                 Hibernate_User_Guide.html#fetching-strategies-dynamic-fetching",
            ),
        ),
        (
            (RedundantSql, JavaJpa),
            "Add Spring's @Cacheable on the repository or service method, \
             or share the EntityManager within the request via @Transactional \
             so Hibernate's first-level cache deduplicates the read.",
            Some("https://docs.spring.io/spring-framework/reference/integration/cache.html"),
        ),
        (
            (NPlusOneSql, JavaQuarkusReactive),
            "Use Mutiny's Hibernate Reactive Session.fetch() with @NamedEntityGraph, \
             or join the relation in a Panache reactive query, to load associations \
             in a single round-trip.",
            Some("https://quarkus.io/guides/hibernate-reactive"),
        ),
        (
            (NPlusOneHttp, JavaWebFlux),
            "Replace the sequential .flatMap() chain with Flux.merge() or Flux.zip() \
             for parallel execution, or call a batch endpoint that returns the \
             aggregated result in one round-trip.",
            Some("https://docs.spring.io/spring-framework/reference/web/webflux-functional.html"),
        ),
        (
            (NPlusOneHttp, JavaQuarkusReactive),
            "Replace chained Uni.chain() / Multi.onItem().transformToUni() calls with \
             Uni.combine().all().unis(...) for parallel execution, or call a batch \
             endpoint.",
            Some("https://smallrye.io/smallrye-mutiny/latest/guides/combining-items/"),
        ),
        (
            (NPlusOneHttp, JavaGeneric),
            "Coalesce the calls into a batch endpoint, or cache the per-request \
             results with Spring's @Cacheable using a request-scoped cache.",
            Some("https://docs.spring.io/spring-framework/reference/integration/cache.html"),
        ),
        (
            (RedundantSql, JavaQuarkusReactive),
            "Use Quarkus' @CacheResult on the reactive method, or memoize the Uni \
             with Mutiny's .memoize().indefinitely() to deduplicate within a request.",
            Some("https://quarkus.io/guides/cache"),
        ),
        (
            (NPlusOneSql, JavaQuarkus),
            "In Quarkus with Hibernate ORM, use a JOIN FETCH in your JPQL or Panache \
             query, annotate the repository method with @EntityGraph, or call \
             entityManager.unwrap(Session.class).fetchProfile(...) for a named fetch \
             plan.",
            Some("https://quarkus.io/guides/hibernate-orm-panache#fetching-and-loading"),
        ),
        (
            (NPlusOneHttp, JavaQuarkus),
            "Use CompletableFuture.allOf(...) on the Quarkus ManagedExecutor for \
             parallel calls, or invoke a batch endpoint via the Quarkus REST Client. \
             For repeated reads, add @CacheResult on the client method.",
            Some("https://quarkus.io/guides/rest-client-reactive"),
        ),
        (
            (RedundantSql, JavaQuarkus),
            "Add @CacheResult on the @ApplicationScoped service method (Quarkus \
             cache extension), or scope a HashMap on a @RequestScoped bean to \
             deduplicate the query within the request.",
            Some("https://quarkus.io/guides/cache"),
        ),
        (
            (NPlusOneSql, JavaHelidonSe),
            "Replace the per-id loop with a single named Helidon DbClient query \
             that performs JOIN, or pass a list of ids via the :ids JDBC parameter \
             binding. Helidon SE has no JPA layer: the fix happens at the \
             DbClient query level.",
            Some("https://helidon.io/docs/latest/se/dbclient"),
        ),
        (
            (NPlusOneHttp, JavaHelidonSe),
            "Fan out concurrent requests with Helidon WebClient using \
             Single.zip(...) or Multi.merge(...). Or call a batch endpoint that \
             returns the aggregated result in one round-trip.",
            Some("https://helidon.io/docs/latest/se/webclient"),
        ),
        (
            (NPlusOneSql, JavaHelidonMp),
            "Helidon MP entities are JPA-managed under Hibernate. Use \
             @EntityGraph on the repository method or JPQL JOIN FETCH on the \
             relationship to load associations in a single query.",
            Some("https://helidon.io/docs/latest/mp/persistence"),
        ),
        (
            (NPlusOneHttp, JavaHelidonMp),
            "Use the MicroProfile Rest Client with CompletableFuture.allOf(...) \
             on the @ManagedExecutorConfig executor for parallel calls. Or call \
             a batch endpoint that returns the aggregated result in one \
             round-trip.",
            Some(
                "https://download.eclipse.org/microprofile/microprofile-rest-client-3.0/microprofile-rest-client-spec-3.0.html",
            ),
        ),
        (
            (RedundantSql, JavaGeneric),
            "Add a service-level cache (Caffeine, Spring Cache) or deduplicate the \
             query within the request scope.",
            Some("https://docs.spring.io/spring-framework/reference/integration/cache.html"),
        ),
        // ── C# (.NET 8 to 10) ──────────────────────────────────────
        (
            (NPlusOneSql, CsharpEfCore),
            "Use .Include() (and .ThenInclude() for nested relations) to eager-load. \
             Add .AsSplitQuery() when Include causes Cartesian explosion. Consider \
             .AsNoTracking() for read-only queries.",
            Some("https://learn.microsoft.com/en-us/ef/core/querying/related-data/eager"),
        ),
        (
            (RedundantSql, CsharpEfCore),
            "Use IMemoryCache from Microsoft.Extensions.Caching.Memory, or add EF \
             Core's second-level cache via a community extension. Within a request, \
             scope the DbContext so identical reads short-circuit through the change \
             tracker.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/performance/caching/memory"),
        ),
        (
            (NPlusOneHttp, CsharpGeneric),
            "Use Task.WhenAll for parallel independent calls, or call a batch \
             endpoint. For repeated identical calls, configure response caching on \
             HttpClient via DelegatingHandler.",
            Some(
                "https://learn.microsoft.com/en-us/dotnet/api/system.threading.tasks.task.whenall",
            ),
        ),
        // ── Rust ───────────────────────────────────────────────────
        (
            (NPlusOneSql, RustDiesel),
            "Load associations with Diesel's belonging_to + grouped_by pattern \
             (two queries instead of N+1), or use .inner_join() / .left_join() to \
             fetch parent + children in a single query.",
            Some("https://docs.diesel.rs/master/diesel/associations/index.html"),
        ),
        (
            (NPlusOneSql, RustSeaOrm),
            "Use Entity::find().find_with_related(...) or .find_also_related(...) to \
             fetch related entities in a single query, or load with a JOIN via \
             QuerySelect::join().",
            Some("https://www.sea-ql.org/SeaORM/docs/relation/select-related/"),
        ),
        (
            (RedundantSql, RustDiesel),
            "Cache the result with the moka crate, or scope-deduplicate via a \
             request-local OnceCell stored in axum/actix-web extensions.",
            Some("https://docs.rs/moka"),
        ),
        (
            (RedundantSql, RustSeaOrm),
            "Cache the result with the moka crate, or memoize per-request via a \
             OnceCell stored in your handler state.",
            Some("https://docs.rs/moka"),
        ),
        (
            (NPlusOneHttp, RustGeneric),
            "Use tokio::join! or futures::future::join_all for parallel independent \
             calls. Switch to a batch endpoint when the calls fan out from the same \
             upstream input.",
            Some("https://docs.rs/tokio/latest/tokio/macro.join.html"),
        ),
        // ── Python ────────────────────────────────────────────────
        (
            (NPlusOneSql, PythonDjango),
            "Use select_related() for foreign-key joins or prefetch_related() for \
             reverse/M2M relations to eager-load associations in one or two queries \
             instead of N+1.",
            Some("https://docs.djangoproject.com/en/5.2/ref/models/querysets/#select-related"),
        ),
        (
            (NPlusOneSql, PythonSqlAlchemy),
            "Add joinedload() or subqueryload() in the query options to eager-load \
             the relationship, or rewrite with an explicit join().",
            Some(
                "https://docs.sqlalchemy.org/en/21/orm/queryguide/relationships.html#joined-eager-loading",
            ),
        ),
        (
            (RedundantSql, PythonDjango),
            "Cache the queryset result with Django's cache framework (@cache_page \
             for views, cache.get/set for manual memoization), or share the result \
             via a request-local variable to deduplicate within the request.",
            Some("https://docs.djangoproject.com/en/5.2/topics/cache/"),
        ),
        (
            (RedundantSql, PythonSqlAlchemy),
            "Use dogpile.cache or a scoped-session memoization pattern to \
             deduplicate identical queries within the request. Share the result \
             via the session's identity map when the same row is loaded twice.",
            Some(
                "https://docs.sqlalchemy.org/en/21/orm/session_basics.html#is-the-session-a-cache",
            ),
        ),
        (
            (NPlusOneHttp, PythonGeneric),
            "Use asyncio.gather() or concurrent.futures.ThreadPoolExecutor for \
             parallel independent calls. Switch to a batch endpoint when the calls \
             fan out from the same upstream input.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.gather"),
        ),
        // ── redundant_http ─────────────────────────────────────────
        (
            (RedundantHttp, JavaGeneric),
            "Wrap the HTTP client in a request-scoped memoization layer (Caffeine, \
             a HashMap on a request-scoped bean, or Spring's @Cacheable with a \
             request-scoped key) so identical calls return the cached response \
             within the request.",
            Some("https://github.com/ben-manes/caffeine"),
        ),
        (
            (RedundantHttp, CsharpEfCore),
            "Insert a DelegatingHandler on HttpClient that memoizes responses by \
             request key (URL + headers) inside IMemoryCache for the request's \
             lifetime. Be explicit about the cache key to avoid serving stale \
             data across users.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/performance/caching/memory"),
        ),
        (
            (RedundantHttp, CsharpGeneric),
            "Add a DelegatingHandler on HttpClient that memoizes by request URI \
             inside IMemoryCache for the request scope, or share the response via \
             AsyncLazy<T> when the duplication is concurrent.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/performance/caching/memory"),
        ),
        (
            (RedundantHttp, RustGeneric),
            "Memoize per-request with the moka crate or a request-local OnceCell \
             stored in your handler state. For concurrent duplicates within one \
             handler, share the in-flight future via futures::future::Shared.",
            Some("https://docs.rs/moka"),
        ),
        (
            (RedundantHttp, PythonGeneric),
            "Memoize per-request with functools.lru_cache (sync) or an async \
             memoization decorator. For Django, wrap the view with @cache_page or \
             share the response via a request-local dict.",
            Some("https://docs.python.org/3/library/functools.html#functools.lru_cache"),
        ),
        // ── slow_sql ───────────────────────────────────────────────
        (
            (SlowSql, JavaJpa),
            "Profile the rendered SQL with EXPLAIN ANALYZE. Common JPA fixes: add \
             a composite index matching the WHERE + ORDER BY columns, switch to \
             a DTO projection via @Query(\"select new ...\") to avoid hydrating \
             the full entity graph, or paginate with Slice/keyset pagination when \
             OFFSET grows linearly.",
            Some("https://docs.spring.io/spring-data/jpa/reference/jpa/query-methods.html"),
        ),
        (
            (SlowSql, JavaQuarkus),
            "Enable hibernate.generate_statistics + the slow query log to confirm \
             the offender. Add a composite index, project to a DTO via Panache's \
             .project(Class) or a native query, or paginate with .range(...) on \
             a keyset-friendly column.",
            Some("https://quarkus.io/guides/hibernate-orm-panache"),
        ),
        (
            (SlowSql, JavaGeneric),
            "Profile with EXPLAIN ANALYZE. Add an index on the columns used in \
             WHERE + ORDER BY when selectivity justifies it. If the plan is \
             dominated by a nested loop on a large outer, rewrite to push the \
             selective predicate first.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowSql, CsharpEfCore),
            "Inspect the generated SQL via .ToQueryString() and EXPLAIN ANALYZE \
             it. Add an index via Fluent API (HasIndex). For Include explosion, \
             switch to .AsSplitQuery(). For read-only paths add .AsNoTracking() \
             to remove change-tracker overhead.",
            Some("https://learn.microsoft.com/en-us/ef/core/performance/efficient-querying"),
        ),
        (
            (SlowSql, CsharpGeneric),
            "Run EXPLAIN ANALYZE against the rendered query and add an index when \
             the plan shows a sequential scan over a large table. Prefer parameterized \
             queries so the plan cache stays hot.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowSql, RustDiesel),
            "Print the rendered SQL with diesel::debug_query, EXPLAIN ANALYZE it, \
             and add the missing index. For wide rows, use a .select((col_a, col_b)) \
             projection so postgres can use an index-only scan.",
            Some("https://docs.diesel.rs/master/diesel/fn.debug_query.html"),
        ),
        (
            (SlowSql, RustSeaOrm),
            "Capture the query via the SQL logger feature, EXPLAIN ANALYZE the \
             output, and add an index when the plan does a sequential scan. \
             Project to a partial model with .select_only().column(...) when \
             only a few columns are read.",
            Some("https://www.sea-ql.org/SeaORM/docs/index/"),
        ),
        (
            (SlowSql, RustGeneric),
            "Run EXPLAIN ANALYZE on the slow query. Add a composite index \
             matching the WHERE + ORDER BY columns, or rewrite to push the \
             selective predicate before any join.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        (
            (SlowSql, PythonDjango),
            "Run EXPLAIN ANALYZE on the rendered query (django-debug-toolbar or \
             connection.queries). Add a db_index=True on the model field, or use \
             .only()/.defer() to limit fetched columns.",
            Some("https://docs.djangoproject.com/en/5.2/ref/models/options/#indexes"),
        ),
        (
            (SlowSql, PythonSqlAlchemy),
            "Capture the rendered SQL via echo=True and EXPLAIN ANALYZE it. Add \
             an index via Index() in the table metadata, or project with \
             .with_only_columns() to reduce transferred data.",
            Some("https://docs.sqlalchemy.org/en/21/core/metadata.html#sqlalchemy.schema.Index"),
        ),
        (
            (SlowSql, PythonGeneric),
            "Run EXPLAIN ANALYZE on the slow query. Add an index on the WHERE + \
             ORDER BY columns, or rewrite to push the selective predicate first.",
            Some("https://www.postgresql.org/docs/current/using-explain.html"),
        ),
        // ── slow_http ──────────────────────────────────────────────
        (
            (SlowHttp, JavaGeneric),
            "Profile the upstream's own slow_sql / slow_http findings first \
             (the latency is usually upstream-side). On the client, wrap the \
             call in a Resilience4j circuit breaker with a tight timeout, and \
             cache the response when staleness tolerance allows.",
            Some("https://resilience4j.readme.io/docs/circuitbreaker"),
        ),
        (
            (SlowHttp, CsharpGeneric),
            "Set a tight HttpClient.Timeout and wrap calls in a Polly retry + \
             circuit breaker. If the latency is structural (slow upstream), \
             cache the response with IMemoryCache or move the call off the \
             request hot path via background hosted services.",
            Some(
                "https://learn.microsoft.com/en-us/dotnet/architecture/microservices/implement-resilient-applications/",
            ),
        ),
        (
            (SlowHttp, RustGeneric),
            "Set a per-request timeout on reqwest / hyper / surf, wrap with the \
             tower circuit-breaker layer, and cache the response with moka when \
             staleness is acceptable.",
            Some("https://docs.rs/tower"),
        ),
        (
            (SlowHttp, PythonGeneric),
            "Set a per-request timeout on httpx / aiohttp, add a tenacity retry \
             with exponential backoff, and cache the response with Django cache \
             or dogpile.cache when staleness is acceptable.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.wait_for"),
        ),
        // ── excessive_fanout ───────────────────────────────────────
        (
            (ExcessiveFanout, JavaWebFlux),
            "Bound the fan-out width with Flux.flatMap(concurrency = N) (default \
             concurrency is unbounded). Add a Resilience4j bulkhead so a slow \
             upstream cannot saturate the reactor scheduler.",
            Some("https://projectreactor.io/docs/core/release/reference/#which-operator"),
        ),
        (
            (ExcessiveFanout, JavaQuarkusReactive),
            "Bound the fan-out with Multi.onItem().transformToUniAndConcatenate() \
             when ordering matters, or .transformToUniAndMerge(concurrency = N) \
             when independent. Add a Resilience4j bulkhead on the downstream \
             client.",
            Some("https://smallrye.io/smallrye-mutiny/latest/guides/combining-items/"),
        ),
        (
            (ExcessiveFanout, JavaGeneric),
            "Replace the fan-out with a single bulk endpoint when the downstream \
             supports it. Otherwise apply the bulkhead pattern (Resilience4j or a \
             dedicated thread pool) to bound the blast radius of a slow \
             dependency.",
            Some("https://resilience4j.readme.io/docs/bulkhead"),
        ),
        (
            (ExcessiveFanout, CsharpGeneric),
            "Cap parallelism with Parallel.ForEachAsync(MaxDegreeOfParallelism = N) \
             or a SemaphoreSlim, and prefer a batch endpoint when the upstream \
             offers one. Layer Polly's RateLimiter strategy on the HttpClient \
             to enforce a hard ceiling (Polly v8 subsumes the v7 Bulkhead).",
            Some("https://www.pollydocs.org/strategies/rate-limiter.html"),
        ),
        (
            (ExcessiveFanout, RustGeneric),
            "Use futures::stream::iter(...).buffer_unordered(N) to cap concurrent \
             requests, or replace the fan-out with a batch endpoint. Layer a \
             tower::ConcurrencyLimit on the downstream client to enforce a hard \
             ceiling.",
            Some(
                "https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html#method.buffer_unordered",
            ),
        ),
        (
            (ExcessiveFanout, PythonGeneric),
            "Cap parallelism with asyncio.Semaphore or \
             concurrent.futures.ThreadPoolExecutor(max_workers=N), and prefer a \
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
             round-trip. As a stopgap, fan-in with Task.WhenAll and an \
             AsyncLazy<T> per-key cache.",
            Some("https://learn.microsoft.com/en-us/aspnet/core/grpc/protobuf"),
        ),
        (
            (ChattyService, RustGeneric),
            "Coalesce the calls behind a single bulk endpoint, or expose a \
             tonic streaming RPC that returns the aggregated payload. As an \
             intermediate, batch with futures::future::join_all and a moka \
             per-key cache.",
            Some("https://docs.rs/tonic"),
        ),
        (
            (ChattyService, PythonGeneric),
            "Coalesce the chatty interactions into a single bulk endpoint, or \
             fan-in with asyncio.gather and a per-key cache (Django cache or \
             dogpile.cache). Reduce round-trips by moving orchestration upstream.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.gather"),
        ),
        // ── pool_saturation ────────────────────────────────────────
        (
            (PoolSaturation, JavaQuarkus),
            "Raise quarkus.datasource.jdbc.max-size only after confirming the \
             root cause: usually slow queries hold connections too long. Profile \
             slow_sql findings on the same service first, then size the pool to \
             handle the corrected workload.",
            Some("https://quarkus.io/guides/datasource#jdbc-configuration-reference"),
        ),
        (
            (PoolSaturation, JavaGeneric),
            "Inspect slow_sql findings on the same service: pool saturation is \
             usually a symptom, not the disease. After speeding up the slow \
             queries, raise the HikariCP maximumPoolSize only if the corrected \
             workload still saturates.",
            Some("https://github.com/brettwooldridge/HikariCP/wiki/About-Pool-Sizing"),
        ),
        (
            (PoolSaturation, CsharpEfCore),
            "Check Npgsql Max Pool Size in the connection string and the \
             DbContext lifetime (a per-request DbContext should release \
             connections to the pool on Dispose). Slow queries on the same \
             service usually drive the saturation, fix those first.",
            Some("https://www.npgsql.org/doc/connection-string-parameters.html"),
        ),
        (
            (PoolSaturation, CsharpGeneric),
            "Profile the slow_sql findings on the same service; pool saturation \
             follows from connections being held during slow work. Raise the \
             pool size only after the slow path is corrected.",
            Some(
                "https://learn.microsoft.com/en-us/sql/connect/ado-net/sql-server-connection-pooling",
            ),
        ),
        (
            (PoolSaturation, RustGeneric),
            "Inspect slow_sql findings first: sqlx and deadpool pools usually \
             saturate because connections are held during slow work. Tune \
             max_connections on PoolOptions only after the slow queries are \
             addressed.",
            Some("https://docs.rs/sqlx/latest/sqlx/pool/struct.PoolOptions.html"),
        ),
        (
            (PoolSaturation, PythonGeneric),
            "Inspect slow_sql findings on the same service: pool saturation usually \
             follows from connections held during slow work. Tune pool_size on \
             SQLAlchemy's create_engine or Django's CONN_MAX_AGE only after the \
             slow path is corrected.",
            Some(
                "https://docs.sqlalchemy.org/en/21/core/engines.html#sqlalchemy.create_engine.params.pool_size",
            ),
        ),
        // ── serialized_calls ───────────────────────────────────────
        (
            (SerializedCalls, JavaWebFlux),
            "Replace the sequential Mono.flatMap chain with Mono.zip(m1, m2, ...) \
             or Flux.merge(...) when the calls are independent. Keep the \
             sequential chain only when one call's output feeds the next.",
            Some("https://projectreactor.io/docs/core/release/reference/#which.combining"),
        ),
        (
            (SerializedCalls, JavaQuarkusReactive),
            "Switch the sequential Uni.chain() to \
             Uni.combine().all().unis(u1, u2, ...).asTuple() when the calls do \
             not depend on each other. Drop back to chain only when the \
             next call needs the previous result.",
            Some("https://smallrye.io/smallrye-mutiny/latest/guides/combining-items/"),
        ),
        (
            (SerializedCalls, JavaGeneric),
            "Parallelize independent calls with CompletableFuture.allOf(...) on \
             the ManagedExecutor (Quarkus) or @Async (Spring). Keep the calls \
             serial only when one's output feeds the next.",
            Some(
                "https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/concurrent/CompletableFuture.html",
            ),
        ),
        (
            (SerializedCalls, CsharpGeneric),
            "Replace the sequential await chain with Task.WhenAll(t1, t2, ...) \
             when the calls do not depend on each other. Use ValueTask for \
             completed-synchronously paths to avoid allocating Task wrappers.",
            Some(
                "https://learn.microsoft.com/en-us/dotnet/api/system.threading.tasks.task.whenall",
            ),
        ),
        (
            (SerializedCalls, RustGeneric),
            "Replace sequential .await chains with tokio::join!(f1, f2, ...) or \
             futures::future::join_all(iter) when the calls are independent. \
             Use try_join! when any failure should short-circuit the rest.",
            Some("https://docs.rs/tokio/latest/tokio/macro.join.html"),
        ),
        (
            (SerializedCalls, PythonGeneric),
            "Replace sequential awaits with asyncio.gather(t1, t2, ...) when the \
             calls are independent. For sync code, use \
             concurrent.futures.ThreadPoolExecutor to run independent calls in \
             parallel. Keep the sequential form only when one call's output feeds \
             the next.",
            Some("https://docs.python.org/3/library/asyncio-task.html#asyncio.gather"),
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

/// Pure framework detector. Inspects three signals in order:
///
/// 1. **Instrumentation scope chain** captured at OTLP ingest time
///    (`io.opentelemetry.spring-data-3.0`, `io.opentelemetry.hibernate-6.0`,
///    etc.). Most reliable, agent-emitted, naming-quirk-immune.
/// 2. **`code_location` namespace** with filepath-derived language.
///    Returns the language-generic fallback when no namespace rule
///    matches but the language is known.
/// 3. **`code_location` namespace alone** when filepath is absent.
///    Tries every language's rules in order (Java, C#, Rust) and
///    returns the first hit. No generic fallback in this path
///    because we cannot know which language to fall back to.
///
/// `None` when no signal is available.
fn detect_framework(finding: &Finding) -> Option<Framework> {
    if let Some(framework) = detect_framework_from_scopes(&finding.instrumentation_scopes) {
        return Some(framework);
    }
    let loc = finding.code_location.as_ref()?;
    let ns = loc.namespace.as_deref().unwrap_or("");
    if let Some(filepath) = loc.filepath.as_deref() {
        let language = language_from_filepath(filepath)?;
        return Some(match_namespace_against_language(ns, language).unwrap_or(language.generic()));
    }
    if ns.is_empty() {
        return None;
    }
    [
        Language::Java,
        Language::Csharp,
        Language::Python,
        Language::Rust,
    ]
    .into_iter()
    .find_map(|language| match_namespace_against_language(ns, language))
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

/// Segment-boundary-aware substring match. Returns `true` when `hint`
/// appears between segment boundaries on both sides: it must start at
/// `ns` start or immediately after a `.` (Java, C#) / `::` (Rust), and
/// must end at `ns` end or immediately before another segment delimiter
/// (`.` or `::`). Prevents false positives in both directions:
/// `orders::mydiesel::query` on `diesel::` (leading boundary) and
/// `io.helidongrpc.Foo` on `io.helidon` (trailing boundary).
///
/// Advances `start` by `hint.len()` after a non-matching candidate so we
/// skip overlapping re-scans (the same hint can never match twice over a
/// single occurrence) and so we always land on a `char` boundary, since
/// `str::find` returns indices aligned to the start of the matched
/// substring.
fn namespace_contains_segment(ns: &str, hint: &str) -> bool {
    let bytes = ns.as_bytes();
    let mut start = 0;
    while let Some(found) = ns[start..].find(hint) {
        let abs = start + found;
        let end = abs + hint.len();

        let leading_ok = abs == 0
            || bytes[abs - 1] == b'.'
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
            || (bytes[end] == b':' && end + 1 < ns.len() && bytes[end + 1] == b':');

        if leading_ok && trailing_ok {
            return true;
        }
        start = end;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{FindingType, Severity};
    use crate::event::CodeLocation;
    use crate::test_helpers::make_finding;

    fn finding_with_location(ft: FindingType, loc: Option<CodeLocation>) -> Finding {
        let mut f = make_finding(ft, Severity::Warning);
        f.code_location = loc;
        f.suggested_fix = None;
        f
    }

    fn finding_with_scopes(ft: FindingType, scopes: &[&str]) -> Finding {
        let mut f = make_finding(ft, Severity::Warning);
        f.code_location = None;
        f.instrumentation_scopes = scopes.iter().map(|s| (*s).to_string()).collect();
        f.suggested_fix = None;
        f
    }

    fn loc(filepath: &str, namespace: Option<&str>) -> CodeLocation {
        CodeLocation {
            function: None,
            filepath: Some(filepath.to_string()),
            lineno: None,
            namespace: namespace.map(ToString::to_string),
        }
    }

    // ── Java framework detection ─────────────────────────────────

    #[test]
    fn detects_java_jpa_via_jakarta_persistence() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "src/main/java/com/example/OrderRepository.java",
                Some("jakarta.persistence.EntityManager"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn detects_java_jpa_via_hibernate() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "src/main/java/com/example/OrderRepository.java",
                Some("org.hibernate.SessionImpl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn detects_java_jpa_via_spring_data_jpa() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("org.springframework.data.jpa.repository.JpaRepository"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn detects_java_webflux_via_reactor() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "src/main/java/com/example/UserClient.java",
                Some("reactor.core.publisher.Flux"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaWebFlux));
    }

    #[test]
    fn detects_java_webflux_via_spring_reactive() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "UserHandler.java",
                Some("org.springframework.web.reactive.function.client.WebClient"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaWebFlux));
    }

    #[test]
    fn detects_java_quarkus_reactive_via_mutiny() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "src/main/java/com/acme/UserService.java",
                Some("io.smallrye.mutiny.Uni"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
    }

    #[test]
    fn detects_java_quarkus_reactive_via_hibernate_reactive() {
        // org.hibernate.reactive contains "org.hibernate" but is more
        // specific. The Quarkus rule must win over the JPA rule.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("org.hibernate.reactive.session.impl.ReactiveSessionImpl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
    }

    #[test]
    fn detects_java_quarkus_reactive_via_quarkus_namespace() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("io.quarkus.hibernate.reactive.panache.PanacheRepository"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
    }

    #[test]
    fn detects_java_quarkus_reactive_via_panache_reactive_subpackage() {
        // The panache.reactive sub-package is reactive even though it does
        // not embed "hibernate.reactive". The dedicated hint catches it.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("io.quarkus.panache.reactive.PanacheRepository"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
    }

    #[test]
    fn detects_java_quarkus_non_reactive_via_hibernate_orm() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("io.quarkus.hibernate.orm.runtime.session.SessionImpl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkus));
    }

    #[test]
    fn detects_java_quarkus_non_reactive_via_panache_common() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("io.quarkus.panache.common.runtime.AbstractJpaOperations"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkus));
    }

    #[test]
    fn detects_java_quarkus_non_reactive_via_generic_quarkus_namespace() {
        // A general `io.quarkus.scheduler` (or any non-reactive Quarkus
        // sub-package) routes to the non-reactive variant. Reactive's
        // catch-all was removed precisely so this case lands here.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "Scheduler.java",
                Some("io.quarkus.scheduler.runtime.SchedulerImpl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkus));
    }

    #[test]
    fn quarkus_reactive_wins_over_non_reactive_on_overlap() {
        // Both rules could plausibly match `io.quarkus.hibernate.reactive...`
        // (it contains both "io.quarkus.hibernate.reactive" and "io.quarkus").
        // Reactive comes first in JAVA_RULES, so it must win.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("io.quarkus.hibernate.reactive.runtime.ReactiveSessionImpl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
    }

    #[test]
    fn detects_java_helidon_se_via_dbclient() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderService.java",
                Some("io.helidon.dbclient.jdbc.JdbcExecute"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaHelidonSe));
    }

    #[test]
    fn detects_java_helidon_se_via_webclient() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "UserClient.java",
                Some("io.helidon.webclient.WebClient"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaHelidonSe));
    }

    #[test]
    fn detects_java_helidon_mp_via_microprofile_namespace() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "UserResource.java",
                Some("io.helidon.microprofile.server.ServerImpl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaHelidonMp));
    }

    #[test]
    fn helidon_mp_wins_over_helidon_se_on_overlap() {
        // `io.helidon.microprofile.*` is a sub-package of `io.helidon`.
        // MP rule comes first so MP must win.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "UserRepository.java",
                Some("io.helidon.microprofile.cdi.HelidonContainerImpl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaHelidonMp));
    }

    #[test]
    fn falls_back_to_java_generic_without_framework_hint() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "src/main/java/com/example/UserClient.java",
                Some("com.example.UserClient"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaGeneric));
    }

    #[test]
    fn case_insensitive_java_extension() {
        let f = finding_with_location(FindingType::NPlusOneSql, Some(loc("Repository.JAVA", None)));
        assert_eq!(detect_framework(&f), Some(Framework::JavaGeneric));
    }

    // ── C# framework detection ───────────────────────────────────

    #[test]
    fn detects_csharp_ef_core() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "src/Orders/Repositories/OrderRepository.cs",
                Some("Microsoft.EntityFrameworkCore.DbSet"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::CsharpEfCore));
    }

    #[test]
    fn detects_csharp_ef_core_via_pomelo_provider() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.cs",
                Some("Pomelo.EntityFrameworkCore.MySql.Query.Internal"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::CsharpEfCore));
    }

    #[test]
    fn falls_back_to_csharp_generic_without_ef_hint() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "src/Orders/UserClient.cs",
                Some("Acme.Orders.UserClient"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::CsharpGeneric));
    }

    // ── Rust framework detection ─────────────────────────────────

    #[test]
    fn detects_rust_diesel() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "crates/orders/src/repository.rs",
                Some("diesel::query_dsl::methods::FilterDsl"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::RustDiesel));
    }

    #[test]
    fn detects_rust_sea_orm() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "crates/orders/src/repository.rs",
                Some("sea_orm::query::Selector"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::RustSeaOrm));
    }

    #[test]
    fn rust_diesel_hint_does_not_match_user_module_named_diesel() {
        // `mydiesel::query` should NOT match because we anchor on `diesel::`
        // (with the separator). False positives on user crates that happen
        // to contain "diesel" in a name would be noisy.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "crates/orders/src/mydiesel.rs",
                Some("orders::mydiesel::query"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::RustGeneric));
    }

    #[test]
    fn falls_back_to_rust_generic_without_orm_hint() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "crates/orders/src/user_client.rs",
                Some("orders::user_client"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::RustGeneric));
    }

    #[test]
    fn java_hint_requires_trailing_segment_boundary() {
        // Regression: `io.helidon` must not match `io.helidongrpc.Foo`,
        // `org.hibernate` must not match `org.hibernatefoo.Bar`. Prior
        // impl only checked the leading boundary.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("src/main/java/Foo.java", Some("io.helidongrpc.Foo"))),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaGeneric));

        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("src/main/java/Bar.java", Some("org.hibernatefoo.Bar"))),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaGeneric));
    }

    #[test]
    fn csharp_hint_requires_trailing_segment_boundary() {
        // Regression: `Microsoft.EntityFrameworkCore` must not match
        // `Microsoft.EntityFrameworkCoreCache.Provider`.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "src/Repo.cs",
                Some("Microsoft.EntityFrameworkCoreCache.Provider"),
            )),
        );
        assert_eq!(detect_framework(&f), Some(Framework::CsharpGeneric));
    }

    // ── Scope-based detection (OpenTelemetry instrumentation scope) ─

    #[test]
    fn scope_detects_jpa_from_spring_data() {
        // Lab case at the wire level: leaf JDBC span, parent
        // Spring Data span. Walker captured both scope names.
        let f = finding_with_scopes(
            FindingType::RedundantSql,
            &[
                "io.opentelemetry.jdbc",
                "io.opentelemetry.hibernate-6.0",
                "io.opentelemetry.spring-data-3.0",
            ],
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn scope_detects_jpa_from_hibernate_alone() {
        let f = finding_with_scopes(
            FindingType::NPlusOneSql,
            &["io.opentelemetry.jdbc", "io.opentelemetry.hibernate-6.0"],
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn scope_detects_quarkus_reactive_via_hibernate_reactive() {
        // hibernate-reactive must win over plain hibernate.
        let f = finding_with_scopes(
            FindingType::NPlusOneSql,
            &[
                "io.opentelemetry.hibernate-reactive-1.0",
                "io.opentelemetry.quarkus-resteasy-reactive-3.0",
            ],
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
    }

    #[test]
    fn scope_detects_quarkus_non_reactive_via_quarkus_short_name() {
        // Non-reactive Quarkus emits scope "quarkus" on REST spans
        // and "hibernate" on DB spans. Quarkus rule ordered before
        // JPA so we get JavaQuarkus, not JavaJpa.
        let f = finding_with_scopes(
            FindingType::NPlusOneSql,
            &[
                "io.opentelemetry.jdbc",
                "io.opentelemetry.hibernate-6.0",
                "io.opentelemetry.quarkus-resteasy-reactive-3.0",
            ],
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkus));
    }

    #[test]
    fn scope_detects_webflux_via_r2dbc() {
        let f = finding_with_scopes(
            FindingType::NPlusOneSql,
            &[
                "io.opentelemetry.r2dbc-1.0",
                "io.opentelemetry.spring-webflux-5.0",
            ],
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaWebFlux));
    }

    #[test]
    fn scope_detects_webflux_via_spring_webflux() {
        let f = finding_with_scopes(
            FindingType::NPlusOneHttp,
            &["io.opentelemetry.spring-webflux-5.0"],
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaWebFlux));
    }

    #[test]
    fn scope_wins_over_namespace_user_code() {
        // When both signals are present, scope should win because
        // it is more reliable than the user-class-name suffix.
        let mut f = finding_with_scopes(
            FindingType::RedundantSql,
            &["io.opentelemetry.spring-data-3.0"],
        );
        f.code_location = Some(loc(
            "OrderRepository.java",
            Some("com.example.OrderRepository"),
        ));
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn scope_falls_back_to_namespace_when_no_scope_rule_matches() {
        // Scope is jdbc-only (no framework hint there). Detection
        // falls through to the namespace path and picks up the
        // Hibernate substring.
        let mut f = finding_with_scopes(FindingType::NPlusOneSql, &["io.opentelemetry.jdbc"]);
        f.code_location = Some(loc("Repository.java", Some("org.hibernate.SessionImpl")));
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn scope_falls_back_to_namespace_when_scope_chain_empty() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "Repository.java",
                Some("org.springframework.data.jpa.repository.JpaRepository"),
            )),
        );
        // empty scopes by default
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn scope_unknown_falls_back_to_namespace() {
        // Unknown scope name (synthetic or third-party tracer):
        // detector skips scope rules and uses namespace.
        let mut f = finding_with_scopes(FindingType::NPlusOneSql, &["com.example.custom-tracer"]);
        f.code_location = Some(loc("Repository.java", Some("org.hibernate.SessionImpl")));
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn scope_third_party_tracer_named_after_framework_does_not_match() {
        // A third-party tracer like `com.acme.quarkus-monitoring` contains
        // the substring `quarkus` but is not under the `io.opentelemetry.`
        // prefix, so the boundary-aware matcher refuses it. Without this
        // guard we would fire JavaQuarkus on any user library that happens
        // to embed a framework name.
        let f = finding_with_scopes(FindingType::NPlusOneSql, &["com.acme.quarkus-monitoring"]);
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn scope_partial_segment_does_not_match() {
        // `quarkus` must end at a segment boundary (end of string or `-`).
        // `quarkusextension-1.0` should not match the `quarkus` rule.
        let f = finding_with_scopes(
            FindingType::NPlusOneSql,
            &["io.opentelemetry.quarkusextension-1.0"],
        );
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn scope_matches_canonical_versioned_form() {
        // The canonical agent form is `io.opentelemetry.<short>-<version>`.
        let f = finding_with_scopes(
            FindingType::NPlusOneSql,
            &["io.opentelemetry.spring-data-3.0"],
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn scope_matches_canonical_bare_form() {
        // Bare canonical form (no version) is also accepted.
        let f = finding_with_scopes(FindingType::NPlusOneSql, &["io.opentelemetry.spring-data"]);
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    // ── Cross-language fallthrough ───────────────────────────────

    #[test]
    fn returns_python_django_for_py_extension_with_django_namespace() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("repo.py", Some("django.db.models"))),
        );
        assert_eq!(detect_framework(&f), Some(Framework::PythonDjango));
    }

    #[test]
    fn returns_python_sqlalchemy_for_py_extension_with_sqlalchemy_namespace() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("models.py", Some("sqlalchemy.orm.session"))),
        );
        assert_eq!(detect_framework(&f), Some(Framework::PythonSqlAlchemy));
    }

    #[test]
    fn returns_python_generic_for_py_extension_with_unknown_namespace() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("handlers.py", Some("myapp.orders.views"))),
        );
        assert_eq!(detect_framework(&f), Some(Framework::PythonGeneric));
    }

    #[test]
    fn returns_python_django_from_otel_scope_without_filepath() {
        let mut f = finding_with_location(FindingType::NPlusOneSql, None);
        f.instrumentation_scopes = vec!["opentelemetry.instrumentation.django".to_string()];
        assert_eq!(detect_framework(&f), Some(Framework::PythonDjango));
    }

    #[test]
    fn returns_none_for_unsupported_extension() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("query.rb", Some("ActiveRecord::Base"))),
        );
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn returns_none_when_code_location_missing() {
        let f = finding_with_location(FindingType::NPlusOneSql, None);
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn detects_framework_via_namespace_when_filepath_absent() {
        // OpenTelemetry agents often emit `code.namespace` on a parent span
        // without `code.filepath`. When the namespace alone is
        // recognised, we return the matching framework instead of
        // bailing out.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("findById".to_string()),
                filepath: None,
                lineno: Some(7),
                namespace: Some("org.hibernate.SessionImpl".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn returns_none_when_filepath_absent_and_namespace_unrecognized() {
        // Without a filepath we cannot identify the language, so an
        // unrecognised namespace must yield None rather than guessing.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("processPayment".to_string()),
                filepath: None,
                lineno: None,
                namespace: Some("custom.PaymentEngine".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn detects_jpa_from_user_repository_class_without_filepath() {
        // Lab case: OTel Java agent attaches `code.namespace` to the
        // user's Spring Data repository (e.g.
        // `com.perfsim.order.domain.OrderRepository`). The suffix
        // `Repository` flags this as JPA without needing the
        // framework package to appear in the namespace.
        let f = finding_with_location(
            FindingType::RedundantSql,
            Some(CodeLocation {
                function: Some("slowQuery".to_string()),
                filepath: None,
                lineno: None,
                namespace: Some("com.perfsim.order.domain.OrderRepository".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn detects_jpa_from_user_dao_class_without_filepath() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("findAll".to_string()),
                filepath: None,
                lineno: None,
                namespace: Some("com.example.legacy.OrderDao".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn user_code_suffix_does_not_match_unrelated_class_without_filepath() {
        // `HttpClientWrapper` ends with neither Repository, Repo nor
        // Dao, so we must not mis-tag this finding as JPA. With no
        // filepath we cannot infer the language, so the result is
        // None (no language-generic guess).
        let f = finding_with_location(
            FindingType::RedundantSql,
            Some(CodeLocation {
                function: Some("send".to_string()),
                filepath: None,
                lineno: None,
                namespace: Some("com.example.HttpClientWrapper".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn enrich_populates_jpa_fix_for_user_repository_without_filepath() {
        // End-to-end: the lab's redundant_sql finding with only a
        // user-code namespace (no filepath, no framework package)
        // must come out with `framework: java_jpa` and a usable
        // recommendation.
        let mut findings = vec![finding_with_location(
            FindingType::RedundantSql,
            Some(CodeLocation {
                function: Some("slowQuery".to_string()),
                filepath: None,
                lineno: None,
                namespace: Some("com.perfsim.order.domain.OrderRepository".to_string()),
            }),
        )];
        enrich(&mut findings);
        let fix = findings[0]
            .suggested_fix
            .as_ref()
            .expect("expected suggested_fix to be set");
        assert_eq!(fix.framework, "java_jpa");
        assert_eq!(fix.pattern, "redundant_sql");
        assert!(
            fix.recommendation.contains("@Cacheable")
                || fix.recommendation.contains("EntityManager"),
            "redundant_sql JPA fix should reference @Cacheable or EntityManager"
        );
    }

    // ── Lookup table ─────────────────────────────────────────────

    #[test]
    fn lookup_table_returns_jpa_fix_for_n_plus_one_sql() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("Repository.java", Some("org.hibernate.SessionImpl"))),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_jpa");
        assert_eq!(fix.pattern, "n_plus_one_sql");
        assert!(fix.recommendation.contains("JOIN FETCH"));
        assert!(fix.reference_url.is_some());
    }

    #[test]
    fn lookup_table_returns_csharp_ef_core_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.cs",
                Some("Microsoft.EntityFrameworkCore.DbSet"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "csharp_ef_core");
        assert!(fix.recommendation.contains(".Include()"));
    }

    #[test]
    fn lookup_table_returns_rust_diesel_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "src/repo.rs",
                Some("diesel::query_dsl::methods::FilterDsl"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "rust_diesel");
        assert!(fix.recommendation.contains("belonging_to"));
    }

    #[test]
    fn lookup_table_returns_rust_sea_orm_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("src/repo.rs", Some("sea_orm::query::Selector"))),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "rust_sea_orm");
        assert!(fix.recommendation.contains("find_with_related"));
    }

    #[test]
    fn lookup_table_returns_quarkus_reactive_http_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc("UserService.java", Some("io.smallrye.mutiny.Uni"))),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_quarkus_reactive");
        assert!(fix.recommendation.contains("Uni.combine()"));
    }

    #[test]
    fn lookup_table_returns_webflux_http_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc("UserHandler.java", Some("reactor.core.publisher.Flux"))),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_webflux");
        assert!(
            fix.recommendation.contains("Flux.zip()")
                || fix.recommendation.contains("Flux.merge()")
        );
    }

    #[test]
    fn lookup_table_returns_quarkus_non_reactive_sql_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderRepository.java",
                Some("io.quarkus.hibernate.orm.runtime.session.SessionImpl"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_quarkus");
        assert!(
            fix.recommendation.contains("JOIN FETCH")
                || fix.recommendation.contains("@EntityGraph"),
            "Quarkus non-reactive SQL fix should mention JOIN FETCH or @EntityGraph"
        );
    }

    #[test]
    fn lookup_table_returns_quarkus_non_reactive_redundant_fix() {
        let f = finding_with_location(
            FindingType::RedundantSql,
            Some(loc(
                "UserService.java",
                Some("io.quarkus.hibernate.orm.runtime.session.SessionImpl"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_quarkus");
        assert!(fix.recommendation.contains("@CacheResult"));
    }

    #[test]
    fn lookup_table_returns_helidon_se_sql_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "OrderService.java",
                Some("io.helidon.dbclient.jdbc.JdbcExecute"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_helidon_se");
        assert!(
            fix.recommendation.contains("DbClient"),
            "Helidon SE SQL fix should reference DbClient"
        );
    }

    #[test]
    fn lookup_table_returns_helidon_se_http_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "UserClient.java",
                Some("io.helidon.webclient.WebClient"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_helidon_se");
        assert!(
            fix.recommendation.contains("Single.zip") || fix.recommendation.contains("Multi.merge"),
            "Helidon SE HTTP fix should reference Single.zip or Multi.merge"
        );
    }

    #[test]
    fn lookup_table_returns_helidon_mp_sql_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc(
                "UserRepository.java",
                Some("io.helidon.microprofile.cdi.HelidonContainerImpl"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_helidon_mp");
        assert!(
            fix.recommendation.contains("@EntityGraph")
                || fix.recommendation.contains("JOIN FETCH"),
            "Helidon MP SQL fix should reference @EntityGraph or JOIN FETCH"
        );
    }

    #[test]
    fn lookup_table_returns_helidon_mp_http_fix() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(loc(
                "UserResource.java",
                Some("io.helidon.microprofile.server.ServerImpl"),
            )),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_helidon_mp");
        assert!(
            fix.recommendation.contains("MicroProfile Rest Client")
                && fix.recommendation.contains("CompletableFuture"),
            "Helidon MP HTTP fix should reference MicroProfile Rest Client + CompletableFuture"
        );
    }

    // ── v3 FIXES expansion (slow_*, redundant_http, fanout, chatty,
    //    pool_saturation, serialized_calls) ─────────────────────────

    #[test]
    fn lookup_table_returns_slow_sql_jpa_fix() {
        let f = finding_with_location(
            FindingType::SlowSql,
            Some(loc("Repository.java", Some("org.hibernate.SessionImpl"))),
        );
        let fix = lookup_fix(&f).expect("(SlowSql, JavaJpa) should have a fix");
        assert_eq!(fix.framework, "java_jpa");
        assert!(fix.recommendation.contains("EXPLAIN ANALYZE"));
        assert!(fix.recommendation.contains("index"));
    }

    #[test]
    fn lookup_table_returns_slow_sql_csharp_ef_core_fix() {
        let f = finding_with_location(
            FindingType::SlowSql,
            Some(loc(
                "OrderQueries.cs",
                Some("Microsoft.EntityFrameworkCore.DbContext"),
            )),
        );
        let fix = lookup_fix(&f).expect("(SlowSql, CsharpEfCore) should have a fix");
        assert_eq!(fix.framework, "csharp_ef_core");
        assert!(fix.recommendation.contains("ToQueryString"));
        assert!(fix.recommendation.contains("HasIndex"));
    }

    #[test]
    fn lookup_table_returns_redundant_http_java_generic_fix() {
        let f = finding_with_location(
            FindingType::RedundantHttp,
            Some(loc(
                "UserClient.java",
                Some("org.springframework.web.client"),
            )),
        );
        let fix = lookup_fix(&f).expect("(RedundantHttp, JavaGeneric) should have a fix");
        assert_eq!(fix.framework, "java_generic");
        assert!(
            fix.recommendation.contains("memoization") || fix.recommendation.contains("@Cacheable")
        );
    }

    #[test]
    fn lookup_table_returns_excessive_fanout_webflux_fix() {
        let f = finding_with_location(
            FindingType::ExcessiveFanout,
            Some(loc(
                "OrderRouter.java",
                Some("org.springframework.web.reactive"),
            )),
        );
        let fix = lookup_fix(&f).expect("(ExcessiveFanout, JavaWebFlux) should have a fix");
        assert_eq!(fix.framework, "java_webflux");
        assert!(fix.recommendation.contains("concurrency"));
        assert!(fix.recommendation.contains("bulkhead"));
    }

    #[test]
    fn lookup_table_returns_chatty_service_csharp_generic_fix() {
        let f = finding_with_location(
            FindingType::ChattyService,
            Some(loc("OrderOrchestrator.cs", Some("MyApp.Orders.Sync"))),
        );
        let fix = lookup_fix(&f).expect("(ChattyService, CsharpGeneric) should have a fix");
        assert_eq!(fix.framework, "csharp_generic");
        assert!(
            fix.recommendation.contains("bulk endpoint") || fix.recommendation.contains("gRPC")
        );
    }

    #[test]
    fn lookup_table_returns_pool_saturation_quarkus_fix() {
        let f = finding_with_location(
            FindingType::PoolSaturation,
            Some(loc(
                "OrderService.java",
                Some("io.quarkus.hibernate.orm.runtime"),
            )),
        );
        let fix = lookup_fix(&f).expect("(PoolSaturation, JavaQuarkus) should have a fix");
        assert_eq!(fix.framework, "java_quarkus");
        assert!(fix.recommendation.contains("jdbc.max-size"));
        assert!(fix.recommendation.contains("slow"));
    }

    #[test]
    fn lookup_table_returns_serialized_calls_rust_generic_fix() {
        let f = finding_with_location(
            FindingType::SerializedCalls,
            Some(loc("src/checkout.rs", Some("myapp::checkout"))),
        );
        let fix = lookup_fix(&f).expect("(SerializedCalls, RustGeneric) should have a fix");
        assert_eq!(fix.framework, "rust_generic");
        assert!(fix.recommendation.contains("tokio::join"));
    }

    #[test]
    fn fix_table_cardinality_is_pinned() {
        // Snapshot of the (FindingType, Framework) table size. Bumping
        // this number is fine when an entry is intentionally added or
        // removed; reading the diff makes the change explicit instead
        // of silently growing the public `suggested_fix` surface.
        assert_eq!(FIXES.len(), 70);
        // Anchor a handful of load-bearing combinations so a swap that
        // preserves the count (drop one entry, add another) still trips
        // the test instead of sliding through silently.
        for anchor in [
            (FindingType::NPlusOneSql, Framework::JavaJpa),
            (FindingType::NPlusOneSql, Framework::CsharpEfCore),
            (FindingType::NPlusOneSql, Framework::RustDiesel),
            (FindingType::SlowSql, Framework::JavaJpa),
            (FindingType::SerializedCalls, Framework::RustGeneric),
            (FindingType::PoolSaturation, Framework::JavaQuarkus),
        ] {
            assert!(
                FIXES.contains_key(&anchor),
                "FIXES anchor entry {anchor:?} is missing; was it dropped or renamed?"
            );
        }
    }

    #[test]
    fn lookup_table_misses_for_pool_saturation_under_webflux() {
        // (PoolSaturation, JavaWebFlux) is intentionally never mapped:
        // WebFlux is a reactor runtime, pool saturation is a server-side
        // HikariCP / Npgsql / sqlx pattern. The recommendation would not
        // differ from JavaGeneric, so the combination is deliberately
        // absent. Verifies the full `lookup_fix → detect_framework →
        // FIXES.get` chain returns `None` end-to-end (not just that the
        // table itself lacks the entry).
        let f = finding_with_location(
            FindingType::PoolSaturation,
            Some(loc(
                "OrderRouter.java",
                Some("org.springframework.web.reactive.function.client"),
            )),
        );
        assert!(lookup_fix(&f).is_none());
    }

    #[test]
    fn lookup_table_misses_for_unmapped_rust_generic_n_plus_one_sql() {
        // Rust generic (no ORM) intentionally has no fix for SQL N+1: we
        // cannot give a sensible cross-cutting recommendation without a
        // specific ORM, and most Rust HTTP handlers go through one of
        // Diesel or SeaORM anyway.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("src/repo.rs", Some("orders::repo"))),
        );
        assert!(lookup_fix(&f).is_none());
    }

    // ── End-to-end enrich behavior ───────────────────────────────

    #[test]
    fn enrich_populates_suggested_fix_when_match() {
        let mut findings = vec![finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("Repository.java", Some("org.hibernate.SessionImpl"))),
        )];
        enrich(&mut findings);
        let fix = findings[0]
            .suggested_fix
            .as_ref()
            .expect("expected suggested_fix to be set");
        assert_eq!(fix.framework, "java_jpa");
    }

    #[test]
    fn enrich_leaves_suggested_fix_none_when_no_match() {
        // Rust file without an ORM hint and N+1 SQL: lookup misses
        // because we don't ship a (NPlusOneSql, RustGeneric) fix.
        let mut findings = vec![finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("src/repo.rs", None)),
        )];
        enrich(&mut findings);
        assert!(findings[0].suggested_fix.is_none());
    }

    #[test]
    fn enrich_populates_suggested_fix_for_python_django() {
        let mut findings = vec![finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("repo.py", Some("django.db.models"))),
        )];
        enrich(&mut findings);
        let fix = findings[0]
            .suggested_fix
            .as_ref()
            .expect("Python Django should be enriched with suggested_fix");
        assert_eq!(fix.framework, "python_django");
        assert!(fix.recommendation.contains("select_related"));
    }

    #[test]
    fn enrich_leaves_suggested_fix_none_for_unsupported_language() {
        let mut findings = vec![finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("query.rb", Some("ActiveRecord::Base"))),
        )];
        enrich(&mut findings);
        assert!(findings[0].suggested_fix.is_none());
    }

    #[test]
    fn suggested_fix_serializes_with_skip_when_url_absent() {
        let fix = SuggestedFix {
            pattern: "n_plus_one_sql".to_string(),
            framework: "java_jpa".to_string(),
            recommendation: "Use JOIN FETCH".to_string(),
            reference_url: None,
        };
        let json = serde_json::to_string(&fix).unwrap();
        assert!(!json.contains("reference_url"));
    }

    /// Defense-in-depth: every `reference_url` in the static `FIXES`
    /// table must be HTTPS and point at a recognised vendor docs
    /// domain. These URLs flow into CLI text, JSON and SARIF outputs
    /// where a hostile or accidentally-malformed URL (e.g. `javascript:`,
    /// mixed-content `http://`, a typo'd domain) would be displayed to
    /// developers. CI catches the regression at PR time.
    #[test]
    fn fix_table_reference_urls_are_https_and_on_allowed_domains() {
        const ALLOWED_DOMAIN_SUFFIXES: &[&str] = &[
            // Java
            "docs.jboss.org",
            "quarkus.io",
            "smallrye.io",
            "helidon.io",
            "docs.spring.io",
            "download.eclipse.org",
            "docs.oracle.com",
            "projectreactor.io",
            "resilience4j.readme.io",
            // C# / .NET
            "learn.microsoft.com",
            "www.pollydocs.org",
            "www.npgsql.org",
            // Python
            "docs.djangoproject.com",
            "docs.sqlalchemy.org",
            "docs.python.org",
            // Rust
            "docs.diesel.rs",
            "sea-ql.org",
            "docs.rs",
            // Cross-language references (vendor-neutral)
            "www.postgresql.org",
            "martinfowler.com",
        ];
        // `github.com` is broad — pin to explicit `<org>/<repo>` paths so a
        // future PR pointing at a typo-squat repo trips the guard.
        const ALLOWED_GITHUB_PREFIXES: &[&str] = &[
            "github.com/ben-manes/caffeine",
            "github.com/brettwooldridge/HikariCP",
        ];
        for ((ft, fw), fix) in FIXES.iter() {
            let Some(url) = fix.reference_url.as_deref() else {
                continue;
            };
            assert!(
                url.starts_with("https://"),
                "({ft:?}, {fw:?}) reference_url must start with https://, got {url:?}"
            );
            let after_scheme = &url["https://".len()..];
            // Reject userinfo (`user@host`) before any path separator: a URL
            // like `https://attacker@github.com/...` would otherwise pass the
            // host check below and end up rendered in operator dashboards.
            let authority_end = after_scheme
                .find(['/', '?', '#'])
                .unwrap_or(after_scheme.len());
            assert!(
                !after_scheme[..authority_end].contains('@'),
                "({ft:?}, {fw:?}) reference_url must not carry userinfo, got {url:?}"
            );
            // Strip an optional `:<port>` before the suffix match so a
            // legit `https://docs.spring.io:8443/...` is not rejected
            // because `host` carries the port.
            let authority = &after_scheme[..authority_end];
            let host = authority.split(':').next().unwrap_or(authority);
            let host_ok = ALLOWED_DOMAIN_SUFFIXES
                .iter()
                .any(|dom| host == *dom || host.ends_with(&format!(".{dom}")));
            // Pin github.com URLs to `<org>/<repo>` path prefixes, but only
            // when the byte after the prefix is a path boundary
            // (`/`, `?`, `#`) or end-of-string. Otherwise
            // `github.com/ben-manes/caffeine` would falsely match a
            // typo-squat like `github.com/ben-manes/caffeine-evil/wiki`.
            let github_path_ok = host == "github.com"
                && ALLOWED_GITHUB_PREFIXES.iter().any(|prefix| {
                    after_scheme
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
                });
            assert!(
                host_ok || github_path_ok,
                "({ft:?}, {fw:?}) reference_url {url:?} not allowed; add the host \
                 to ALLOWED_DOMAIN_SUFFIXES or the `<org>/<repo>` path to \
                 ALLOWED_GITHUB_PREFIXES"
            );
        }
    }
}
