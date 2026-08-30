use clap::Parser;

mod commands;
mod utilities;

#[derive(Parser)]
#[command(
  name = "cnvs",
  about = "Use a signed-in Chromium session from the command line"
)]
struct Cli {
  #[arg(short, long, global = true)]
  verbose: bool,

  #[command(subcommand)]
  command: commands::Command,
}

fn main() {
  match run() {
    Ok(code) => std::process::exit(code),
    Err(error) => {
      eprintln!("{error:#}");
      std::process::exit(1);
    }
  }
}

fn run() -> anyhow::Result<i32> {
  let cli = Cli::parse();
  commands::run(cli.command, cli.verbose)
}
