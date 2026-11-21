use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about, arg_required_else_help = true)]
pub struct Cli {
    #[arg(long, global = true, default_value_t = 15)]
    pub timeout: u64,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    Switch {
        profile: String,
    },
    Save {
        profile: String,
        /// Omit the currently focused window from the snapshot.
        #[arg(long)]
        ignore_focused_window: bool,
        /// Set the unmatched-window policy in the saved profile.
        #[arg(long, value_enum)]
        unmatched: Option<crate::config::Policy>,
        /// Replace an existing profile with a fresh snapshot.
        #[arg(long)]
        overwrite: bool,
    },
    List,
    /// Select a profile using Fuzzel.
    Pick,
    Status,
    Daemon,
    #[command(external_subcommand)]
    Profile(Vec<String>),
}
