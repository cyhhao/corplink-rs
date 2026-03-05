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

/// Start the web server on a pre-bound listener.
///
/// The caller is responsible for binding the `TcpListener` so that PID file
/// and flock are written only after the port is successfully acquired.
///
/// On shutdown (SIGINT / SIGTERM), any running daemon child process is
/// killed so that VPN routes and DNS are not left behind.
pub async fn serve(
    state: AppState,
    port: u16,
    listener: tokio::net::TcpListener,
    state_for_shutdown: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_router(state, port);
    log::info!("web UI listening on http://127.0.0.1:{}", port);

    let shutdown = async move {
        // Wait for SIGINT (Ctrl+C) or SIGTERM.
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )
        .expect("failed to register SIGTERM handler");

        #[cfg(unix)]
        tokio::select! {
            _ = ctrl_c => { log::info!("received SIGINT, shutting down..."); }
            _ = sigterm.recv() => { log::info!("received SIGTERM, shutting down..."); }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await.ok();
            log::info!("received SIGINT, shutting down...");
        }

        // Kill the daemon child process if it is still running.
        kill_daemon(&state_for_shutdown).await;
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// Request daemon shutdown via sentinel file, then wait for it to exit.
///
/// Note: we cannot use `libc::kill()` because the parent runs as a normal
/// user while the daemon runs as root — the kernel returns EPERM.
/// Ctrl+C in a terminal works because the TTY driver bypasses UID checks
/// when sending SIGINT to the foreground process group.
async fn kill_daemon(state: &AppState) {
    let (has_daemon, tmp_dir) = {
        let inner = state.lock().await;
        (inner.daemon_pid.is_some(), inner.daemon_tmp_dir.clone())
    };
    if !has_daemon {
        return;
    }

    // Create the shutdown sentinel file.
    if let Some(ref dir) = tmp_dir {
        let shutdown_file = dir.join("shutdown");
        match std::fs::write(&shutdown_file, b"") {
            Ok(_) => log::info!("created shutdown sentinel: {}", shutdown_file.display()),
            Err(e) => log::warn!("failed to create shutdown sentinel: {}", e),
        }
    }

    // Give the daemon a few seconds to detect the file and clean up.
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(5);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let inner = state.lock().await;
        if inner.daemon_pid.is_none() {
            log::info!("daemon exited cleanly");
            return;
        }
        if start.elapsed() > timeout {
            break;
        }
    }
    log::warn!("daemon did not exit within {:?} of shutdown request", timeout);
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
