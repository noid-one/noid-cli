use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "noid",
    about = "noid — manage remote Firecracker microVMs",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Configure server connection
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Set the active VM for this directory
    Use {
        /// VM name
        name: String,
    },
    /// Show the current active VM and server
    Current,
    /// Show authenticated user info
    Whoami,
    /// Create a new microVM
    Create {
        /// VM name
        name: String,
        /// Number of vCPUs
        #[arg(long, default_value = "1")]
        cpus: u32,
        /// Memory in MiB
        #[arg(long, default_value = "128")]
        mem: u32,
    },
    /// Destroy a microVM
    Destroy {
        /// VM name (optional if .noid file exists)
        name: Option<String>,
    },
    /// List all microVMs
    List,
    /// Show info about a microVM
    Info {
        /// VM name (optional if .noid file exists)
        name: Option<String>,
    },
    /// Execute a command in a microVM
    Exec {
        /// VM name (optional if .noid file exists)
        #[arg(long)]
        name: Option<String>,
        /// Command to run
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Attach to VM serial console
    Console {
        /// VM name (optional if .noid file exists)
        name: Option<String>,
    },
    /// Create a checkpoint of a microVM
    Checkpoint {
        /// VM name (optional if .noid file exists)
        #[arg(long)]
        name: Option<String>,
        /// Optional label
        #[arg(long)]
        label: Option<String>,
    },
    /// List checkpoints for a microVM
    Checkpoints {
        /// VM name (optional if .noid file exists)
        name: Option<String>,
    },
    /// Update noid to the latest release
    Update,
    /// Restore a microVM from a checkpoint
    Restore {
        /// VM name (optional if .noid file exists)
        #[arg(long)]
        name: Option<String>,
        /// Checkpoint ID
        checkpoint_id: String,
        /// Create as a new VM with this name
        #[arg(long = "as")]
        new_name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AuthAction {
    /// Set up server connection
    Setup {
        /// Server URL
        #[arg(long)]
        url: String,
        /// Authentication token
        #[arg(long)]
        token: String,
    },
}
