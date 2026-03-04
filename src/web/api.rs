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

pub async fn connect(
    State(state): State<AppState>,
    Json(req): Json<ConnectRequest>,
) -> (StatusCode, Json<ApiResponse<ConnectionInfo>>) {
    // Quick check: already connected?
    {
        let inner = state.lock().await;
        if inner.status == VpnStatus::Connected || inner.status == VpnStatus::Connecting {
            return err_json(
                StatusCode::CONFLICT,
                format!("already {:?}", inner.status),
            );
        }
    }

    // Resolve profile path
    let profile_path = {
        let inner = state.lock().await;
        inner.profiles_dir.join(format!("{}.json", req.profile))
    };

    if !profile_path.exists() {
        return err_json(
            StatusCode::NOT_FOUND,
            format!("profile '{}' not found", req.profile),
        );
    }

    // Mark as connecting
    {
        let mut inner = state.lock().await;
        inner.status = VpnStatus::Connecting;
        inner.active_profile = Some(req.profile.clone());
        inner.last_error = None;
    }

    // Spawn the heavy VPN work in a background task so the HTTP response
    // returns immediately.
    let state_clone = state.clone();
    let _profile_name = req.profile.clone();
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

    let interface_name = conf.interface_name.clone()
        .ok_or_else(|| "interface_name not set in config".to_string())?;
    let with_wg_log = conf.debug_wg.unwrap_or_default();

    #[cfg(target_os = "macos")]
    let use_vpn_dns = conf.use_vpn_dns.unwrap_or(false);

    let mut client = Client::new(conf).map_err(|e| format!("client init failed: {}", e))?;

    // Login if needed
    let mut logout_retry = true;
    loop {
        if client.need_login() {
            client.login().await.map_err(|e| format!("login failed: {}", e))?;
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
                    return Err(format!("failed to start wg-corplink for {}", interface_name));
                }

                let mut uapi = wg::UAPIClient {
                    name: interface_name.clone(),
                };
                uapi.config_wg(&wg_conf)
                    .await
                    .map_err(|e| format!("wg config failed: {}", e))?;

                // Set DNS if requested
                #[cfg(target_os = "macos")]
                if use_vpn_dns {
                    let dns_manager = crate::dns::DNSManager::new();
                    // DNS management is best-effort
                    let _ = dns_manager;
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
                    // Connection lost — update state
                    let mut inner = state_ka.lock().await;
                    if inner.status == VpnStatus::Connected {
                        inner.status = VpnStatus::Disconnected;
                        inner.last_error = Some("connection lost (timeout)".to_string());
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
    let (has_connection, wg_conf) = {
        let mut inner = state.lock().await;
        if inner.status != VpnStatus::Connected {
            return err_json(StatusCode::CONFLICT, "not connected");
        }
        inner.status = VpnStatus::Disconnecting;
        (inner.client.is_some(), inner.wg_conf.clone())
    };

    if has_connection {
        if let Some(wg_conf) = &wg_conf {
            let mut inner = state.lock().await;
            if let Some(ref mut client) = inner.client {
                let _ = client.disconnect_vpn(wg_conf).await;
            }
        }
        crate::wg::stop_wg_go();
    }

    {
        let mut inner = state.lock().await;
        inner.status = VpnStatus::Disconnected;
        inner.client = None;
        inner.wg_conf = None;
        inner.connected_since = None;
        inner.active_profile = None;
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
