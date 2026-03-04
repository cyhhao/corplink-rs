use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::web::state::{AppState, ConnectionInfo, ProfileEntry, VpnStatus};

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn ok_json<T: Serialize>(data: T) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        StatusCode::OK,
        Json(ApiResponse {
            ok: true,
            data: Some(data),
            error: None,
        }),
    )
}

fn err_json<T: Serialize>(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        status,
        Json(ApiResponse {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }),
    )
}

// ---------------------------------------------------------------------------
// GET /api/status
// ---------------------------------------------------------------------------

pub async fn get_status(
    State(state): State<AppState>,
) -> (StatusCode, Json<ApiResponse<ConnectionInfo>>) {
    let inner = state.lock().await;
    ok_json(inner.connection_info())
}

// ---------------------------------------------------------------------------
// GET /api/profiles
// ---------------------------------------------------------------------------

pub async fn list_profiles(
    State(state): State<AppState>,
) -> (StatusCode, Json<ApiResponse<Vec<ProfileEntry>>>) {
    let inner = state.lock().await;
    let dir = &inner.profiles_dir;

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            return err_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read profiles dir: {}", e),
            );
        }
    };

    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "json") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(conf) = serde_json::from_str::<Config>(&content) {
                    let name = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    profiles.push(ProfileEntry {
                        name,
                        company: conf.company_name.clone(),
                        username: conf.username.clone(),
                        server: conf.server.clone(),
                    });
                }
            }
        }
    }

    ok_json(profiles)
}

// ---------------------------------------------------------------------------
// POST /api/connect
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub profile: String,
}

/// Validate that a profile name is safe (no path traversal).
fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("profile name cannot be empty".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("profile name contains invalid characters".into());
    }
    // Also reject hidden files
    if name.starts_with('.') {
        return Err("profile name cannot start with '.'".into());
    }
    Ok(())
}

pub async fn connect(
    State(state): State<AppState>,
    Json(req): Json<ConnectRequest>,
) -> (StatusCode, Json<ApiResponse<ConnectionInfo>>) {
    // Validate profile name against path traversal
    if let Err(e) = validate_profile_name(&req.profile) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }

    // Atomic check-and-set: status check + mark Connecting in one lock scope.
    // This prevents two concurrent requests from both passing the check.
    let profile_path = {
        let mut inner = state.lock().await;
        if inner.status == VpnStatus::Connected || inner.status == VpnStatus::Connecting {
            return err_json(
                StatusCode::CONFLICT,
                format!("already {:?}", inner.status),
            );
        }

        let path = inner.profiles_dir.join(format!("{}.json", req.profile));
        if !path.exists() {
            return err_json(
                StatusCode::NOT_FOUND,
                format!("profile '{}' not found", req.profile),
            );
        }

        // Atomically mark as connecting
        inner.status = VpnStatus::Connecting;
        inner.active_profile = Some(req.profile.clone());
        inner.last_error = None;

        path
    };
    // Lock released here

    // Spawn the heavy VPN work in a background task so the HTTP response
    // returns immediately.
    let state_clone = state.clone();
    let profile_path_str = profile_path.to_string_lossy().to_string();

    tokio::spawn(async move {
        if let Err(e) = do_connect(state_clone.clone(), &profile_path_str).await {
            let mut inner = state_clone.lock().await;
            inner.status = VpnStatus::Error;
            inner.last_error = Some(e);
        }
    });

    // Return current (connecting) status
    let inner = state.lock().await;
    ok_json(inner.connection_info())
}

async fn do_connect(state: AppState, config_path: &str) -> Result<(), String> {
    use crate::client::Client;
    use crate::wg;

    let mut conf = Config::from_file(config_path).await?;

    // Resolve server if needed
    if conf.server.is_none() {
        match crate::client::get_company_url(conf.company_name.as_str()).await {
            Ok(resp) => {
                conf.server = Some(resp.domain);
                let _ = conf.save().await;
            }
            Err(e) => return Err(format!("failed to resolve server: {}", e)),
        }
    }

    let interface_name = conf
        .interface_name
        .clone()
        .ok_or_else(|| "interface_name not set in config".to_string())?;
    let with_wg_log = conf.debug_wg.unwrap_or_default();

    #[cfg(target_os = "macos")]
    let _use_vpn_dns = conf.use_vpn_dns.unwrap_or(false);

    let mut client = Client::new(conf).map_err(|e| format!("client init failed: {}", e))?;

    // Login if needed
    let mut logout_retry = true;
    loop {
        if client.need_login() {
            client
                .login()
                .await
                .map_err(|e| format!("login failed: {}", e))?;
        }
        match client.connect_vpn().await {
            Ok(wg_conf) => {
                // Ensure peer route
                if let Some(peer_ip) = extract_peer_host(&wg_conf.peer_address) {
                    if let Err(e) = client.ensure_peer_route(&peer_ip).await {
                        log::warn!("failed to ensure peer route: {}", e);
                    }
                }

                // Start WireGuard
                let protocol = wg_conf.protocol;
                if !wg::start_wg_go(&interface_name, protocol, with_wg_log) {
                    return Err(format!(
                        "failed to start wg-corplink for {}",
                        interface_name
                    ));
                }

                let mut uapi = wg::UAPIClient {
                    name: interface_name.clone(),
                };
                // FIX #4: if config_wg fails, stop WG to avoid resource leak
                if let Err(e) = uapi.config_wg(&wg_conf).await {
                    wg::stop_wg_go();
                    return Err(format!("wg config failed: {}", e));
                }

                // Store state
                {
                    let mut inner = state.lock().await;
                    inner.status = VpnStatus::Connected;
                    inner.client = Some(client.clone());
                    inner.wg_conf = Some(wg_conf.clone());
                    inner.connected_since = Some(chrono::Utc::now());
                }

                // Spawn keep-alive + handshake checker
                let state_ka = state.clone();
                let wg_conf_ka = wg_conf.clone();
                let mut client_ka = client.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        _ = client_ka.keep_alive_vpn(&wg_conf_ka, 60) => {
                            log::warn!("keep-alive ended");
                        }
                        _ = async {
                            let mut uapi = wg::UAPIClient { name: interface_name.clone() };
                            uapi.check_wg_connection().await;
                            log::warn!("wg handshake timeout");
                        } => {}
                    }

                    // FIX #5: Connection lost — clean up WG and all state fields
                    // Best-effort disconnect from server
                    let _ = client_ka.disconnect_vpn(&wg_conf_ka).await;
                    wg::stop_wg_go();

                    let mut inner = state_ka.lock().await;
                    if inner.status == VpnStatus::Connected {
                        inner.status = VpnStatus::Disconnected;
                        inner.last_error = Some("connection lost (timeout)".to_string());
                        inner.client = None;
                        inner.wg_conf = None;
                        inner.connected_since = None;
                        inner.active_profile = None;
                    }
                });

                return Ok(());
            }
            Err(e) => {
                if logout_retry && e.to_string().contains("logout") {
                    log::warn!("{}", e);
                    logout_retry = false;
                    continue;
                }
                return Err(format!("connect failed: {}", e));
            }
        }
    }
}

fn extract_peer_host(peer_address: &str) -> Option<String> {
    if peer_address.starts_with('[') {
        let end = peer_address.find(']')?;
        Some(peer_address[1..end].to_string())
    } else if let Some(idx) = peer_address.rfind(':') {
        Some(peer_address[..idx].to_string())
    } else if peer_address.is_empty() {
        None
    } else {
        Some(peer_address.to_string())
    }
}

// ---------------------------------------------------------------------------
// POST /api/disconnect
// ---------------------------------------------------------------------------

pub async fn disconnect(
    State(state): State<AppState>,
) -> (StatusCode, Json<ApiResponse<ConnectionInfo>>) {
    // FIX #3: Extract client + wg_conf from state, then release lock before
    // any async work, so we don't block other tasks.
    // FIX #4: Also allow disconnect from Error state so leaked WG can be cleaned.
    let (client, wg_conf) = {
        let mut inner = state.lock().await;
        match inner.status {
            VpnStatus::Connected | VpnStatus::Error => {}
            _ => {
                return err_json(
                    StatusCode::CONFLICT,
                    format!("cannot disconnect in {:?} state", inner.status),
                );
            }
        }
        inner.status = VpnStatus::Disconnecting;
        // Take ownership — moves out of state
        (inner.client.take(), inner.wg_conf.take())
    };
    // Lock released here — no .await while holding the lock

    // FIX #6: Capture disconnect error to report it
    let mut disconnect_error: Option<String> = None;

    if let (Some(mut client), Some(wg_conf)) = (client, wg_conf) {
        if let Err(e) = client.disconnect_vpn(&wg_conf).await {
            let msg = format!("server disconnect failed: {}", e);
            log::warn!("{}", msg);
            disconnect_error = Some(msg);
        }
    }
    crate::wg::stop_wg_go();

    // Clean up state
    {
        let mut inner = state.lock().await;
        inner.status = VpnStatus::Disconnected;
        inner.client = None;
        inner.wg_conf = None;
        inner.connected_since = None;
        inner.active_profile = None;
        inner.last_error = disconnect_error;
    }

    let inner = state.lock().await;
    ok_json(inner.connection_info())
}

// ---------------------------------------------------------------------------
// GET /api/version
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct VersionInfo {
    pub name: &'static str,
    pub version: &'static str,
}

pub async fn get_version() -> (StatusCode, Json<ApiResponse<VersionInfo>>) {
    ok_json(VersionInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
    })
}
