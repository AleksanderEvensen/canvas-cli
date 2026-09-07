use clap::Parser;

mod commands;
mod utilities;

pub(crate) static READ_ONLY_ACTIONS: bool = !cfg!(feature = "write-requests");

#[derive(Parser)]
#[command(
  name = "cnvs",
  about = "Use a signed-in Chromium session from the command line",
  long_about = "Use a signed-in Chromium session from the command line. Common Canvas resources have grouped commands; use `cnvs agent skills` for guidance intended for AI agents."
)]
struct Cli {
  #[arg(
    short,
    long,
    global = true,
    help = "Print HTTP status and daemon startup details to stderr"
  )]
  verbose: bool,

  #[command(subcommand)]
  command: commands::Command,
}

#[tokio::main]
async fn main() {
  match run().await {
    Ok(code) => std::process::exit(code),
    Err(error) => {
      eprintln!("{error:#}");
      std::process::exit(1);
    }
  }
}

async fn run() -> anyhow::Result<i32> {
  let cli = Cli::parse();
  commands::run(cli.command, cli.verbose).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use clap::CommandFactory;

  #[test]
  fn command_definitions_are_consistent() {
    Cli::command().debug_assert();
    assert!(Cli::try_parse_from(["cnvs", "assignments", "list", "--past", "--json"]).is_ok());
    assert_eq!(
      Cli::try_parse_from(["cnvs", "gql", "--url", "https://canvas.example/api/graphql"]).is_ok(),
      cfg!(feature = "write-requests")
    );
  }
}
