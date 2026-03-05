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

// ---------------------------------------------------------------------------
// PID file management — tracks the background daemon process
//
// Uses `flock(2)` as the authoritative "is daemon alive?" check, which is
// immune to PID reuse after crashes.  The daemon (`cmd_serve`) holds an
// exclusive lock on the PID file for its entire lifetime.
// ---------------------------------------------------------------------------

/// Path to the PID file: `~/.config/corplink/daemon.pid`.
fn pid_file_path() -> PathBuf {
    config_dir().join("daemon.pid")
}

/// Read the PID recorded in the PID file, rejecting obviously invalid values.
fn read_daemon_pid() -> Option<u32> {
    std::fs::read_to_string(pid_file_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&pid| pid > 1) // 0 ⇒ kill whole pgrp, 1 ⇒ init
}

/// Write PID to the file with restricted permissions.
#[cfg(unix)]
fn write_daemon_pid(pid: u32) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let result = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(pid_file_path())
        .and_then(|mut f| write!(f, "{}", pid));
    if let Err(e) = result {
        log::warn!("failed to write PID file: {}", e);
    }
}

/// Remove the PID file only if it still contains `expected_pid`.
/// Prevents a concurrent `start` from accidentally deleting a valid PID file
/// written by another instance.
fn remove_pid_file_for(expected_pid: u32) {
    if read_daemon_pid() == Some(expected_pid) {
        let _ = std::fs::remove_file(pid_file_path());
    }
}

/// Remove the PID file unconditionally (for use after the daemon is confirmed
/// dead, e.g. in `cmd_stop`).
fn remove_pid_file() {
    let _ = std::fs::remove_file(pid_file_path());
}

/// Try to acquire an exclusive advisory lock on the PID file (non-blocking).
///
/// Returns the `File` handle on success — the caller **must** keep it alive
/// for the entire daemon lifetime; dropping it releases the lock.
/// Returns `None` if the lock is already held by another process.
#[cfg(unix)]
fn try_acquire_daemon_lock() -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(pid_file_path())
        .ok()?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Some(file)
    } else {
        None
    }
}

/// Check whether the PID file is locked by a running daemon.
#[cfg(unix)]
fn is_daemon_locked() -> bool {
    use std::os::unix::io::AsRawFd;

    let file = match std::fs::File::open(pid_file_path()) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        // We got the lock → no daemon is holding it.  Release immediately.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
        false
    } else {
        true
    }
}

/// Check whether a process with the given PID is still alive.
#[cfg(unix)]
fn is_process_running(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Return the PID of the running daemon, or `None` if not running.
///
/// On Unix, uses `flock` as the authoritative check (immune to PID reuse).
fn find_running_daemon() -> Option<u32> {
    if is_daemon_locked() {
        read_daemon_pid()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------

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
        // Daemon lifecycle commands — no root required.
        Command::Start { port, no_open } => cmd_start(port, no_open),
        Command::Stop => cmd_stop(),
        Command::Restart { port, no_open } => cmd_restart(port, no_open),

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
        Command::Update { check } => cmd_update(check).await,
    }
}

// ---------------------------------------------------------------------------
// `corplink start` — launch the daemon in the background and exit
// ---------------------------------------------------------------------------

fn cmd_start(port: u16, no_open: bool) {
    // Already running?
    if let Some(pid) = find_running_daemon() {
        println!("daemon already running (pid {})", pid);
        if !no_open {
            let url = format!("http://localhost:{}", port);
            let _ = open_browser(&url);
        }
        return;
    }

    // Clean up any stale PID file (lock not held ⇒ safe to remove).
    remove_pid_file();

    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("cannot find self: {}", e);
        exit(EPERM);
    });

    let log_path = logs_dir().join("daemon-stdout.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap_or_else(|e| {
            eprintln!("cannot open log file {}: {}", log_path.display(), e);
            exit(EPERM);
        });
    let log_err = log_file
        .try_clone()
        .expect("failed to clone log file handle");

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["serve", "--port", &port.to_string(), "--no-open"]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(log_file);
    cmd.stderr(log_err);

    // Create a new session so the daemon survives terminal close.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let child = cmd.spawn().unwrap_or_else(|e| {
        eprintln!("failed to start daemon: {}", e);
        exit(EPERM);
    });
    let pid = child.id();

    // Wait for the daemon to acquire its flock (meaning the port is bound
    // and the server is ready), or detect early exit.
    let mut started = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if is_daemon_locked() {
            started = true;
            break;
        }
        if !is_process_running(pid) {
            break;
        }
    }

    if started && is_process_running(pid) {
        println!(
            "daemon started (pid {}), listening on http://127.0.0.1:{}",
            pid, port
        );
        if !no_open {
            let url = format!("http://localhost:{}", port);
            let _ = open_browser(&url);
        }
    } else {
        // Only remove PID file if it belongs to the child we spawned.
        remove_pid_file_for(pid);
        eprintln!(
            "daemon failed to start — check logs at {}",
            log_path.display()
        );
        exit(EPERM);
    }
}

// ---------------------------------------------------------------------------
// `corplink stop` — send SIGTERM and wait for the daemon to exit
// ---------------------------------------------------------------------------

fn cmd_stop() {
    if !is_daemon_locked() {
        // No daemon holds the lock — clean up any stale PID file.
        remove_pid_file();
        println!("daemon is not running");
        return;
    }

    let pid = match read_daemon_pid() {
        Some(pid) => pid,
        None => {
            eprintln!("daemon appears to be running (lock held) but PID is unknown");
            exit(EPERM);
        }
    };

    // Send SIGTERM for graceful shutdown (triggers axum graceful shutdown
    // which in turn cleans up VPN daemon via the sentinel file).
    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                println!("daemon (pid {}) already exited", pid);
                remove_pid_file();
                return;
            }
            eprintln!("failed to send SIGTERM to pid {}: {}", pid, err);
            exit(EPERM);
        }
    }

    // Wait up to 8 seconds for the process to exit.
    for _ in 0..32 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !is_process_running(pid) {
            remove_pid_file();
            println!("daemon stopped (pid {})", pid);
            return;
        }
    }

    // Force-kill if still alive.
    eprintln!(
        "daemon (pid {}) did not exit gracefully, sending SIGKILL",
        pid
    );
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }

    // Wait for SIGKILL to take effect.
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !is_process_running(pid) {
            break;
        }
    }

    remove_pid_file();
    if is_process_running(pid) {
        eprintln!("failed to kill daemon (pid {})", pid);
        exit(EPERM);
    }
    println!("daemon killed (pid {})", pid);
}

// ---------------------------------------------------------------------------
// `corplink restart` — stop then start
// ---------------------------------------------------------------------------

fn cmd_restart(port: u16, no_open: bool) {
    if find_running_daemon().is_some() {
        cmd_stop();
        // Brief pause so the TCP port is released by the kernel.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    cmd_start(port, no_open);
}

// ---------------------------------------------------------------------------
// `corplink` / `corplink serve` — daemon + web UI (no root required)
// ---------------------------------------------------------------------------

async fn cmd_serve(port: u16, no_open: bool) {
    let dir = profiles_dir();
    // Ensure the cookies directory exists (normal user ownership).
    let _ = cookies_dir();
    let state = web::state::new_app_state(dir);

    // Bind the port first — fail early if something else is using it.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("failed to bind port {}: {}", port, e);
            exit(EPERM);
        }
    };

    // Acquire exclusive flock — prevents duplicate instances and serves as
    // the authoritative "is daemon alive?" indicator for other commands.
    let _lock_guard = match try_acquire_daemon_lock() {
        Some(f) => f,
        None => {
            log::error!("another daemon instance is already running");
            exit(EPERM);
        }
    };
    let my_pid = std::process::id();
    write_daemon_pid(my_pid);

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

    let state_for_shutdown = state.clone();
    if let Err(e) = web::serve(state, port, listener, state_for_shutdown).await {
        log::error!("web server error: {}", e);
        remove_pid_file_for(my_pid);
        exit(EPERM);
    }

    remove_pid_file_for(my_pid);
    // _lock_guard is dropped here, releasing the flock
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
                #[cfg(unix)]
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

    // ── Keep alive until signal, timeout, or shutdown request ────────────
    //
    // The parent process (corplink serve) cannot send Unix signals to us
    // because we run as root and it runs as a normal user.  Instead, it
    // creates a "shutdown" file in the temp directory to request disconnect.
    // We poll for that file every 500 ms.
    let shutdown_file = std::path::Path::new(event_pipe_path)
        .parent()
        .map(|d| d.join("shutdown"));

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
        _ = async {
            if let Some(ref path) = shutdown_file {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if path.exists() {
                        log::info!("shutdown file detected: {}", path.display());
                        break;
                    }
                }
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            disconnect_reason = "shutdown_requested";
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
    // Order matters: stop WireGuard first so routes are removed and the
    // user's network is restored immediately.  DNS and API notification
    // happen afterwards over the normal (non-VPN) network.
    log::info!("disconnecting vpn (reason: {})...", disconnect_reason);

    // 1. Stop WireGuard — destroys TUN interface and removes all VPN routes.
    wg::stop_wg_go();
    log::info!("wireguard stopped, TUN interface destroyed");

    // 2. Restore DNS.
    #[cfg(target_os = "macos")]
    if use_vpn_dns {
        if let Err(e) = dns_manager.restore_dns() {
            log::warn!("failed to restore dns: {}", e);
        }
    }

    // 3. Remove the peer host route added by ensure_peer_route().
    #[cfg(target_os = "macos")]
    if let Some(peer_ip) = extract_peer_host(&wg_conf.peer_address) {
        let _ = std::process::Command::new("route")
            .args(["-n", "delete", "-host", &peer_ip])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        log::debug!("removed peer host route for {}", peer_ip);
    }

    // 4. Notify server (best effort, with timeout — network is normal now).
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        c.disconnect_vpn(&wg_conf),
    ).await {
        Ok(Ok(())) => log::info!("server notified of disconnect"),
        Ok(Err(e)) => log::warn!("disconnect_vpn API failed: {}", e),
        Err(_) => log::warn!("disconnect_vpn API timed out after 3s"),
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
    match find_running_daemon() {
        Some(pid) => {
            println!("daemon is running (pid {})", pid);
            let url = format!("http://127.0.0.1:{}/api/status", port);
            match reqwest::get(&url).await {
                Ok(resp) => {
                    if let Ok(body) = resp.text().await {
                        println!("{}", body);
                    }
                }
                Err(e) => {
                    println!("cannot reach web API: {}", e);
                }
            }
        }
        None => {
            println!("daemon is not running");
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

    // shutdown — same order as daemon: stop WireGuard first to restore network.
    log::info!("disconnecting vpn...");

    // 1. Stop WireGuard — destroys TUN, removes VPN routes.
    wg::stop_wg_go();

    // 2. Restore DNS.
    #[cfg(target_os = "macos")]
    if use_vpn_dns {
        match dns_manager.restore_dns() {
            Ok(_) => {}
            Err(err) => {
                log::warn!("failed to delete dns: {}", err);
            }
        }
    }

    // 3. Remove peer host route.
    #[cfg(target_os = "macos")]
    if let Some(peer_ip) = extract_peer_host(&wg_conf.peer_address) {
        let _ = std::process::Command::new("route")
            .args(["-n", "delete", "-host", &peer_ip])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    // 4. Notify server (best effort).
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        c.disconnect_vpn(&wg_conf),
    ).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log::warn!("failed to disconnect vpn: {}", e),
        Err(_) => log::warn!("disconnect_vpn timed out"),
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
    let pkg_version = env!("BUILD_VERSION");
    log::info!("running {}@{}", pkg_name, pkg_version);
}

// ---------------------------------------------------------------------------
// `corplink update` — self-update from GitHub releases
// ---------------------------------------------------------------------------

const GITHUB_REPO: &str = "cyhhao/corplink-rs";

/// Normalise a version tag: strip leading `v` and trailing `.0` patch if
/// the release uses MAJOR.MINOR while Cargo uses MAJOR.MINOR.PATCH.
fn normalize_version(v: &str) -> String {
    v.trim().trim_start_matches('v').to_string()
}

/// Return the expected asset filename suffix for the current platform.
fn platform_asset_suffix() -> Option<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return None;
    };

    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };

    Some((os, arch))
}

async fn cmd_update(check_only: bool) {
    let current_version = env!("BUILD_VERSION");
    println!("current version: {}", current_version);

    // 1. Fetch latest release from GitHub API.
    let api_url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_REPO
    );
    let http = reqwest::Client::builder()
        .user_agent("corplink-updater")
        .build()
        .expect("failed to build HTTP client");

    let resp = match http.get(&api_url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to query GitHub releases: {}", e);
            exit(EPERM);
        }
    };

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("no releases found on {}", GITHUB_REPO);
        return;
    }
    if !resp.status().is_success() {
        eprintln!("GitHub API returned {}", resp.status());
        exit(EPERM);
    }

    let release: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("failed to parse release JSON: {}", e);
            exit(EPERM);
        }
    };

    let tag = release["tag_name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let latest = normalize_version(&tag);
    let current = normalize_version(current_version);

    println!("latest release:  {} (tag: {})", latest, tag);

    // 2. Compare versions.
    if latest == current {
        println!("already up to date.");
        return;
    }

    if check_only {
        println!("update available: {} -> {}", current, latest);
        return;
    }

    println!("updating {} -> {} ...", current, latest);

    // 3. Find the matching asset for this platform.
    let (os, arch) = match platform_asset_suffix() {
        Some(pair) => pair,
        None => {
            eprintln!("unsupported platform");
            exit(EPERM);
        }
    };

    let assets = match release["assets"].as_array() {
        Some(a) => a,
        None => {
            eprintln!("no assets in release");
            exit(EPERM);
        }
    };

    let asset_pattern = format!("{}-{}", os, arch);
    let asset = assets.iter().find(|a| {
        a["name"]
            .as_str()
            .map_or(false, |n| n.contains(&asset_pattern))
    });

    let asset = match asset {
        Some(a) => a,
        None => {
            eprintln!(
                "no asset found for {}-{} in release {}",
                os, arch, tag
            );
            eprintln!("available assets:");
            for a in assets {
                if let Some(name) = a["name"].as_str() {
                    eprintln!("  {}", name);
                }
            }
            exit(EPERM);
        }
    };

    let download_url = asset["browser_download_url"]
        .as_str()
        .expect("asset has no download URL");
    let asset_name = asset["name"].as_str().unwrap_or("update");

    println!("downloading {} ...", asset_name);

    // 4. Download the asset.
    let resp = match http.get(download_url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("download failed: {}", e);
            exit(EPERM);
        }
    };
    if !resp.status().is_success() {
        eprintln!("download returned {}", resp.status());
        exit(EPERM);
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read download body: {}", e);
            exit(EPERM);
        }
    };

    let tmp_dir = std::env::temp_dir().join("corplink-update");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("failed to create temp dir");

    let archive_path = tmp_dir.join(asset_name);
    std::fs::write(&archive_path, &bytes).expect("failed to write downloaded archive");

    // 5. Extract the archive.
    println!("extracting ...");
    let extract_ok = if asset_name.ends_with(".tar.gz") || asset_name.ends_with(".tgz") {
        std::process::Command::new("tar")
            .args(["-xzf", &archive_path.to_string_lossy()])
            .current_dir(&tmp_dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else if asset_name.ends_with(".zip") {
        std::process::Command::new("unzip")
            .args(["-o", &archive_path.to_string_lossy()])
            .current_dir(&tmp_dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        eprintln!("unknown archive format: {}", asset_name);
        exit(EPERM);
    };

    if !extract_ok {
        eprintln!("failed to extract archive");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        exit(EPERM);
    }

    // 6. Locate the new binary (could be named `corplink` or legacy `corplink-rs`).
    let new_binary = ["corplink", "corplink-rs"]
        .iter()
        .map(|name| tmp_dir.join(name))
        .find(|p| p.exists());

    let new_binary = match new_binary {
        Some(p) => p,
        None => {
            eprintln!("binary not found in extracted archive");
            let _ = std::fs::remove_dir_all(&tmp_dir);
            exit(EPERM);
        }
    };

    // 7. Replace the current executable.
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot determine current executable path: {}", e);
            let _ = std::fs::remove_dir_all(&tmp_dir);
            exit(EPERM);
        }
    };

    // Set executable permission on the new binary.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&new_binary, std::fs::Permissions::from_mode(0o755));
    }

    // On Unix, we can atomically replace a running binary via rename.
    // If cross-device, fall back to copy.
    println!("installing to {} ...", current_exe.display());
    if std::fs::rename(&new_binary, &current_exe).is_err() {
        if let Err(e) = std::fs::copy(&new_binary, &current_exe) {
            eprintln!("failed to install new binary: {}", e);
            eprintln!("you may need to run with sudo or copy manually:");
            eprintln!("  sudo cp {} {}", new_binary.display(), current_exe.display());
            let _ = std::fs::remove_dir_all(&tmp_dir);
            exit(EPERM);
        }
    }

    // 8. Cleanup.
    let _ = std::fs::remove_dir_all(&tmp_dir);

    println!("updated to {} successfully!", latest);
}
