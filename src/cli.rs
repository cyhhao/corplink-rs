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
    /// Start the daemon and open the web management UI (default)
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
        Command::Serve {
            port: DEFAULT_PORT,
            no_open: false,
        }
    }
}
