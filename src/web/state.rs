use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

use crate::client::Client;
use crate::config::WgConf;

/// Connection status visible to the web UI and CLI.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VpnStatus {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Error,
}

/// A loaded profile entry (derived from a JSON file under ~/.config/corplink/profiles/).
#[derive(Debug, Clone, Serialize)]
pub struct ProfileEntry {
    /// Filename stem, e.g. "office-bj"
    pub name: String,
    /// Company name from the config
    pub company: String,
    /// Username
    pub username: String,
    /// Server URL (if resolved)
    pub server: Option<String>,
}

/// Snapshot of current connection for API consumers.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionInfo {
    pub status: VpnStatus,
    pub profile: Option<String>,
    pub vpn_ip: Option<String>,
    pub peer_address: Option<String>,
    pub connected_since: Option<String>,
    pub error: Option<String>,
}

/// Inner mutable state behind the Arc<Mutex>.
pub struct AppStateInner {
    pub status: VpnStatus,
    pub active_profile: Option<String>,
    pub client: Option<Client>,
    pub wg_conf: Option<WgConf>,
    pub connected_since: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
    /// Directory holding profile JSON files.
    pub profiles_dir: std::path::PathBuf,
}

impl AppStateInner {
    pub fn connection_info(&self) -> ConnectionInfo {
        ConnectionInfo {
            status: self.status.clone(),
            profile: self.active_profile.clone(),
            vpn_ip: self.wg_conf.as_ref().map(|c| c.address.clone()),
            peer_address: self.wg_conf.as_ref().map(|c| c.peer_address.clone()),
            connected_since: self.connected_since.map(|t| t.to_rfc3339()),
            error: self.last_error.clone(),
        }
    }
}

/// Thread-safe shared state handle.
pub type AppState = Arc<Mutex<AppStateInner>>;

pub fn new_app_state(profiles_dir: std::path::PathBuf) -> AppState {
    Arc::new(Mutex::new(AppStateInner {
        status: VpnStatus::Disconnected,
        active_profile: None,
        client: None,
        wg_conf: None,
        connected_since: None,
        last_error: None,
        profiles_dir,
    }))
}
