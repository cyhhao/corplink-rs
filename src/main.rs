mod api;
mod cli;
mod client;
mod config;
mod dns;
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

/// Return the profiles directory, creating it if necessary.
fn profiles_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("corplink")
        .join("profiles");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| {
            log::error!("failed to create profiles directory {}: {}", dir.display(), e);
            exit(EPERM);
        });
    }
    dir
}

#[tokio::main]
async fn main() {
    env_logger::init();
    print_version();
    check_privilege();

    let cli = Cli::parse();
    let command = cli.command.unwrap_or_default();

    match command {
        Command::Serve { port, no_open } => cmd_serve(port, no_open).await,
        Command::Connect { profile } => cmd_connect(&profile).await,
        Command::Status { port } => cmd_status(port).await,
        Command::Profiles => cmd_profiles(),
        Command::Legacy { config } => cmd_legacy(&config).await,
    }
}

// ---------------------------------------------------------------------------
// `corplink` / `corplink serve` — daemon + web UI
// ---------------------------------------------------------------------------

async fn cmd_serve(port: u16, no_open: bool) {
    let dir = profiles_dir();
    let state = web::state::new_app_state(dir);

    if !no_open {
        let url = format!("http://localhost:{}", port);
        // Small delay so the server has time to bind before the browser hits it.
        let url_clone = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Err(e) = open::that(&url_clone) {
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
// `corplink connect <profile>` — headless quick-connect
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
            println!("corplink daemon is not running (cannot reach localhost:{})", port);
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
// Shared: the original main() VPN flow, used by `connect` and `legacy`
// ---------------------------------------------------------------------------

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
            log::error!("failed to config interface with uapi for {}: {}", name, err);
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
