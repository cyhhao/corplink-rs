pub mod api;
pub mod embedded;
pub mod state;

use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use state::AppState;

/// Build the full Axum router with API routes and embedded frontend.
pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

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
    let app = build_router(state);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    log::info!("web UI listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
