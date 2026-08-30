use anyhow::Result;
use clap::Subcommand;

mod agent;
mod api;
mod config;
mod daemon;

#[cfg(feature = "write-requests")]
mod gql;

#[derive(Subcommand)]
pub enum Command {
  Api(api::ApiArgs),
  Agent {
    #[command(subcommand)]
    command: agent::AgentCommand,
  },
  #[cfg(feature = "write-requests")]
  Gql(gql::GqlArgs),
  Daemon {
    #[command(subcommand)]
    command: daemon::DaemonCommand,
  },
  Config {
    #[command(subcommand)]
    command: config::ConfigCommand,
  },
}

pub(crate) fn run(command: Command, verbose: bool) -> Result<i32> {
  match command {
    Command::Api(args) => api::run(args, verbose),
    Command::Agent { command } => agent::run(command),
    #[cfg(feature = "write-requests")]
    Command::Gql(args) => gql::run(args, verbose),
    Command::Daemon { command } => daemon::run(command),
    Command::Config { command } => config::run(command),
  }
}
