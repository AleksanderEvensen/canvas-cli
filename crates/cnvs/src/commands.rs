use anyhow::Result;
use clap::Subcommand;

pub mod agent;
pub mod api;
pub mod assignments;
pub mod config;
pub mod courses;
pub mod daemon;
pub mod users;

#[cfg(feature = "write-requests")]
mod gql;

#[derive(Subcommand)]
pub enum Command {
  #[command(about = "Make a raw Canvas REST API request")]
  Api(api::ApiArgs),

  #[command(about = "Discover skills and instructions for AI agents; run `cnvs agent skills`")]
  Agent {
    #[command(subcommand)]
    command: agent::AgentCommand,
  },

  #[cfg(feature = "write-requests")]
  #[command(about = "Execute a GraphQL query or mutation (write-enabled builds)")]
  Gql(gql::GqlArgs),

  #[command(about = "Start, inspect, or stop the browser connection daemon")]
  Daemon {
    #[command(subcommand)]
    command: daemon::DaemonCommand,
  },

  #[command(about = "Inspect or edit cnvs configuration")]
  Config {
    #[command(subcommand)]
    command: config::ConfigCommand,
  },

  #[command(about = "List or inspect Canvas courses")]
  Courses {
    #[command(subcommand)]
    command: courses::CoursesCommand,
  },

  #[command(about = "List upcoming assignments, submissions, and grades")]
  Assignments {
    #[command(subcommand)]
    command: assignments::AssignmentsCommand,
  },

  #[command(about = "Inspect the signed-in Canvas user")]
  Users {
    #[command(subcommand)]
    command: users::UsersCommand,
  },
}

pub async fn run(command: Command, verbose: bool) -> Result<i32> {
  match command {
    Command::Api(args) => api::run(args, verbose).await,
    Command::Agent { command } => agent::run(command),
    Command::Daemon { command } => daemon::run(command).await,
    Command::Config { command } => config::run(command),
    Command::Courses { command } => courses::run(command, verbose).await,
    Command::Assignments { command } => assignments::run(command, verbose).await,
    Command::Users { command } => users::run(command, verbose).await,

    #[cfg(feature = "write-requests")]
    Command::Gql(args) => gql::run(args, verbose).await,
  }
}
