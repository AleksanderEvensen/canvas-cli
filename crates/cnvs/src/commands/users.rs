use anyhow::Result;
use clap::Subcommand;

use super::api;

#[derive(Subcommand)]
pub enum UsersCommand {
  #[command(about = "Fetch the currently signed-in Canvas user")]
  Me,
}

pub(crate) fn run(command: UsersCommand, verbose: bool) -> Result<i32> {
  match command {
    UsersCommand::Me => api::get("/api/v1/users/self", verbose),
  }
}
