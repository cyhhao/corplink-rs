use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::web::state::{AppState, ConnectionInfo, ProfileEntry, ProfileFormData, VpnStatus};

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

fn err_json<T: Serialize>(
    status: StatusCode,
    msg: impl Into<String>,
) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        status,
        Json(ApiResponse {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }),
    )
}

/// Validate that a profile name is safe (no path traversal).
fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("profile name cannot be empty".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("profile name contains invalid characters".into());
    }
    if name.starts_with('.') {
        return Err("profile name cannot start with '.'".into());
    }
    Ok(())
}

/// Build a ProfileEntry from a Config + filename stem.
fn profile_entry_from_config(name: &str, conf: &Config) -> ProfileEntry {
    ProfileEntry {
        name: name.to_string(),
        company: conf.company_name.clone(),
        username: conf.username.clone(),
        server: conf.server.clone(),
        platform: conf.platform.clone(),
        has_password: conf.password.as_ref().map_or(false, |p| !p.is_empty()),
        has_totp: conf.code.as_ref().map_or(false, |c| !c.is_empty()),
    }
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
                    profiles.push(profile_entry_from_config(&name, &conf));
                }
            }
        }
    }

    ok_json(profiles)
}

// ---------------------------------------------------------------------------
// GET /api/profiles/:name — get single profile details (safe subset)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ProfileDetail {
    pub name: String,
    pub company_name: String,
    pub username: String,
    pub platform: Option<String>,
    pub server: Option<String>,
    pub has_password: bool,
    pub has_totp: bool,
    pub vpn_server_name: Option<String>,
    pub vpn_select_strategy: Option<String>,
    pub use_vpn_dns: Option<bool>,
    pub use_full_route: Option<bool>,
    pub include_private_routes: Option<bool>,
    pub extra_routes: Option<Vec<String>>,
}

pub async fn get_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> (StatusCode, Json<ApiResponse<ProfileDetail>>) {
    if let Err(e) = validate_profile_name(&name) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }

    let inner = state.lock().await;
    let path = inner.profiles_dir.join(format!("{}.json", name));

    if !path.exists() {
        return err_json(
            StatusCode::NOT_FOUND,
            format!("profile '{}' not found", name),
        );
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            return err_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read error: {}", e),
            )
        }
    };

    let conf: Config = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            return err_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("parse error: {}", e),
            )
        }
    };

    ok_json(ProfileDetail {
        name,
        company_name: conf.company_name,
        username: conf.username,
        platform: conf.platform,
        server: conf.server,
        has_password: conf.password.as_ref().map_or(false, |p| !p.is_empty()),
        has_totp: conf.code.as_ref().map_or(false, |c| !c.is_empty()),
        vpn_server_name: conf.vpn_server_name,
        vpn_select_strategy: conf.vpn_select_strategy,
        use_vpn_dns: conf.use_vpn_dns,
        use_full_route: conf.use_full_route,
        include_private_routes: conf.include_private_routes,
        extra_routes: conf.extra_routes,
    })
}

// ---------------------------------------------------------------------------
// POST /api/profiles/:name — create a new profile
// ---------------------------------------------------------------------------

pub async fn create_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(form): Json<ProfileFormData>,
) -> (StatusCode, Json<ApiResponse<ProfileEntry>>) {
    if let Err(e) = validate_profile_name(&name) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }

    let inner = state.lock().await;
    let path = inner.profiles_dir.join(format!("{}.json", name));

    if path.exists() {
        return err_json(
            StatusCode::CONFLICT,
            format!("profile '{}' already exists", name),
        );
    }

    let conf = build_config_from_form(&form);

    match write_config(&path, &conf) {
        Ok(_) => ok_json(profile_entry_from_config(&name, &conf)),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ---------------------------------------------------------------------------
// PUT /api/profiles/:name — update an existing profile
// ---------------------------------------------------------------------------

pub async fn update_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(form): Json<ProfileFormData>,
) -> (StatusCode, Json<ApiResponse<ProfileEntry>>) {
    if let Err(e) = validate_profile_name(&name) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }

    let inner = state.lock().await;
    let path = inner.profiles_dir.join(format!("{}.json", name));

    if !path.exists() {
        return err_json(
            StatusCode::NOT_FOUND,
            format!("profile '{}' not found", name),
        );
    }

    let existing: Config = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
    {
        Some(c) => c,
        None => {
            return err_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read existing profile",
            )
        }
    };

    let mut conf = build_config_from_form(&form);
    conf.device_id = existing.device_id;
    conf.device_name = existing.device_name.or(conf.device_name);
    conf.public_key = existing.public_key;
    conf.private_key = existing.private_key;
    conf.interface_name = existing.interface_name.or(conf.interface_name);
    conf.state = existing.state;
    conf.conf_file = Some(path.to_string_lossy().to_string());
    if conf.password.as_ref().map_or(true, |p| p.is_empty()) {
        conf.password = existing.password;
    }
    if conf.code.as_ref().map_or(true, |c| c.is_empty()) {
        conf.code = existing.code;
    }

    match write_config(&path, &conf) {
        Ok(_) => ok_json(profile_entry_from_config(&name, &conf)),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ---------------------------------------------------------------------------
// DELETE /api/profiles/:name — delete a profile
// ---------------------------------------------------------------------------

pub async fn delete_profile(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    if let Err(e) = validate_profile_name(&name) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }

    let inner = state.lock().await;
    if inner.active_profile.as_deref() == Some(&name)
        && (inner.status == VpnStatus::Connected || inner.status == VpnStatus::Connecting)
    {
        return err_json(
            StatusCode::CONFLICT,
            "cannot delete the active profile while connected",
        );
    }

    let path = inner.profiles_dir.join(format!("{}.json", name));

    if !path.exists() {
        return err_json(
            StatusCode::NOT_FOUND,
            format!("profile '{}' not found", name),
        );
    }

    match std::fs::remove_file(&path) {
        Ok(_) => ok_json(()),
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to delete profile: {}", e),
        ),
    }
}

// ---------------------------------------------------------------------------
// Profile helpers
// ---------------------------------------------------------------------------

fn build_config_from_form(form: &ProfileFormData) -> Config {
    Config {
        company_name: form.company_name.clone(),
        username: form.username.clone(),
        password: form.password.clone(),
        platform: form.platform.clone(),
        code: form.code.clone(),
        server: form.server.clone(),
        device_name: None,
        device_id: None,
        public_key: None,
        private_key: None,
        interface_name: None,
        debug_wg: None,
        conf_file: None,
        state: None,
        vpn_server_name: form.vpn_server_name.clone(),
        vpn_select_strategy: form.vpn_select_strategy.clone(),
        use_vpn_dns: form.use_vpn_dns,
        use_full_route: form.use_full_route,
        include_private_routes: form.include_private_routes,
        extra_routes: form.extra_routes.clone(),
    }
}

fn write_config(path: &std::path::Path, conf: &Config) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(conf).map_err(|e| format!("serialize error: {}", e))?;
    std::fs::write(path, json).map_err(|e| format!("write error: {}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /api/connect — spawn a privileged connect-daemon child process
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub profile: String,
}

pub async fn connect(
    State(state): State<AppState>,
    Json(req): Json<ConnectRequest>,
) -> (StatusCode, Json<ApiResponse<ConnectionInfo>>) {
    if let Err(e) = validate_profile_name(&req.profile) {
        return err_json(StatusCode::BAD_REQUEST, e);
    }

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

        inner.status = VpnStatus::Connecting;
        inner.active_profile = Some(req.profile.clone());
        inner.last_error = None;

        path
    };

    let state_clone = state.clone();
    let profile_path_str = profile_path.to_string_lossy().to_string();

    tokio::spawn(async move {
        if let Err(e) = do_connect(state_clone.clone(), &profile_path_str).await {
            let mut inner = state_clone.lock().await;
            inner.status = VpnStatus::Error;
            inner.last_error = Some(e);
        }
    });

    let inner = state.lock().await;
    ok_json(inner.connection_info())
}

// ---------------------------------------------------------------------------
// Privileged daemon spawning & event reading
// ---------------------------------------------------------------------------

/// JSON events emitted by the connect-daemon child (one per line on the pipe).
#[derive(Deserialize)]
struct DaemonEvent {
    event: String,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    vpn_ip: Option<String>,
    #[serde(default)]
    peer_address: Option<String>,
    #[serde(default)]
    server_name: Option<String>,
    #[serde(default)]
    use_full_route: Option<bool>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// Create a temporary directory with a named pipe (FIFO) for daemon IPC.
fn create_event_pipe() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let tmp_dir = std::env::temp_dir().join(format!("corplink-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("failed to create temp dir: {}", e))?;
    let pipe_path = tmp_dir.join("events");
    let _ = std::fs::remove_file(&pipe_path);
    let status = std::process::Command::new("mkfifo")
        .arg(&pipe_path)
        .status()
        .map_err(|e| format!("mkfifo failed: {}", e))?;
    if !status.success() {
        return Err("mkfifo failed".into());
    }
    Ok((tmp_dir, pipe_path))
}

/// Request the daemon to shut down by creating a sentinel file.
///
/// The daemon (running as root) polls for this file every 500 ms.
/// We use a file instead of Unix signals because a non-root parent
/// process cannot send signals to a root child (EPERM).
fn request_daemon_shutdown(tmp_dir: &Option<std::path::PathBuf>) {
    if let Some(ref dir) = tmp_dir {
        let shutdown_file = dir.join("shutdown");
        match std::fs::write(&shutdown_file, b"") {
            Ok(_) => log::info!("created shutdown sentinel: {}", shutdown_file.display()),
            Err(e) => log::warn!("failed to create shutdown sentinel: {}", e),
        }
    } else {
        log::warn!("no daemon tmp_dir — cannot create shutdown sentinel");
    }
}

/// Build the privileged command for the connect-daemon.
///
/// On macOS, uses `sudo` which triggers Touch ID via `pam_tid.so`
/// (requires `/etc/pam.d/sudo_local` with `auth sufficient pam_tid.so`).
/// Falls back to password prompt if Touch ID is not configured.
#[cfg(target_os = "macos")]
fn build_privileged_command(
    exe: &std::path::Path,
    config_path: &str,
    event_pipe: &std::path::Path,
) -> tokio::process::Command {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let log_file = std::path::Path::new(config_path)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("logs").join("daemon-stderr.log"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/corplink-daemon-stderr.log"));

    // Use a shell wrapper so we can redirect stdout/stderr to the log file.
    // `sudo` with `pam_tid.so` will show the Touch ID dialog.
    let inner_cmd = format!(
        "'{exe}' connect-daemon --config '{config}' --event-pipe '{pipe}' --owner-uid {uid} --owner-gid {gid} >>'{log}' 2>&1",
        exe = exe.display(),
        config = config_path,
        pipe = event_pipe.display(),
        uid = uid,
        gid = gid,
        log = log_file.display(),
    );

    let mut cmd = tokio::process::Command::new("sudo");
    cmd.args(["--stdin", "/bin/sh", "-c", &inner_cmd]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd
}

/// Fallback for non-macOS: use sudo (prompts in terminal).
#[cfg(not(target_os = "macos"))]
fn build_privileged_command(
    exe: &std::path::Path,
    config_path: &str,
    event_pipe: &std::path::Path,
) -> tokio::process::Command {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let mut cmd = tokio::process::Command::new("sudo");
    cmd.args([
        exe.to_string_lossy().as_ref(),
        "connect-daemon",
        "--config",
        config_path,
        "--event-pipe",
        event_pipe.to_string_lossy().as_ref(),
        "--owner-uid",
        &uid.to_string(),
        "--owner-gid",
        &gid.to_string(),
    ]);
    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd
}

async fn do_connect(state: AppState, config_path: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find self: {}", e))?;

    // Create FIFO for daemon → parent communication.
    let (tmp_dir, event_pipe) = create_event_pipe()?;

    // Spawn the privileged daemon via osascript (Touch ID) or sudo.
    let mut child = build_privileged_command(&exe, config_path, &event_pipe)
        .spawn()
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            format!("failed to launch privileged helper: {}", e)
        })?;

    // Open the FIFO for reading.  This blocks until the daemon opens it for
    // writing, so we race it against the child exiting (e.g. user cancelled
    // the auth dialog).
    let pipe_path_clone = event_pipe.clone();
    let open_result = tokio::select! {
        result = tokio::task::spawn_blocking(move || std::fs::File::open(&pipe_path_clone)) => {
            match result {
                Ok(Ok(file)) => Ok(file),
                Ok(Err(e)) => Err(format!("failed to open event pipe: {}", e)),
                Err(e) => Err(format!("open task panicked: {}", e)),
            }
        }
        status = child.wait() => {
            let code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
            if code == 0 {
                Err("privileged helper exited unexpectedly".into())
            } else {
                Err("authorization cancelled or denied".into())
            }
        }
    };

    let pipe_file = match open_result {
        Ok(f) => f,
        Err(e) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }
    };

    // Read daemon events in a blocking task (BufReader on a FIFO).
    let reader = std::io::BufReader::new(pipe_file);
    let state_for_reader = state.clone();
    let tmp_dir_for_cleanup = tmp_dir.clone();

    tokio::task::spawn_blocking(move || {
        use std::io::BufRead;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let ev: DaemonEvent = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let state = state_for_reader.clone();
            let handle = tokio::runtime::Handle::current();

            match ev.event.as_str() {
                "started" => {
                    if let Some(pid) = ev.pid {
                        handle.block_on(async {
                            let mut inner = state.lock().await;
                            inner.daemon_pid = Some(pid);
                            inner.daemon_tmp_dir = Some(tmp_dir_for_cleanup.clone());
                        });
                        log::info!("daemon started with pid {}", pid);
                    }
                }
                "connected" => {
                    handle.block_on(async {
                        let mut inner = state.lock().await;
                        inner.status = VpnStatus::Connected;
                        inner.vpn_ip = ev.vpn_ip;
                        inner.peer_address = ev.peer_address;
                        inner.server_name = ev.server_name;
                        inner.use_full_route = ev.use_full_route;
                        inner.connected_since = Some(chrono::Utc::now());
                    });
                    log::info!("VPN connected");
                }
                "error" => {
                    let msg = ev.message.unwrap_or_else(|| "unknown error".into());
                    handle.block_on(async {
                        let mut inner = state.lock().await;
                        inner.status = VpnStatus::Error;
                        inner.last_error = Some(msg.clone());
                    });
                    log::error!("daemon error: {}", msg);
                    break;
                }
                "disconnected" => {
                    let reason = ev.reason.unwrap_or_else(|| "unknown".into());
                    handle.block_on(async {
                        let mut inner = state.lock().await;
                        inner.status = VpnStatus::Disconnected;
                        inner.last_error = Some(format!("connection lost ({})", reason));
                        inner.reset_connection();
                        inner.active_profile = None;
                    });
                    log::info!("daemon disconnected: {}", reason);
                    break;
                }
                _ => {}
            }
        }

        // Pipe closed — daemon exited.  Ensure state is cleaned up.
        let state = state_for_reader;
        let handle = tokio::runtime::Handle::current();
        handle.block_on(async {
            let mut inner = state.lock().await;
            if inner.status == VpnStatus::Connected || inner.status == VpnStatus::Connecting {
                inner.status = VpnStatus::Disconnected;
                if inner.last_error.is_none() {
                    inner.last_error = Some("daemon exited unexpectedly".into());
                }
                inner.reset_connection();
                inner.active_profile = None;
            }
        });

        let _ = std::fs::remove_dir_all(&tmp_dir_for_cleanup);
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// POST /api/disconnect — request daemon shutdown via sentinel file
// ---------------------------------------------------------------------------

pub async fn disconnect(
    State(state): State<AppState>,
) -> (StatusCode, Json<ApiResponse<ConnectionInfo>>) {
    let (daemon_pid, tmp_dir) = {
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
        (inner.daemon_pid, inner.daemon_tmp_dir.clone())
    };

    if daemon_pid.is_some() {
        // Signal the daemon to shut down by creating a sentinel file.
        // We cannot use libc::kill() because the daemon runs as root and
        // we are a normal user — the kernel would return EPERM.
        request_daemon_shutdown(&tmp_dir);

        // Wait for daemon to exit gracefully.  The daemon cleanup order is:
        //   1. stop_wg_go (instant — destroys TUN, removes routes)
        //   2. restore_dns (fast)
        //   3. delete peer route (fast)
        //   4. disconnect_vpn API call (up to 3s timeout)
        // Total expected: <5s.  Allow up to 8s before giving up.
        let grace = std::time::Duration::from_secs(8);
        let start = std::time::Instant::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let inner = state.lock().await;
            if inner.status == VpnStatus::Disconnected || inner.daemon_pid.is_none() {
                break;
            }
            if start.elapsed() > grace {
                log::warn!("daemon did not exit after {:?}", grace);
                break;
            }
        }
    } else {
        let mut inner = state.lock().await;
        inner.status = VpnStatus::Disconnected;
        inner.reset_connection();
        inner.active_profile = None;
    }

    // Ensure state is cleaned up regardless.
    {
        let mut inner = state.lock().await;
        if inner.status != VpnStatus::Disconnected {
            inner.status = VpnStatus::Disconnected;
            inner.reset_connection();
            inner.active_profile = None;
        }
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

// ---------------------------------------------------------------------------
// GET /api/logs
// ---------------------------------------------------------------------------

pub async fn get_logs() -> (StatusCode, Json<ApiResponse<Vec<String>>>) {
    ok_json(crate::logging::recent_logs())
}

// ---------------------------------------------------------------------------
// POST /api/reconnect — kill daemon, update config, reconnect
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReconnectRequest {
    pub vpn_server_name: Option<String>,
    pub use_full_route: Option<bool>,
}

pub async fn reconnect(
    State(state): State<AppState>,
    Json(req): Json<ReconnectRequest>,
) -> (StatusCode, Json<ApiResponse<ConnectionInfo>>) {
    let (_active_profile, profile_path) = {
        let inner = state.lock().await;
        match inner.status {
            VpnStatus::Connected => {}
            _ => {
                return err_json(
                    StatusCode::CONFLICT,
                    format!("cannot reconnect in {:?} state", inner.status),
                );
            }
        }
        let profile = match &inner.active_profile {
            Some(p) => p.clone(),
            None => return err_json(StatusCode::CONFLICT, "no active profile".to_string()),
        };
        let path = inner.profiles_dir.join(format!("{}.json", profile));
        (profile, path)
    };

    // Update profile config on disk.
    if let Ok(mut conf) = Config::from_file(&profile_path.to_string_lossy()).await {
        let mut changed = false;
        if let Some(ref name) = req.vpn_server_name {
            conf.vpn_server_name = if name.is_empty() {
                None
            } else {
                Some(name.clone())
            };
            changed = true;
        }
        if let Some(full) = req.use_full_route {
            conf.use_full_route = Some(full);
            changed = true;
        }
        if changed {
            let _ = conf.save().await;
        }
    }

    // Kill current daemon.
    let tmp_dir = {
        let mut inner = state.lock().await;
        inner.status = VpnStatus::Connecting;
        inner.last_error = None;
        inner.daemon_tmp_dir.clone()
    };

    if tmp_dir.is_some() {
        request_daemon_shutdown(&tmp_dir);
        // Wait for daemon to fully exit (reader task resets daemon_pid).
        let start = std::time::Instant::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let inner = state.lock().await;
            if inner.daemon_pid.is_none() {
                break;
            }
            if start.elapsed() > std::time::Duration::from_secs(8) {
                log::warn!("reconnect: daemon did not exit after 8s");
                break;
            }
        }
    }

    {
        let mut inner = state.lock().await;
        inner.reset_connection();
    }

    // Reconnect in background.
    let state_clone = state.clone();
    let path_str = profile_path.to_string_lossy().to_string();
    tokio::spawn(async move {
        if let Err(e) = do_connect(state_clone.clone(), &path_str).await {
            let mut inner = state_clone.lock().await;
            inner.status = VpnStatus::Error;
            inner.last_error = Some(e);
        }
    });

    let inner = state.lock().await;
    ok_json(inner.connection_info())
}

// ---------------------------------------------------------------------------
// GET /api/vpn-servers/:profile
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct VpnServerEntry {
    pub name: String,
    pub en_name: String,
    pub ip: String,
    pub vpn_port: u16,
    pub protocol: String,
}

pub async fn list_vpn_servers(
    State(state): State<AppState>,
    Path(profile): Path<String>,
) -> (StatusCode, Json<ApiResponse<Vec<VpnServerEntry>>>) {
    match do_list_vpn_servers(state, &profile).await {
        Ok(servers) => ok_json(servers),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn do_list_vpn_servers(
    state: AppState,
    profile: &str,
) -> Result<Vec<VpnServerEntry>, String> {
    use crate::client::Client;

    let profile_path = {
        let inner = state.lock().await;
        inner.profiles_dir.join(format!("{}.json", profile))
    };
    if !profile_path.exists() {
        return Err(format!("profile '{}' not found", profile));
    }

    let mut conf = Config::from_file(&profile_path.to_string_lossy()).await?;

    if conf.server.is_none() {
        match crate::client::get_company_url(conf.company_name.as_str()).await {
            Ok(resp) => {
                conf.server = Some(resp.domain);
                let _ = conf.save().await;
            }
            Err(e) => return Err(format!("failed to resolve server: {}", e)),
        }
    }

    let mut client =
        Client::new_headless(conf).map_err(|e| format!("client init failed: {}", e))?;

    if client.need_login() {
        client
            .login()
            .await
            .map_err(|e| format!("login failed: {}", e))?;
    }

    let vpn_list = client
        .list_vpn()
        .await
        .map_err(|e| format!("failed to list vpn servers: {}", e))?;

    let servers = vpn_list
        .into_iter()
        .map(|v| VpnServerEntry {
            name: v.name,
            en_name: v.en_name,
            ip: v.ip,
            vpn_port: v.vpn_port,
            protocol: match v.protocol_mode {
                1 => "tcp".to_string(),
                2 => "udp".to_string(),
                _ => "unknown".to_string(),
            },
        })
        .collect();

    Ok(servers)
}
