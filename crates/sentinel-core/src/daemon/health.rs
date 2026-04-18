//! Liveness healthcheck endpoint for the daemon.
//!
//! `GET /health` returns `200 OK` with a minimal JSON body. It has no
//! dependencies on daemon state (no window lock, no findings store),
//! so it cannot false-negative under load and is safe to wire as a
//! Kubernetes liveness or load-balancer health probe.
//!
//! Readiness-style signals (uptime, active traces, scraper staleness)
//! already live at `/api/status`; this endpoint deliberately stays O(1)
//! and dependency-free.

use axum::Json;
use axum::Router;
use axum::routing::get;
use serde::Serialize;

/// JSON body returned by `GET /health`.
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

/// Build an axum router with a `GET /health` endpoint. Stateless.
pub fn health_route() -> Router {
    async fn handle_health() -> Json<HealthResponse> {
        Json(HealthResponse {
            status: "ok",
            version: env!("CARGO_PKG_VERSION"),
        })
    }

    Router::new().route("/health", get(handle_health))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_200_with_status_ok_and_version() {
        let app = health_route();
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("ok"));
        assert_eq!(
            json.get("version").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }
}
