use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

/// Embedded frontend assets built from `web/dist/`.
/// During development if the directory doesn't exist, rust-embed will simply
/// serve nothing — the API still works and the frontend can be run separately
/// via `npm run dev` with a proxy to :4027.
#[derive(Embed)]
#[folder = "web/dist/"]
#[prefix = ""]
struct Assets;

/// Serve embedded static files. Falls back to `index.html` for SPA routing.
pub async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');

    // Try the exact path first
    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(Body::from(file.data.to_vec()))
            .unwrap();
    }

    // SPA fallback: return index.html for unmatched routes
    match Assets::get("index.html") {
        Some(file) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(file.data.to_vec()))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("frontend not built — run `npm run build` in web/"))
            .unwrap(),
    }
}
