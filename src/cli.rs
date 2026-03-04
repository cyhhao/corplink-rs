use clap::{Parser, Subcommand};

/// CorpLink VPN Client — Connect to your company's VPN
#[derive(Parser)]
#[command(name = "corplink", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start the daemon and open the web management UI (default)
    Serve {
        /// Port for the web UI
        #[arg(short, long, default_value_t = 4027)]
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
    Status,

    /// List available profiles
    Profiles,

    /// Run in legacy mode with a config file (backward compatible)
    Legacy {
        /// Path to config JSON file
        config: String,
    },
}

impl Default for Command {
    fn default() -> Self {
        Command::Serve {
            port: 4027,
            no_open: false,
        }
    }
}
