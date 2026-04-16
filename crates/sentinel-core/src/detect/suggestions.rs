//! Framework-aware actionable fixes for findings.
//!
//! Enriches detected findings with a [`SuggestedFix`] when the finding's
//! `code_location` lets us infer which framework produced the
//! anti-pattern. v1 covers Java/JPA only; other frameworks land via
//! community contributions.
//!
//! The detection is intentionally cheap and deterministic: we only look
//! at fields already present on [`crate::detect::Finding`] (no span-level
//! access, no extra heap allocations on the hot path), and missing
//! information always degrades gracefully to `suggested_fix = None`.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::{Finding, FindingType};

/// A framework-specific actionable fix attached to a [`Finding`].
///
/// Stable JSON shape from v0.4.2 onward. Field names will not be renamed
/// or removed in a minor release; new optional fields may be added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedFix {
    /// Mirrors the parent finding's `type` in `snake_case` (e.g.
    /// `n_plus_one_sql`). Lets downstream consumers route fixes without
    /// re-reading the parent.
    pub pattern: String,
    /// Framework tag this fix applies to (e.g. `java_jpa`,
    /// `java_generic`). Stable enum-like string.
    pub framework: String,
    /// Short, imperative remediation sentence.
    pub recommendation: String,
    /// Documentation URL backing the recommendation. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_url: Option<String>,
}

/// Internal framework tag, used as a lookup key for the static fixes
/// table. Kept private; the public surface is the `framework` string on
/// [`SuggestedFix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Framework {
    JavaJpa,
    JavaGeneric,
}

impl Framework {
    const fn as_str(self) -> &'static str {
        match self {
            Self::JavaJpa => "java_jpa",
            Self::JavaGeneric => "java_generic",
        }
    }
}

/// JPA-related package prefixes used as a heuristic to upgrade a Java
/// finding from `JavaGeneric` to `JavaJpa`. Order does not matter; any
/// match wins.
const JPA_NAMESPACE_HINTS: &[&str] = &[
    "jakarta.persistence",
    "javax.persistence",
    "org.hibernate",
    "org.springframework.data.jpa",
];

/// Static mapping of `(finding_type, framework)` to a fix template.
///
/// Lookups missing from the table return `None` and the finding's
/// `suggested_fix` field stays `None`. This is the extension point for
/// future framework support: add entries here, no other wiring required.
static FIXES: LazyLock<HashMap<(FindingType, Framework), SuggestedFix>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert(
        (FindingType::NPlusOneSql, Framework::JavaJpa),
        SuggestedFix {
            pattern: FindingType::NPlusOneSql.as_str().to_string(),
            framework: Framework::JavaJpa.as_str().to_string(),
            recommendation: "Use JOIN FETCH on the relationship or annotate the repository \
                method with @EntityGraph to load associations in a single query."
                .to_string(),
            reference_url: Some(
                "https://docs.jboss.org/hibernate/orm/current/userguide/html_single/\
                 Hibernate_User_Guide.html#fetching-strategies-dynamic-fetching"
                    .to_string(),
            ),
        },
    );
    m.insert(
        (FindingType::NPlusOneHttp, Framework::JavaGeneric),
        SuggestedFix {
            pattern: FindingType::NPlusOneHttp.as_str().to_string(),
            framework: Framework::JavaGeneric.as_str().to_string(),
            recommendation: "Coalesce the calls into a batch endpoint, or cache the \
                per-request results with Spring's @Cacheable using a request-scoped cache."
                .to_string(),
            reference_url: Some(
                "https://docs.spring.io/spring-framework/reference/integration/cache.html"
                    .to_string(),
            ),
        },
    );
    m.insert(
        (FindingType::RedundantSql, Framework::JavaGeneric),
        SuggestedFix {
            pattern: FindingType::RedundantSql.as_str().to_string(),
            framework: Framework::JavaGeneric.as_str().to_string(),
            recommendation: "Add a service-level cache (Caffeine, Spring Cache) or \
                deduplicate the query within the request scope."
                .to_string(),
            reference_url: Some(
                "https://docs.spring.io/spring-framework/reference/integration/cache.html"
                    .to_string(),
            ),
        },
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

/// Pure framework detector. Operates only on the finding's
/// `code_location` field. Returns `None` when no information is
/// available, `Some(JavaGeneric)` for any `.java` filepath without a JPA
/// hint, and `Some(JavaJpa)` when a JPA-flavored namespace is present.
fn detect_framework(finding: &Finding) -> Option<Framework> {
    let loc = finding.code_location.as_ref()?;
    let filepath = loc.filepath.as_ref()?;
    if !filepath.to_ascii_lowercase().ends_with(".java") {
        return None;
    }
    if let Some(ns) = loc.namespace.as_deref()
        && JPA_NAMESPACE_HINTS.iter().any(|hint| ns.contains(hint))
    {
        return Some(Framework::JavaJpa);
    }
    Some(Framework::JavaGeneric)
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

    #[test]
    fn detects_java_jpa_via_jakarta_persistence() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("findByOrderId".to_string()),
                filepath: Some("src/main/java/com/example/OrderRepository.java".to_string()),
                lineno: Some(42),
                namespace: Some("jakarta.persistence.EntityManager".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn detects_java_jpa_via_hibernate() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("findByOrderId".to_string()),
                filepath: Some("src/main/java/com/example/OrderRepository.java".to_string()),
                lineno: Some(42),
                namespace: Some("org.hibernate.SessionImpl".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn detects_java_jpa_via_spring_data_jpa() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("findAll".to_string()),
                filepath: Some("OrderRepository.java".to_string()),
                lineno: Some(10),
                namespace: Some(
                    "org.springframework.data.jpa.repository.JpaRepository".to_string(),
                ),
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
    }

    #[test]
    fn falls_back_to_java_generic_without_jpa_hint() {
        let f = finding_with_location(
            FindingType::NPlusOneHttp,
            Some(CodeLocation {
                function: Some("fetchUser".to_string()),
                filepath: Some("src/main/java/com/example/UserClient.java".to_string()),
                lineno: Some(99),
                namespace: Some("com.example.UserClient".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaGeneric));
    }

    #[test]
    fn case_insensitive_java_extension() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: None,
                filepath: Some("Repository.JAVA".to_string()),
                lineno: None,
                namespace: None,
            }),
        );
        assert_eq!(detect_framework(&f), Some(Framework::JavaGeneric));
    }

    #[test]
    fn returns_none_for_non_java_filepath() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("query_user".to_string()),
                filepath: Some("src/repo.rs".to_string()),
                lineno: Some(7),
                namespace: Some("crate::repo".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn returns_none_when_code_location_missing() {
        let f = finding_with_location(FindingType::NPlusOneSql, None);
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn returns_none_when_filepath_absent_even_if_namespace_present() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: Some("findById".to_string()),
                filepath: None,
                lineno: Some(7),
                namespace: Some("org.hibernate.SessionImpl".to_string()),
            }),
        );
        assert_eq!(detect_framework(&f), None);
    }

    #[test]
    fn lookup_table_returns_jpa_fix_for_n_plus_one_sql() {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: None,
                filepath: Some("Repository.java".to_string()),
                lineno: None,
                namespace: Some("org.hibernate.SessionImpl".to_string()),
            }),
        );
        let fix = lookup_fix(&f).expect("should have a fix");
        assert_eq!(fix.framework, "java_jpa");
        assert_eq!(fix.pattern, "n_plus_one_sql");
        assert!(fix.recommendation.contains("JOIN FETCH"));
        assert!(fix.reference_url.is_some());
    }

    #[test]
    fn lookup_table_misses_for_unmapped_combination() {
        // (SlowSql, JavaJpa) is intentionally not mapped in the v1 table.
        let f = finding_with_location(
            FindingType::SlowSql,
            Some(CodeLocation {
                function: None,
                filepath: Some("Repository.java".to_string()),
                lineno: None,
                namespace: Some("org.hibernate.SessionImpl".to_string()),
            }),
        );
        assert!(lookup_fix(&f).is_none());
    }

    #[test]
    fn enrich_populates_suggested_fix_when_match() {
        let mut findings = vec![finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: None,
                filepath: Some("Repository.java".to_string()),
                lineno: None,
                namespace: Some("org.hibernate.SessionImpl".to_string()),
            }),
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
        let mut findings = vec![finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: None,
                filepath: Some("repo.rs".to_string()),
                lineno: None,
                namespace: None,
            }),
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
}
