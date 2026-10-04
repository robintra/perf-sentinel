//! Static fix tables keyed by framework and by messaging broker.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::{Framework, MessagingSystem, SuggestedFix};
use crate::detect::FindingType;

/// Static mapping of `(finding_type, framework)` to a fix template.
///
/// A lookup missing from the table retries with the framework's
/// language generic. When that misses too, the finding's
/// `suggested_fix` field stays `None`. This is the extension point for
/// future framework support: add entries here, no other wiring required.
pub(super) static FIXES: LazyLock<HashMap<(FindingType, Framework), SuggestedFix>> = LazyLock::new(
    || {
        use FindingType::{
            ChattyService, ExcessiveFanout, NPlusOneHttp, NPlusOneSql, PoolSaturation,
            RedundantHttp, RedundantSql, SerializedCalls, SlowHttp, SlowSql,
        };
        use Framework::{
            CsharpEfCore, CsharpGeneric, GoGeneric, GoGorm, JavaGeneric, JavaHelidonMp,
            JavaHelidonSe, JavaJpa, JavaQuarkus, JavaQuarkusReactive, JavaWebFlux, NodeGeneric,
            NodePrisma, PhpDoctrine, PhpGeneric, PhpLaravelEloquent, PythonDjango, PythonGeneric,
            PythonSqlAlchemy, RubyActiveRecord, RubyGeneric, RustDiesel, RustGeneric, RustSeaOrm,
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
                Some(
                    "https://docs.spring.io/spring-framework/reference/web/webflux-functional.html",
                ),
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
                (NPlusOneSql, JavaGeneric),
                "Rewrite the per-id loop as a single query with a `JOIN` or `WHERE id \
             IN (...)`. With `JdbcTemplate`, bind the id list through \
             `NamedParameterJdbcTemplate`, or on PostgreSQL pass one array via \
             `= ANY(?)`.",
                Some(
                    "https://docs.spring.io/spring-framework/reference/data-access/jdbc/\
                 parameter-handling.html#jdbc-in-clause",
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
                Some(
                    "https://docs.sqlalchemy.org/en/21/core/metadata.html#sqlalchemy.schema.Index",
                ),
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
                Some(
                    "https://martinfowler.com/articles/microservices.html#SmartEndpointsAndDumbPipes",
                ),
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
        build_fix_table(entries, Framework::as_str, "FIXES")
    },
);

/// Broker-technology fixes for the messaging finding types. Same
/// extension point contract as [`FIXES`]: add entries here, nothing
/// else to wire.
pub(super) static MESSAGING_FIXES: LazyLock<HashMap<(FindingType, MessagingSystem), SuggestedFix>> =
    LazyLock::new(|| {
        use FindingType::{NPlusOneMessaging, SlowMessaging};
        use MessagingSystem::{AwsSqs, Jms, Kafka, Nats, Pulsar, RabbitMq};
        const KAFKA_PRODUCER_CONFIGS: &str = "https://docs.confluent.io/platform/current/installation/configuration/producer-configs.html";
        const RABBITMQ_CONFIRMS: &str = "https://www.rabbitmq.com/docs/confirms";
        const SQS_BATCH: &str = "https://docs.aws.amazon.com/AWSSimpleQueueService/latest/\
             SQSDeveloperGuide/sqs-batch-api-actions.html";
        const PULSAR_BATCHING: &str = "https://pulsar.apache.org/docs/concepts-messaging/";
        const JMS_LOCAL_TX: &str = "https://docs.oracle.com/javaee/7/tutorial/jms-concepts004.htm";
        let entries: &[((FindingType, MessagingSystem), &str, Option<&str>)] = &[
            (
                (NPlusOneMessaging, Kafka),
                "Let the producer batch: publish the records without calling `flush()` or \
                 blocking on each `send()` future, and tune `linger.ms` / `batch.size` so \
                 records to the same partition coalesce into one broker request.",
                Some(KAFKA_PRODUCER_CONFIGS),
            ),
            (
                (SlowMessaging, Kafka),
                "A slow publish usually blocks on acknowledgement or metadata: avoid \
                 `send().get()` per record, let `linger.ms` batching amortize the \
                 round-trip, and weigh `acks=all` durability against latency explicitly.",
                Some(KAFKA_PRODUCER_CONFIGS),
            ),
            (
                (NPlusOneMessaging, RabbitMq),
                "Publish the batch on one channel and wait for outstanding confirms once \
                 after the loop instead of per message, or aggregate the items into one \
                 message when the consumer processes them together.",
                Some(RABBITMQ_CONFIRMS),
            ),
            (
                (SlowMessaging, RabbitMq),
                "Waiting for a publisher confirm per message costs one broker round-trip \
                 each: process confirms asynchronously or per batch. A broker under a \
                 memory or disk alarm also throttles publishers cluster-wide.",
                Some(RABBITMQ_CONFIRMS),
            ),
            (
                (NPlusOneMessaging, AwsSqs),
                "Replace the per-item `SendMessage` calls with `SendMessageBatch`, which \
                 carries up to 10 messages per request.",
                Some(SQS_BATCH),
            ),
            (
                (SlowMessaging, AwsSqs),
                "Each `SendMessage` is an HTTPS round-trip: reuse one SDK client so \
                 connections pool, and amortize with `SendMessageBatch`. Throttling \
                 retries with backoff also surface as slow publishes.",
                Some(SQS_BATCH),
            ),
            (
                (NPlusOneMessaging, Pulsar),
                "Enable producer batching (`enableBatching`, `batchingMaxPublishDelay`, \
                 `batchingMaxMessages`) and publish with `sendAsync` so the client \
                 coalesces the messages into one broker request.",
                Some(PULSAR_BATCHING),
            ),
            (
                (SlowMessaging, Pulsar),
                "Publish with `sendAsync` and batching enabled rather than a blocking \
                 `send` per message, and watch the pending-messages queue: with \
                 `blockIfQueueFull` a full queue makes `sendAsync` block too.",
                Some(PULSAR_BATCHING),
            ),
            (
                (NPlusOneMessaging, Nats),
                "With JetStream, publish asynchronously and await the acks together after \
                 the loop instead of one synchronous publish per item. If the subscriber \
                 handles the items together, aggregate them into one message.",
                None,
            ),
            (
                (SlowMessaging, Nats),
                "A synchronous JetStream publish waits for the stream's acknowledgement, \
                 and a replicated stream acknowledges after quorum: publish asynchronously \
                 and await the acks in batch.",
                None,
            ),
            (
                (NPlusOneMessaging, Jms),
                "Send the batch inside a transacted session and commit once after the \
                 loop, so the provider ships the sends as one unit instead of one \
                 synchronous round-trip each.",
                Some(JMS_LOCAL_TX),
            ),
            (
                (SlowMessaging, Jms),
                "Persistent delivery makes the broker sync each message before \
                 acknowledging: batch sends in a transacted session, and use \
                 non-persistent delivery only where message loss is acceptable.",
                Some(JMS_LOCAL_TX),
            ),
        ];
        build_fix_table(entries, MessagingSystem::as_str, "MESSAGING_FIXES")
    });

/// Builds a fix table from its entries. `HashMap::insert` silently
/// overwrites, so the debug assertion catches a copy-pasted duplicate key.
fn build_fix_table<K: Copy + Eq + std::hash::Hash>(
    entries: &[((FindingType, K), &str, Option<&str>)],
    label: fn(K) -> &'static str,
    table: &str,
) -> HashMap<(FindingType, K), SuggestedFix> {
    let mut m = HashMap::with_capacity(entries.len());
    for ((ft, key), recommendation, url) in entries {
        m.insert(
            (ft.clone(), *key),
            SuggestedFix {
                pattern: ft.as_str().to_string(),
                framework: label(*key).to_string(),
                recommendation: (*recommendation).to_string(),
                reference_url: url.map(ToString::to_string),
            },
        );
    }
    debug_assert_eq!(entries.len(), m.len(), "duplicate key in {table} entries");
    m
}
