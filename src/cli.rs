use clap::{Parser, Subcommand};

/// CorpLink VPN Client — Connect to your company's VPN
#[derive(Parser)]
#[command(name = "corplink", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

pub const DEFAULT_PORT: u16 = 4027;

#[derive(Subcommand)]
pub enum Command {
    /// Start the daemon in the background and open the web UI (default)
    Start {
        /// Port for the web UI
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Don't open the browser automatically
        #[arg(long)]
        no_open: bool,
    },

    /// Stop the background daemon
    Stop,

    /// Restart the background daemon
    Restart {
        /// Port for the web UI
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Don't open the browser automatically
        #[arg(long)]
        no_open: bool,
    },

    /// Run the web server in the foreground (for debugging)
    Serve {
        /// Port for the web UI
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Don't open the browser automatically
        #[arg(long)]
        no_open: bool,
    },

    /// Quick-connect to a VPN profile from the command line
    Connect {
        /// Profile name to connect (filename without .json)
        profile: String,
    },

    /// Show current VPN connection status
    Status {
        /// Port of the running daemon
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },

    /// List available profiles
    Profiles,

    /// Run in legacy mode with a config file (backward compatible)
    Legacy {
        /// Path to config JSON file
        config: String,
    },

    /// Check for updates and self-update from GitHub releases
    Update {
        /// Only check, don't actually update
        #[arg(long)]
        check: bool,
    },

    /// (Internal) Privileged VPN daemon — spawned by `serve` via sudo
    #[command(name = "connect-daemon", hide = true)]
    ConnectDaemon {
        /// Path to the profile config JSON file
        #[arg(long)]
        config: String,
        /// Path to the named pipe for sending status events to the parent
        #[arg(long)]
        event_pipe: String,
        /// UID of the unprivileged user that owns the config files
        #[arg(long)]
        owner_uid: u32,
        /// GID of the unprivileged user that owns the config files
        #[arg(long)]
        owner_gid: u32,
    },
}

impl Default for Command {
    fn default() -> Self {
        Command::Start {
            port: DEFAULT_PORT,
            no_open: false,
        }
    }
}
