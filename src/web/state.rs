use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

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
    /// Login platform
    pub platform: Option<String>,
    /// Has password configured
    pub has_password: bool,
    /// Has TOTP secret configured
    pub has_totp: bool,
}

/// Data for creating / updating a profile from the web UI.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileFormData {
    pub company_name: String,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub vpn_server_name: Option<String>,
    #[serde(default)]
    pub vpn_select_strategy: Option<String>,
    #[serde(default)]
    pub use_vpn_dns: Option<bool>,
    #[serde(default)]
    pub use_full_route: Option<bool>,
    #[serde(default)]
    pub include_private_routes: Option<bool>,
    #[serde(default)]
    pub extra_routes: Option<Vec<String>>,
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
    pub server_name: Option<String>,
    pub use_full_route: Option<bool>,
    /// Number of orphan connect-daemon processes detected (only checked when
    /// the UI state is Disconnected or Error).
    pub orphan_processes: u32,
}

/// Inner mutable state behind the Arc<Mutex>.
pub struct AppStateInner {
    pub status: VpnStatus,
    pub active_profile: Option<String>,
    /// PID of the privileged connect-daemon child process.
    pub daemon_pid: Option<u32>,
    /// Temp directory holding the event pipe and shutdown sentinel.
    pub daemon_tmp_dir: Option<std::path::PathBuf>,
    pub vpn_ip: Option<String>,
    pub peer_address: Option<String>,
    pub connected_since: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
    /// Directory holding profile JSON files.
    pub profiles_dir: std::path::PathBuf,
    pub server_name: Option<String>,
    pub use_full_route: Option<bool>,
}

impl AppStateInner {
    pub fn connection_info(&self) -> ConnectionInfo {
        ConnectionInfo {
            status: self.status.clone(),
            profile: self.active_profile.clone(),
            vpn_ip: self.vpn_ip.clone(),
            peer_address: self.peer_address.clone(),
            connected_since: self.connected_since.map(|t| t.to_rfc3339()),
            error: self.last_error.clone(),
            server_name: self.server_name.clone(),
            use_full_route: self.use_full_route,
            orphan_processes: 0, // populated by the API handler
        }
    }

    /// Reset all connection-related fields to their disconnected defaults.
    pub fn reset_connection(&mut self) {
        self.daemon_pid = None;
        self.daemon_tmp_dir = None;
        self.vpn_ip = None;
        self.peer_address = None;
        self.connected_since = None;
        self.server_name = None;
        self.use_full_route = None;
    }
}

/// Thread-safe shared state handle.
pub type AppState = Arc<Mutex<AppStateInner>>;

pub fn new_app_state(profiles_dir: std::path::PathBuf) -> AppState {
    Arc::new(Mutex::new(AppStateInner {
        status: VpnStatus::Disconnected,
        active_profile: None,
        daemon_pid: None,
        daemon_tmp_dir: None,
        vpn_ip: None,
        peer_address: None,
        connected_since: None,
        last_error: None,
        profiles_dir,
        server_name: None,
        use_full_route: None,
    }))
}
