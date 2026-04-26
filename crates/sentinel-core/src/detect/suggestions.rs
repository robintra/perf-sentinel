//! Framework-aware actionable fixes for findings.
//!
//! Enriches detected findings with a [`SuggestedFix`] when the finding's
//! `code_location` lets us infer which framework produced the
//! anti-pattern. v1 covered Java/JPA. v2 (this module's current state)
//! adds:
//!
//! - **Java**: `WebFlux` (reactor), Quarkus reactive (Mutiny + Hibernate
//!   Reactive), Quarkus non-reactive (Hibernate ORM + Panache),
//!   Helidon SE (`DbClient` + `WebClient` + Single/Multi), Helidon MP
//!   (`MicroProfile` Rest Client + JPA-managed entities)
//! - **C# (.NET 8 to 10)**: EF Core (with Pomelo `MySQL` provider),
//!   `CsharpGeneric` fallback
//! - **Rust**: Diesel, `SeaORM`, `RustGeneric` fallback
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
/// Stable JSON shape from v0.4.2 onward. Field names will not be renamed
/// or removed in a minor release. New optional fields may be added.
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

const RUST_RULES: &[(Framework, &[Hint])] = &[
    (Framework::RustDiesel, &[Hint::Substring("diesel::")]),
    (Framework::RustSeaOrm, &[Hint::Substring("sea_orm::")]),
];

#[derive(Debug, Clone, Copy)]
enum Language {
    Java,
    Csharp,
    Rust,
}

impl Language {
    const fn rules(self) -> &'static [(Framework, &'static [Hint])] {
        match self {
            Self::Java => JAVA_RULES,
            Self::Csharp => CSHARP_RULES,
            Self::Rust => RUST_RULES,
        }
    }

    const fn generic(self) -> Framework {
        match self {
            Self::Java => Framework::JavaGeneric,
            Self::Csharp => Framework::CsharpGeneric,
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
    use FindingType::{NPlusOneHttp, NPlusOneSql, RedundantSql};
    use Framework::{
        CsharpEfCore, CsharpGeneric, JavaGeneric, JavaHelidonMp, JavaHelidonSe, JavaJpa,
        JavaQuarkus, JavaQuarkusReactive, JavaWebFlux, RustDiesel, RustGeneric, RustSeaOrm,
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

/// Pure framework detector. Operates only on the finding's
/// `code_location` field. Returns `None` when no information is
/// available. Otherwise returns the most specific framework matched
/// by the namespace, or the language's generic fallback when the
/// language is known via the filepath but no rule matches.
///
/// When `filepath` is missing we cannot identify the language with
/// certainty, so we try every language's rules in order (Java, C#,
/// Rust) and return the first hit. This recovers the common case
/// where an OpenTelemetry agent emits `code.namespace` on a parent span but
/// not `code.filepath`. No generic fallback fires in that path:
/// without a filepath we will not guess a language for an unknown
/// namespace.
fn detect_framework(finding: &Finding) -> Option<Framework> {
    let loc = finding.code_location.as_ref()?;
    let ns = loc.namespace.as_deref().unwrap_or("");
    if let Some(filepath) = loc.filepath.as_deref() {
        let language = language_from_filepath(filepath)?;
        for (framework, hints) in language.rules() {
            if hints.iter().any(|hint| hint_matches(ns, *hint)) {
                return Some(*framework);
            }
        }
        return Some(language.generic());
    }
    if ns.is_empty() {
        return None;
    }
    for language in [Language::Java, Language::Csharp, Language::Rust] {
        for (framework, hints) in language.rules() {
            if hints.iter().any(|hint| hint_matches(ns, *hint)) {
                return Some(*framework);
            }
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

    // ── Cross-language fallthrough ───────────────────────────────

    #[test]
    fn returns_none_for_unsupported_extension() {
        // Python file: not in our v2 scope.
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("repo.py", Some("django.db.models"))),
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

    #[test]
    fn lookup_table_misses_for_unmapped_combination() {
        // (SlowSql, JavaJpa) is intentionally not mapped.
        let f = finding_with_location(
            FindingType::SlowSql,
            Some(loc("Repository.java", Some("org.hibernate.SessionImpl"))),
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
    fn enrich_leaves_suggested_fix_none_for_unsupported_language() {
        let mut findings = vec![finding_with_location(
            FindingType::NPlusOneSql,
            Some(loc("repo.py", Some("django.db.models"))),
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
            // C# / .NET
            "learn.microsoft.com",
            // Rust
            "docs.diesel.rs",
            "sea-ql.org",
            "docs.rs",
        ];
        for ((ft, fw), fix) in FIXES.iter() {
            let Some(url) = fix.reference_url.as_deref() else {
                continue;
            };
            assert!(
                url.starts_with("https://"),
                "({ft:?}, {fw:?}) reference_url must start with https://, got {url:?}"
            );
            // Strip scheme and isolate the host.
            let after_scheme = &url["https://".len()..];
            let host = after_scheme
                .split(['/', '?', '#'])
                .next()
                .expect("split has at least one element");
            assert!(
                ALLOWED_DOMAIN_SUFFIXES
                    .iter()
                    .any(|dom| host == *dom || host.ends_with(&format!(".{dom}"))),
                "({ft:?}, {fw:?}) reference_url host {host:?} not in the allowlist; \
                 add it to ALLOWED_DOMAIN_SUFFIXES if intentional, otherwise fix the URL"
            );
        }
    }
}
