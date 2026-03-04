pub mod api;
pub mod embedded;
pub mod state;

use axum::http::HeaderValue;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;

use state::AppState;

/// Build the full Axum router with API routes and embedded frontend.
pub fn build_router(state: AppState, port: u16) -> Router {
    // Only allow requests from our own web UI origin (localhost).
    let allowed_origins = [
        format!("http://localhost:{}", port).parse::<HeaderValue>().unwrap(),
        format!("http://127.0.0.1:{}", port).parse::<HeaderValue>().unwrap(),
    ];
    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
        ])
        .allow_headers([axum::http::header::CONTENT_TYPE]);

    Router::new()
        .route("/api/status", get(api::get_status))
        .route("/api/profiles", get(api::list_profiles))
        .route(
            "/api/profiles/:name",
            get(api::get_profile)
                .post(api::create_profile)
                .put(api::update_profile)
                .delete(api::delete_profile),
        )
        .route("/api/connect", post(api::connect))
        .route("/api/disconnect", post(api::disconnect))
        .route("/api/reconnect", post(api::reconnect))
        .route("/api/vpn-servers/:profile", get(api::list_vpn_servers))
        .route("/api/logs", get(api::get_logs))
        .route("/api/version", get(api::get_version))
        .fallback(embedded::static_handler)
        .layer(cors)
        .with_state(state)
}

/// Start the web server on the given port.
pub async fn serve(state: AppState, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_router(state, port);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    log::info!("web UI listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Each test gets its own isolated directory to avoid parallel test interference.
    fn test_state(test_name: &str) -> AppState {
        let dir = std::env::temp_dir()
            .join("corplink-test")
            .join(test_name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        state::new_app_state(dir)
    }

    #[tokio::test]
    async fn test_get_status() {
        let app = build_router(test_state("get_status"), 4099);
        let resp = app
            .oneshot(Request::builder().uri("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "expected json, got: {}", ct);
    }

    #[tokio::test]
    async fn test_list_profiles() {
        let app = build_router(test_state("list_profiles"), 4099);
        let resp = app
            .oneshot(Request::builder().uri("/api/profiles").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "expected json, got: {}", ct);
    }

    #[tokio::test]
    async fn test_get_single_profile() {
        let state = test_state("get_single");
        // Create a profile file manually
        let dir = state.lock().await.profiles_dir.clone();
        std::fs::write(
            dir.join("myprofile.json"),
            r#"{"company_name":"testco","username":"testuser"}"#,
        )
        .unwrap();

        let app = build_router(state, 4099);
        let resp = app
            .oneshot(Request::builder().uri("/api/profiles/myprofile").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "expected json, got: {}", ct);
    }

    #[tokio::test]
    async fn test_create_profile() {
        let app = build_router(test_state("create"), 4099);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/new-profile")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"company_name":"testco","username":"testuser"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "expected json, got: {}", ct);
    }

    #[tokio::test]
    async fn test_update_profile() {
        let state = test_state("update");
        // Create first
        let app = build_router(state.clone(), 4099);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/upd-profile")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"company_name":"testco","username":"testuser"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Update
        let app = build_router(state, 4099);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/profiles/upd-profile")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"company_name":"testco","username":"newuser"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "expected json, got: {}", ct);
    }

    #[tokio::test]
    async fn test_delete_profile() {
        let state = test_state("delete");
        // Create first
        let app = build_router(state.clone(), 4099);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/profiles/del-profile")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"company_name":"testco","username":"testuser"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Delete
        let app = build_router(state, 4099);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/profiles/del-profile")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "expected json, got: {}", ct);
    }

    #[tokio::test]
    async fn test_nonexistent_path_returns_fallback() {
        let app = build_router(test_state("fallback"), 4099);
        let resp = app
            .oneshot(Request::builder().uri("/some/random/path").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(
            ct.contains("text/html") || ct.contains("text/plain"),
            "fallback should be html or plain text, got: {}",
            ct
        );
    }
}
