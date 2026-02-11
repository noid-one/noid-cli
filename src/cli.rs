use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "noid", about = "Firecracker microVM manager", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a new microVM
    Create {
        /// VM name
        name: String,
        /// Path to kernel image
        #[arg(long)]
        kernel: Option<String>,
        /// Path to rootfs image
        #[arg(long)]
        rootfs: Option<String>,
        /// Number of vCPUs
        #[arg(long, default_value = "1")]
        cpus: u32,
        /// Memory in MiB
        #[arg(long, default_value = "128")]
        mem: u32,
    },
    /// Destroy a microVM
    Destroy {
        /// VM name
        name: String,
    },
    /// List all microVMs
    List,
    /// Execute a command in a microVM
    Exec {
        /// VM name
        name: String,
        /// Command to run
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Attach to VM serial console
    Console {
        /// VM name
        name: String,
    },
    /// Create a checkpoint of a microVM
    Checkpoint {
        /// VM name
        name: String,
        /// Optional label for the checkpoint
        #[arg(long)]
        label: Option<String>,
    },
    /// List checkpoints for a microVM
    Checkpoints {
        /// VM name
        name: String,
    },
    /// Restore a microVM from a checkpoint
    Restore {
        /// VM name
        name: String,
        /// Checkpoint ID
        checkpoint_id: String,
        /// Create as a new VM with this name
        #[arg(long = "as")]
        new_name: Option<String>,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Set a configuration value
    Set {
        /// Config key (kernel, rootfs)
        key: String,
        /// Config value
        value: String,
    },
}
