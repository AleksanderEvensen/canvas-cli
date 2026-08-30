use anyhow::Result;
use clap::Subcommand;

pub(crate) mod agent;
pub(crate) mod api;
pub(crate) mod config;
pub(crate) mod courses;
pub(crate) mod daemon;
pub(crate) mod users;

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

  Courses {
    #[command(subcommand)]
    command: courses::CoursesCommand,
  },

  Users {
    #[command(subcommand)]
    command: users::UsersCommand,
  },
}

pub(crate) fn run(command: Command, verbose: bool) -> Result<i32> {
  match command {
    Command::Api(args) => api::run(args, verbose),
    Command::Agent { command } => agent::run(command),
    Command::Daemon { command } => daemon::run(command),
    Command::Config { command } => config::run(command),
    Command::Courses { command } => courses::run(command, verbose),
    Command::Users { command } => users::run(command, verbose),

    #[cfg(feature = "write-requests")]
    Command::Gql(args) => gql::run(args, verbose),
  }
}
