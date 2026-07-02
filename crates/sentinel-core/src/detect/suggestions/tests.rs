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

// ── Vendor-specific scope detection ────────────────────────

#[test]
fn vendor_scope_dotnet_ef_core() {
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["OpenTelemetry.Instrumentation.EntityFrameworkCore"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::CsharpEfCore));
}

#[test]
fn vendor_scope_dotnet_ef_core_wins_over_namespace() {
    let mut f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["OpenTelemetry.Instrumentation.EntityFrameworkCore"],
    );
    f.code_location = Some(loc("OrderController.cs", Some("MyApp.Controllers")));
    assert_eq!(detect_framework(&f), Some(Framework::CsharpEfCore));
}

#[test]
fn vendor_scope_quarkus_generic() {
    let f = finding_with_scopes(FindingType::NPlusOneSql, &["io.quarkus.opentelemetry"]);
    assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkus));
}

#[test]
fn vendor_scope_quarkus_reactive_wins_over_generic() {
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["io.quarkus.hibernate.reactive", "io.quarkus.opentelemetry"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
}

#[test]
fn vendor_scope_quarkus_panache_reactive() {
    let f = finding_with_scopes(FindingType::NPlusOneSql, &["io.quarkus.panache.reactive"]);
    assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkusReactive));
}

#[test]
fn vendor_scope_wins_over_standard_scope() {
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["io.quarkus.opentelemetry", "io.opentelemetry.hibernate-6.0"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkus));
}

#[test]
fn vendor_scope_unknown_dotnet_instrumentation_falls_to_csharp_generic() {
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["OpenTelemetry.Instrumentation.SqlClient"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::CsharpGeneric));
}

#[test]
fn vendor_scope_quarkus_prefix_requires_dot_boundary() {
    let f = finding_with_scopes(FindingType::NPlusOneSql, &["io.quarkusbridge.acme"]);
    assert_eq!(detect_framework(&f), None);
}

#[test]
fn vendor_scope_reactive_prefix_does_not_match_across_underscore() {
    // io.quarkus.reactive_streams is under io.quarkus.* so the
    // catch-all JavaQuarkus matches, but the reactive-specific rule
    // for "io.quarkus.reactive" must NOT fire (next char is '_').
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["io.quarkus.reactive_streams.vendor"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::JavaQuarkus));
}

// ── Service-name fallback ──────────────────────────────────

#[test]
fn service_name_fallback_detects_helidon_se() {
    let mut f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    f.service = "helidon-se-svc".to_string();
    f.code_location = None;
    f.instrumentation_scopes = vec![];
    f.suggested_fix = None;
    assert_eq!(detect_framework(&f), Some(Framework::JavaHelidonSe));
}

#[test]
fn service_name_fallback_does_not_match_generic_names() {
    let mut f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    f.service = "diesel-svc".to_string();
    f.code_location = None;
    f.instrumentation_scopes = vec![];
    f.suggested_fix = None;
    assert_eq!(detect_framework(&f), None);
}

#[test]
fn service_name_fallback_returns_none_for_unknown() {
    let mut f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    f.service = "myapp-svc".to_string();
    f.code_location = None;
    f.instrumentation_scopes = vec![];
    f.suggested_fix = None;
    assert_eq!(detect_framework(&f), None);
}

#[test]
fn service_name_fallback_not_reached_when_scope_matches() {
    let mut f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["io.opentelemetry.spring-data-3.0"],
    );
    f.service = "diesel-svc".to_string();
    assert_eq!(detect_framework(&f), Some(Framework::JavaJpa));
}

#[test]
fn service_name_fallback_helidon_mp_wins_over_se() {
    let mut f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    f.service = "helidon-mp-svc".to_string();
    f.code_location = None;
    f.instrumentation_scopes = vec![];
    f.suggested_fix = None;
    assert_eq!(detect_framework(&f), Some(Framework::JavaHelidonMp));
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
fn detects_php_laravel_eloquent_from_php_filepath_and_namespace() {
    // `.php` maps to Language::Php and `App\Models\User` matches the
    // Eloquent `App\Models` namespace hint via the `\` boundary.
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("query.php", Some("App\\Models\\User"))),
    );
    assert_eq!(detect_framework(&f), Some(Framework::PhpLaravelEloquent));
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
fn returns_none_for_unsupported_extension() {
    // A language outside the taxonomy (Kotlin) with a namespace that
    // matches no rule must not be enriched. Guards the invariant
    // "unknown extension -> no false enrichment" now that `.php` is
    // supported and no longer serves as the unsupported example.
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("Order.kt", Some("com.example.OrderService"))),
    );
    assert_eq!(detect_framework(&f), None);
}

#[test]
fn enrich_leaves_suggested_fix_none_for_unsupported_language() {
    let mut findings = vec![finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("Order.kt", Some("com.example.OrderService"))),
    )];
    enrich(&mut findings);
    assert!(findings[0].suggested_fix.is_none());
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
        fix.recommendation.contains("@Cacheable") || fix.recommendation.contains("EntityManager"),
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
        fix.recommendation.contains("Flux.zip()") || fix.recommendation.contains("Flux.merge()")
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
        fix.recommendation.contains("JOIN FETCH") || fix.recommendation.contains("@EntityGraph"),
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
        fix.recommendation.contains("@EntityGraph") || fix.recommendation.contains("JOIN FETCH"),
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
    assert!(fix.recommendation.contains("bulk endpoint") || fix.recommendation.contains("gRPC"));
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

// ── Language-from-scope-prefix ─────────────────────────────

#[test]
fn go_generic_from_scope_prefix_without_code_location() {
    // go-svc spans have scope `github.com/exaring/otelpgx` but no
    // code.filepath or code.namespace. The scope prefix `github.com/`
    // reveals Go, and the generic fallback fires.
    let mut f = finding_with_location(FindingType::NPlusOneSql, None);
    f.instrumentation_scopes = vec!["github.com/exaring/otelpgx".to_string()];
    let fix = lookup_fix(&f).expect("GoGeneric via scope prefix");
    assert_eq!(fix.framework, "go_generic");
}

#[test]
fn node_generic_from_scope_prefix_without_code_location() {
    // nest-svc spans have scope `@opentelemetry/instrumentation-pg`
    // but no code.filepath.
    let mut f = finding_with_location(FindingType::NPlusOneSql, None);
    f.instrumentation_scopes = vec!["@opentelemetry/instrumentation-pg".to_string()];
    let fix = lookup_fix(&f).expect("NodeGeneric via scope prefix");
    assert_eq!(fix.framework, "node_generic");
}

#[test]
fn python_bare_driver_scope_returns_none_without_filepath() {
    // Python `opentelemetry.instrumentation.psycopg` uses the OTel
    // convention prefix, handled by SCOPE_RULES. psycopg is not a
    // listed needle, so SCOPE_RULES miss. The conservative
    // language_from_scope_prefix does NOT catch OTel-convention
    // prefixes. Without filepath, detection returns None.
    let mut f = finding_with_location(FindingType::RedundantSql, None);
    f.instrumentation_scopes = vec!["opentelemetry.instrumentation.psycopg".to_string()];
    assert!(lookup_fix(&f).is_none());
}

// ── Go detection ──────────────────────────────────────────

#[test]
fn go_gorm_detected_from_filepath_and_namespace() {
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("handler.go", Some("gorm.io/gorm"))),
    );
    let fix = lookup_fix(&f).expect("(NPlusOneSql, GoGorm) should have a fix");
    assert_eq!(fix.framework, "go_gorm");
    assert!(fix.recommendation.contains("Preload"));
}

#[test]
fn go_generic_fallback_from_filepath_without_gorm_namespace() {
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("handler.go", Some("main"))),
    );
    let fix = lookup_fix(&f).expect("(NPlusOneSql, GoGeneric) should have a fix");
    assert_eq!(fix.framework, "go_generic");
}

#[test]
fn go_gorm_detected_from_namespace_without_filepath() {
    let f = finding_with_location(
        FindingType::RedundantSql,
        Some(CodeLocation {
            function: Some("List".to_string()),
            filepath: None,
            lineno: None,
            namespace: Some("gorm.io/gorm".to_string()),
        }),
    );
    let fix = lookup_fix(&f).expect("GORM should be detected from namespace alone");
    assert_eq!(fix.framework, "go_gorm");
}

// ── Node.js / TypeScript detection ───────────────────────

#[test]
fn node_prisma_detected_from_ts_filepath_and_namespace() {
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("service.ts", Some("prisma"))),
    );
    let fix = lookup_fix(&f).expect("(NPlusOneSql, NodePrisma) should have a fix");
    assert_eq!(fix.framework, "node_prisma");
    assert!(fix.recommendation.contains("include"));
}

#[test]
fn node_generic_from_tsx_filepath() {
    let f = finding_with_location(
        FindingType::SlowHttp,
        Some(loc("OrderList.tsx", Some("components.OrderList"))),
    );
    let fix = lookup_fix(&f).expect("(SlowHttp, NodeGeneric) should have a fix");
    assert_eq!(fix.framework, "node_generic");
}

#[test]
fn node_generic_from_mjs_filepath() {
    let f = finding_with_location(
        FindingType::NPlusOneHttp,
        Some(loc("handler.mjs", Some("routes.orders"))),
    );
    let fix = lookup_fix(&f).expect("(NPlusOneHttp, NodeGeneric) should have a fix");
    assert_eq!(fix.framework, "node_generic");
}

#[test]
fn fix_table_cardinality_is_pinned() {
    // Snapshot of the (FindingType, Framework) table size. Bumping
    // this number is fine when an entry is intentionally added or
    // removed; reading the diff makes the change explicit instead
    // of silently growing the public `suggested_fix` surface.
    assert_eq!(FIXES.len(), 132);
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
        (FindingType::NPlusOneSql, Framework::GoGorm),
        (FindingType::NPlusOneSql, Framework::NodePrisma),
        (FindingType::NPlusOneSql, Framework::RubyActiveRecord),
        (FindingType::NPlusOneSql, Framework::PhpLaravelEloquent),
        (FindingType::NPlusOneHttp, Framework::PhpLaravelEloquent),
        (FindingType::NPlusOneSql, Framework::PhpDoctrine),
        (FindingType::NPlusOneSql, Framework::PhpGeneric),
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
fn enrich_populates_suggested_fix_for_php_laravel_eloquent() {
    // `.php` maps to Language::Php and `App\Models\User` matches the
    // Eloquent namespace hint, so the finding is enriched.
    let mut findings = vec![finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("app/Models/User.php", Some("App\\Models\\User"))),
    )];
    enrich(&mut findings);
    let fix = findings[0]
        .suggested_fix
        .as_ref()
        .expect("PHP Laravel Eloquent should be enriched with suggested_fix");
    assert_eq!(fix.framework, "php_laravel_eloquent");
    assert!(fix.recommendation.contains("with("));
}

#[test]
fn detects_ruby_active_record_via_scope() {
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["OpenTelemetry::Instrumentation::ActiveRecord"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::RubyActiveRecord));
}

#[test]
fn detects_ruby_generic_via_scope_prefix() {
    // A non-ActiveRecord Ruby OTel scope routes to the Ruby generic.
    let f = finding_with_scopes(
        FindingType::SlowSql,
        &["OpenTelemetry::Instrumentation::PG"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::RubyGeneric));
}

#[test]
fn detects_ruby_generic_via_rb_filepath() {
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("app/models/order.rb", None)),
    );
    assert_eq!(detect_framework(&f), Some(Framework::RubyGeneric));
}

#[test]
fn enrich_populates_suggested_fix_for_ruby_active_record() {
    let mut findings = vec![finding_with_scopes(
        FindingType::NPlusOneSql,
        &["OpenTelemetry::Instrumentation::ActiveRecord"],
    )];
    enrich(&mut findings);
    let fix = findings[0]
        .suggested_fix
        .as_ref()
        .expect("Ruby ActiveRecord should be enriched with suggested_fix");
    assert_eq!(fix.framework, "ruby_active_record");
    assert!(fix.recommendation.contains("includes"));
}

#[test]
fn detects_php_doctrine_via_scope() {
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &["io.opentelemetry.contrib.php.doctrine"],
    );
    assert_eq!(detect_framework(&f), Some(Framework::PhpDoctrine));
}

#[test]
fn detects_php_laravel_eloquent_via_scope() {
    // The Laravel SQL leaf span is PDO-scoped, but the app-wide laravel
    // scope rides the finding's parent chain.
    let f = finding_with_scopes(
        FindingType::NPlusOneSql,
        &[
            "io.opentelemetry.contrib.php.pdo",
            "io.opentelemetry.contrib.php.laravel",
        ],
    );
    assert_eq!(detect_framework(&f), Some(Framework::PhpLaravelEloquent));
}

#[test]
fn detects_php_generic_via_pdo_scope_prefix() {
    // A PHP OTel scope with no Laravel/Doctrine vendor match routes to
    // the PHP generic.
    let f = finding_with_scopes(FindingType::SlowSql, &["io.opentelemetry.contrib.php.pdo"]);
    assert_eq!(detect_framework(&f), Some(Framework::PhpGeneric));
}

#[test]
fn detects_php_generic_via_php_filepath() {
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("src/Repository/OrderRepository.php", None)),
    );
    assert_eq!(detect_framework(&f), Some(Framework::PhpGeneric));
}

#[test]
fn disambiguates_php_doctrine_from_eloquent_via_namespace() {
    let doctrine = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("src/Order.php", Some("Doctrine\\ORM\\EntityManager"))),
    );
    assert_eq!(detect_framework(&doctrine), Some(Framework::PhpDoctrine));
    let eloquent = finding_with_location(
        FindingType::NPlusOneSql,
        Some(loc("app/Models/User.php", Some("App\\Models\\User"))),
    );
    assert_eq!(
        detect_framework(&eloquent),
        Some(Framework::PhpLaravelEloquent)
    );
}

#[test]
fn php_namespace_alone_cross_language_fallthrough() {
    // No scope, no filepath: the cross-language namespace scan still
    // recognises the Doctrine package via the `\` boundary.
    let f = finding_with_location(
        FindingType::NPlusOneSql,
        Some(CodeLocation {
            function: Some("query".to_string()),
            filepath: None,
            lineno: None,
            namespace: Some("Doctrine\\DBAL\\Driver".to_string()),
        }),
    );
    assert_eq!(detect_framework(&f), Some(Framework::PhpDoctrine));
}

#[test]
fn php_backslash_namespace_does_not_leak_to_other_languages() {
    // Regression: a `\`-separated PHP namespace must never be claimed by a
    // dot/colon language in the namespace-alone scan. Java's
    // `LastSegmentEndsWith("Repository")` would otherwise tag the Symfony
    // convention `App\Repository\OrderRepository` as java_jpa, and Node's
    // bare `prisma` hint would tag `App\prisma\Foo` as node_prisma.
    for ns in ["App\\Repository\\OrderRepository", "App\\prisma\\Foo"] {
        let f = finding_with_location(
            FindingType::NPlusOneSql,
            Some(CodeLocation {
                function: None,
                filepath: None,
                lineno: None,
                namespace: Some(ns.to_string()),
            }),
        );
        // No PHP hint matches these, and no non-PHP language may claim a
        // `\` namespace, so detection yields nothing.
        assert_eq!(detect_framework(&f), None, "ns = {ns}");
    }
}

#[test]
fn namespace_matcher_backslash_boundaries() {
    // PHP `\` segments match like `.`/`::` segments.
    assert!(namespace_contains_segment(
        "Doctrine\\ORM\\EntityManager",
        "Doctrine\\ORM"
    ));
    assert!(namespace_contains_segment(
        "App\\Models\\User",
        "App\\Models"
    ));
    // Leading boundary: a longer leading segment must not match.
    assert!(!namespace_contains_segment(
        "MyDoctrine\\ORM",
        "Doctrine\\ORM"
    ));
    // Trailing boundary: a longer trailing segment must not match.
    assert!(!namespace_contains_segment(
        "Doctrine\\ORMExtra",
        "Doctrine\\ORM"
    ));
}

#[test]
fn namespace_matcher_other_separators_unchanged() {
    // Adding the `\` arm must not change `.`/`::` matching.
    assert!(namespace_contains_segment(
        "org.hibernate.SessionImpl",
        "org.hibernate"
    ));
    assert!(!namespace_contains_segment(
        "io.helidongrpc.Foo",
        "io.helidon"
    ));
    assert!(namespace_contains_segment(
        "orders::diesel::query",
        "diesel::"
    ));
    assert!(!namespace_contains_segment(
        "orders::mydiesel::query",
        "diesel::"
    ));
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
        // Go
        "pkg.go.dev",
        "gorm.io",
        // Node.js
        "www.npmjs.com",
        "node-postgres.com",
        "www.prisma.io",
        "developer.mozilla.org",
        // Ruby
        "guides.rubyonrails.org",
        "lostisland.github.io",
        // PHP
        "laravel.com",
        "symfony.com",
        "www.doctrine-project.org",
        "www.php.net",
        // Cross-language references (vendor-neutral)
        "www.postgresql.org",
        "martinfowler.com",
    ];
    // `github.com` is broad — pin to explicit `<org>/<repo>` paths so a
    // future PR pointing at a typo-squat repo trips the guard.
    const ALLOWED_GITHUB_PREFIXES: &[&str] = &[
        "github.com/ben-manes/caffeine",
        "github.com/brettwooldridge/HikariCP",
        "github.com/jackc/pgx",
        "github.com/patrickmn/go-cache",
        "github.com/sony/gobreaker",
        "github.com/ruby-concurrency/concurrent-ruby",
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
