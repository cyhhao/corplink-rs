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
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE]);

    let api_routes = Router::new()
        .route("/api/status", get(api::get_status))
        .route("/api/profiles", get(api::list_profiles))
        .route("/api/connect", post(api::connect))
        .route("/api/disconnect", post(api::disconnect))
        .route("/api/version", get(api::get_version));

    Router::new()
        .merge(api_routes)
        // Fallback: serve embedded SPA assets for everything else
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
