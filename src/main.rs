mod api;
mod cli;
mod client;
mod config;
mod dns;
mod logging;
mod qrcode;
mod resp;
mod state;
mod template;
mod totp;
mod utils;
mod web;
mod wg;

#[cfg(windows)]
use is_elevated;

#[cfg(target_os = "macos")]
use dns::DNSManager;

use clap::Parser;
use std::path::PathBuf;
use std::process::exit;

use cli::{Cli, Command};
use client::Client;
use config::{Config, WgConf};

pub const EPERM: i32 = 1;
pub const ENOENT: i32 = 2;
pub const ETIMEDOUT: i32 = 110;

/// Return the base XDG config directory (`~/.config`), respecting `SUDO_USER`
/// when running under sudo (for `connect` / `legacy` commands).
fn real_config_dir() -> PathBuf {
    #[cfg(unix)]
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() && sudo_user != "root" {
            #[cfg(target_os = "macos")]
            {
                return PathBuf::from(format!("/Users/{}/.config", sudo_user));
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                return PathBuf::from(format!("/home/{}/.config", sudo_user));
            }
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
}

/// After writing a file as root (in connect-daemon), fix ownership so the
/// normal user still owns their config files.
#[cfg(unix)]
pub(crate) fn chown_to_user(path: &std::path::Path, uid: u32, gid: u32) {
    let _ = std::process::Command::new("chown")
        .args([
            &format!("{}:{}", uid, gid),
            &path.to_string_lossy().into_owned(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Return the corplink config root: `~/.config/corplink`.
fn config_dir() -> PathBuf {
    real_config_dir().join("corplink")
}

/// Return the profiles directory (`~/.config/corplink/profiles`), creating it if necessary.
fn profiles_dir() -> PathBuf {
    let dir = config_dir().join("profiles");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| {
            log::error!(
                "failed to create profiles directory {}: {}",
                dir.display(),
                e
            );
            exit(EPERM);
        });
    }
    dir
}

/// Return the logs directory (`~/.config/corplink/logs`), creating it if necessary.
fn logs_dir() -> PathBuf {
    let dir = config_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Return the cookies directory (`~/.config/corplink/cookies`), creating it if necessary.
pub(crate) fn cookies_dir() -> PathBuf {
    let dir = config_dir().join("cookies");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Check whether the current process is running as root / admin.
#[cfg(unix)]
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(windows)]
fn is_root() -> bool {
    is_elevated::is_elevated()
}

#[tokio::main]
async fn main() {
    let log_dir = logs_dir();
    logging::init(log_dir);
    print_version();

    let cli = Cli::parse();
    let command = cli.command.unwrap_or_default();

    match command {
        // `serve` does NOT require root — the privileged connect-daemon
        // child process is spawned on demand via osascript / sudo.
        Command::Serve { port, no_open } => cmd_serve(port, no_open).await,

        // CLI commands that directly perform VPN operations need root.
        Command::Connect { profile } => {
            check_privilege();
            cmd_connect(&profile).await;
        }
        Command::Legacy { config } => {
            check_privilege();
            cmd_legacy(&config).await;
        }

        // Internal: already running as root (spawned by serve via osascript/sudo).
        Command::ConnectDaemon {
            config,
            event_pipe,
            owner_uid,
            owner_gid,
        } => {
            if !is_root() {
                log::error!("connect-daemon must be run as root");
                exit(EPERM);
            }
            cmd_connect_daemon(&config, &event_pipe, owner_uid, owner_gid).await;
        }

        // Read-only commands — no privileges needed.
        Command::Status { port } => cmd_status(port).await,
        Command::Profiles => cmd_profiles(),
    }
}

// ---------------------------------------------------------------------------
// `corplink` / `corplink serve` — daemon + web UI (no root required)
// ---------------------------------------------------------------------------

async fn cmd_serve(port: u16, no_open: bool) {
    let dir = profiles_dir();
    // Ensure the cookies directory exists (normal user ownership).
    let _ = cookies_dir();
    let state = web::state::new_app_state(dir);

    if !no_open {
        let url = format!("http://localhost:{}", port);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Err(e) = open_browser(&url) {
                log::warn!("failed to open browser: {}", e);
            }
        });
    }

    log::info!("starting web UI on port {}", port);
    if let Err(e) = web::serve(state, port).await {
        log::error!("web server error: {}", e);
        exit(EPERM);
    }
}

// ---------------------------------------------------------------------------
// `corplink connect-daemon` — privileged VPN child process (internal)
//
// Spawned by the web server via osascript (macOS) or sudo (Linux).
// Communicates with the parent through a named pipe (JSON events).
// ---------------------------------------------------------------------------

async fn cmd_connect_daemon(config_path: &str, event_pipe_path: &str, owner_uid: u32, owner_gid: u32) {
    use std::io::Write;

    // Open the event pipe for writing.  The parent is already blocking on
    // the read side, so this open will succeed immediately.
    let mut pipe = match std::fs::OpenOptions::new()
        .write(true)
        .open(event_pipe_path)
    {
        Ok(f) => f,
        Err(e) => {
            log::error!("failed to open event pipe {}: {}", event_pipe_path, e);
            exit(EPERM);
        }
    };

    // Helper: write a JSON event line to the pipe.
    macro_rules! emit {
        ($val:expr) => {
            if let Ok(json) = serde_json::to_string(&$val) {
                let _ = writeln!(pipe, "{}", json);
                let _ = pipe.flush();
            }
        };
    }

    // Notify parent of our PID so it can send SIGTERM to disconnect.
    emit!(serde_json::json!({"event": "started", "pid": std::process::id()}));

    // ── VPN flow (same logic as run_legacy_flow, but headless) ──────────

    let mut conf = match Config::from_file(config_path).await {
        Ok(c) => c,
        Err(e) => {
            emit!(serde_json::json!({"event": "error", "message": e}));
            exit(EPERM);
        }
    };

    if conf.server.is_none() {
        match client::get_company_url(conf.company_name.as_str()).await {
            Ok(resp) => {
                log::info!(
                    "resolved company: {}(zh)/{}(en) server={}",
                    resp.zh_name,
                    resp.en_name,
                    resp.domain
                );
                conf.server = Some(resp.domain);
                let _ = conf.save().await;
                // Fix ownership: daemon runs as root but files belong to the user.
                if let Some(ref cf) = conf.conf_file {
                    chown_to_user(std::path::Path::new(cf), owner_uid, owner_gid);
                }
            }
            Err(e) => {
                emit!(serde_json::json!({"event": "error", "message": format!("failed to resolve server: {}", e)}));
                exit(EPERM);
            }
        }
    }

    // Use "utun" to let the kernel auto-assign the interface number.
    // A fixed number (e.g. utun953) causes "resource busy" if a previous
    // daemon was killed without proper cleanup.
    let tun_name = "utun";
    let with_wg_log = conf.debug_wg.unwrap_or_default();
    let use_full_route = conf.use_full_route.unwrap_or(false);
    #[cfg(target_os = "macos")]
    let use_vpn_dns = conf.use_vpn_dns.unwrap_or(false);

    let mut c = match Client::new_headless(conf) {
        Ok(c) => c,
        Err(e) => {
            emit!(serde_json::json!({"event": "error", "message": format!("client init: {}", e)}));
            exit(EPERM);
        }
    };

    // Login + connect (with one automatic retry on "logout" error)
    let mut logout_retry = true;
    let wg_conf: WgConf = loop {
        if c.need_login() {
            log::info!("logging in...");
            if let Err(e) = c.login().await {
                emit!(serde_json::json!({"event": "error", "message": format!("login failed: {}", e)}));
                exit(EPERM);
            }
        }
        match c.connect_vpn().await {
            Ok(conf) => break conf,
            Err(e) => {
                if logout_retry && e.to_string().contains("logout") {
                    log::warn!("{}", e);
                    logout_retry = false;
                    continue;
                }
                emit!(serde_json::json!({"event": "error", "message": format!("connect failed: {}", e)}));
                exit(EPERM);
            }
        }
    };

    // Fix ownership of files written as root back to the real user.
    #[cfg(unix)]
    {
        chown_to_user(std::path::Path::new(config_path), owner_uid, owner_gid);
        let cookie_path = c.cookie_file_path();
        if let Some(cookie_dir) = cookie_path.parent() {
            chown_to_user(cookie_dir, owner_uid, owner_gid);
        }
        chown_to_user(&cookie_path, owner_uid, owner_gid);
    }

    // Routes
    if let Some(peer_ip) = extract_peer_host(&wg_conf.peer_address) {
        if let Err(e) = c.ensure_peer_route(&peer_ip).await {
            log::warn!("failed to ensure peer route: {}", e);
        }
    }

    // WireGuard
    log::info!("starting wg-corplink (tun={}, protocol={})", tun_name, wg_conf.protocol);
    if !wg::start_wg_go(tun_name, wg_conf.protocol, with_wg_log) {
        emit!(serde_json::json!({"event": "error", "message": format!("failed to start wg — check ~/.config/corplink/logs/daemon-stderr.log for details")}));
        exit(EPERM);
    }

    let mut uapi = wg::UAPIClient {
        name: tun_name.to_string(),
    };
    if let Err(e) = uapi.config_wg(&wg_conf).await {
        wg::stop_wg_go();
        emit!(serde_json::json!({"event": "error", "message": format!("wg config failed: {}", e)}));
        exit(EPERM);
    }

    // DNS
    #[cfg(target_os = "macos")]
    let mut dns_manager = DNSManager::new();
    #[cfg(target_os = "macos")]
    if use_vpn_dns {
        let dns_domains: Vec<&str> = wg_conf.dns_domain_split.iter().map(|s| s.as_str()).collect();
        if let Err(e) = dns_manager.set_dns(vec![&wg_conf.dns], dns_domains) {
            log::warn!("failed to set dns: {}", e);
        }
    }

    // ── Connected — tell parent ─────────────────────────────────────────
    emit!(serde_json::json!({
        "event": "connected",
        "vpn_ip": wg_conf.address,
        "peer_address": wg_conf.peer_address,
        "server_name": wg_conf.server_name,
        "use_full_route": use_full_route,
    }));

    // ── Keep alive until signal or timeout ───────────────────────────────
    let disconnect_reason;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            disconnect_reason = "signal";
        }
        _ = async {
            #[cfg(unix)]
            {
                let mut sig = tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::terminate(),
                ).expect("failed to register SIGTERM handler");
                sig.recv().await;
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        } => {
            disconnect_reason = "signal";
        }
        _ = c.keep_alive_vpn(&wg_conf, 60) => {
            disconnect_reason = "keepalive_timeout";
        }
        _ = async {
            uapi.check_wg_connection().await;
        } => {
            disconnect_reason = "handshake_timeout";
        }
    }

    // ── Cleanup ─────────────────────────────────────────────────────────
    log::info!("disconnecting vpn (reason: {})...", disconnect_reason);
    let _ = c.disconnect_vpn(&wg_conf).await;
    wg::stop_wg_go();

    #[cfg(target_os = "macos")]
    if use_vpn_dns {
        let _ = dns_manager.restore_dns();
    }

    emit!(serde_json::json!({"event": "disconnected", "reason": disconnect_reason}));
}

// ---------------------------------------------------------------------------
// `corplink connect <profile>` — headless quick-connect (requires root)
// ---------------------------------------------------------------------------

async fn cmd_connect(profile: &str) {
    let dir = profiles_dir();
    let conf_path = dir.join(format!("{}.json", profile));

    if !conf_path.exists() {
        log::error!("profile '{}' not found at {}", profile, conf_path.display());
        exit(ENOENT);
    }

    let conf_path_str = conf_path.to_string_lossy().to_string();
    run_legacy_flow(&conf_path_str).await;
}

// ---------------------------------------------------------------------------
// `corplink status` — show connection status (reads daemon over HTTP)
// ---------------------------------------------------------------------------

async fn cmd_status(port: u16) {
    let url = format!("http://127.0.0.1:{}/api/status", port);
    match reqwest::get(&url).await {
        Ok(resp) => {
            if let Ok(body) = resp.text().await {
                println!("{}", body);
            } else {
                log::error!("failed to read status response");
                exit(EPERM);
            }
        }
        Err(_) => {
            println!(
                "corplink daemon is not running (cannot reach localhost:{})",
                port
            );
        }
    }
}

// ---------------------------------------------------------------------------
// `corplink profiles` — list available profiles
// ---------------------------------------------------------------------------

fn cmd_profiles() {
    let dir = profiles_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            log::error!("failed to read {}: {}", dir.display(), e);
            exit(EPERM);
        }
    };

    let mut found = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "json") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(conf) = serde_json::from_str::<Config>(&content) {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                    println!(
                        "  {} — {} ({}@{})",
                        stem,
                        conf.company_name,
                        conf.username,
                        conf.server.as_deref().unwrap_or("unresolved")
                    );
                    found = true;
                }
            }
        }
    }
    if !found {
        println!("no profiles found in {}", dir.display());
        println!("place JSON config files there to get started.");
    }
}

// ---------------------------------------------------------------------------
// `corplink legacy <config>` — backward-compatible single-config mode
// ---------------------------------------------------------------------------

async fn cmd_legacy(conf_file: &str) {
    run_legacy_flow(conf_file).await;
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Open a URL in the user's default browser.
/// Under sudo, `open` runs as root and may pick Safari instead of the user's
/// default browser.  We detect SUDO_USER and run `open` as that user.
fn open_browser(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(sudo_user) = std::env::var("SUDO_USER") {
            log::debug!(
                "running under sudo, opening browser as user '{}'",
                sudo_user
            );
            let status = std::process::Command::new("sudo")
                .args(["-u", &sudo_user, "open", url])
                .status()?;
            if !status.success() {
                return Err(format!("open exited with {}", status).into());
            }
            return Ok(());
        }
    }
    open::that(url)?;
    Ok(())
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
// Legacy flow — used by `connect` and `legacy` commands (runs in-process)
// ---------------------------------------------------------------------------

async fn run_legacy_flow(conf_file: &str) {
    let mut conf = match Config::from_file(conf_file).await {
        Ok(c) => c,
        Err(e) => {
            log::error!("{}", e);
            exit(EPERM);
        }
    };
    let name = conf.interface_name.clone().unwrap_or_else(|| {
        log::error!("interface_name not set in config");
        exit(EPERM);
    });

    #[cfg(target_os = "macos")]
    let use_vpn_dns = conf.use_vpn_dns.unwrap_or(false);

    match conf.server {
        Some(_) => {}
        None => match client::get_company_url(conf.company_name.as_str()).await {
            Ok(resp) => {
                log::info!(
                    "company name is {}(zh)/{}(en) server is {}",
                    resp.zh_name,
                    resp.en_name,
                    resp.domain
                );
                conf.server = Some(resp.domain);
                if let Err(e) = conf.save().await {
                    log::warn!("failed to save config: {}", e);
                }
            }
            Err(err) => {
                log::error!(
                    "failed to fetch company server from company name {}: {}",
                    conf.company_name,
                    err
                );
                exit(EPERM);
            }
        },
    }

    let with_wg_log = conf.debug_wg.unwrap_or_default();
    let mut c = match Client::new(conf) {
        Ok(c) => c,
        Err(e) => {
            log::error!("failed to initialize client: {}", e);
            exit(EPERM);
        }
    };
    let mut logout_retry = true;
    let wg_conf: Option<WgConf>;

    loop {
        if c.need_login() {
            log::info!("not login yet, try to login");
            if let Err(e) = c.login().await {
                log::error!("login failed: {}", e);
                exit(EPERM);
            }
            log::info!("login success");
        }
        log::info!("try to connect");
        match c.connect_vpn().await {
            Ok(conf) => {
                wg_conf = Some(conf);
                break;
            }
            Err(e) => {
                if logout_retry && e.to_string().contains("logout") {
                    log::warn!("{}", e);
                    logout_retry = false;
                    continue;
                } else {
                    log::error!("connect failed: {}", e);
                    exit(EPERM);
                }
            }
        };
    }
    let wg_conf = wg_conf.expect("unreachable: loop above guarantees wg_conf is Some");

    if let Some(peer_ip) = extract_peer_host(&wg_conf.peer_address) {
        if let Err(err) = c.ensure_peer_route(&peer_ip).await {
            log::warn!("failed to ensure route to peer {}: {}", peer_ip, err);
        }
    }

    log::info!("start wg-corplink for {}", &name);
    let protocol = wg_conf.protocol;
    if !wg::start_wg_go(&name, protocol, with_wg_log) {
        log::warn!("failed to start wg-corplink for {}", name);
        exit(EPERM);
    }
    let mut uapi = wg::UAPIClient { name: name.clone() };
    match uapi.config_wg(&wg_conf).await {
        Ok(_) => {}
        Err(err) => {
            log::error!(
                "failed to config interface with uapi for {}: {}",
                name,
                err
            );
            exit(EPERM);
        }
    }

    #[cfg(target_os = "macos")]
    let mut dns_manager = DNSManager::new();

    #[cfg(target_os = "macos")]
    if use_vpn_dns {
        let dns_domains: Vec<&str> = wg_conf.dns_domain_split.iter().map(|s| s.as_str()).collect();
        match dns_manager.set_dns(vec![&wg_conf.dns], dns_domains) {
            Ok(_) => {}
            Err(err) => {
                log::warn!("failed to set dns: {}", err);
            }
        }
    }

    let mut exit_code = 0;
    tokio::select! {
        _ = async {
            match tokio::signal::ctrl_c().await {
                Ok(_) => {},
                Err(e) => {
                    log::warn!("failed to receive signal: {}", e);
                },
            }
            log::info!("ctrl+c received");
        } => {},

        _ = c.keep_alive_vpn(&wg_conf, 60) => {
            exit_code = ETIMEDOUT;
        },

        _ = async {
            uapi.check_wg_connection().await;
            log::warn!("last handshake timeout");
        } => {
            exit_code = ETIMEDOUT;
        },
    }

    // shutdown
    log::info!("disconnecting vpn...");
    match c.disconnect_vpn(&wg_conf).await {
        Ok(_) => {}
        Err(e) => log::warn!("failed to disconnect vpn: {}", e),
    };

    wg::stop_wg_go();

    #[cfg(target_os = "macos")]
    if use_vpn_dns {
        match dns_manager.restore_dns() {
            Ok(_) => {}
            Err(err) => {
                log::warn!("failed to delete dns: {}", err);
            }
        }
    }

    log::info!("reach exit");
    exit(exit_code)
}

fn check_privilege() {
    #[cfg(unix)]
    match sudo::escalate_if_needed() {
        Ok(_) => {}
        Err(_) => {
            log::error!("please run as root");
            exit(EPERM);
        }
    }

    #[cfg(windows)]
    if !is_elevated::is_elevated() {
        log::error!("please run as administrator");
        exit(EPERM);
    }
}

fn print_version() {
    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    log::info!("running {}@{}", pkg_name, pkg_version);
}
