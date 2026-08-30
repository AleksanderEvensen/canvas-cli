use std::{env, fs, process::Command};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use cnvs_config::Config;

#[derive(Subcommand)]
pub enum ConfigCommand {
  Info,
  Edit,
}

pub(crate) fn run(command: ConfigCommand) -> Result<i32> {
  match command {
    ConfigCommand::Info => info(),
    ConfigCommand::Edit => edit(),
  }
}

fn info() -> Result<i32> {
  let Some(path) = cnvs_config::config_path() else {
    println!("Config file: unavailable (no user config directory)");
    println!(
      "\nCurrent configuration:\n{}",
      Config::default().serialize()?
    );
    return Ok(0);
  };

  println!("Config file: {}", path.display());
  println!("Exists: {}", path.is_file());
  println!("\nCurrent configuration:");
  println!("{}", Config::load()?.serialize()?);
  Ok(0)
}

fn edit() -> Result<i32> {
  let editor = env::var("EDITOR").context("$EDITOR is not set")?;
  if editor.trim().is_empty() {
    bail!("$EDITOR is empty");
  }

  let path = cnvs_config::config_path().context("could not determine the configuration path")?;
  if !path.exists() {
    let directory = path
      .parent()
      .context("configuration path has no parent directory")?;
    fs::create_dir_all(directory)
      .with_context(|| format!("could not create {}", directory.display()))?;
    fs::write(&path, Config::default().serialize()? + "\n")
      .with_context(|| format!("could not create {}", path.display()))?;
  }

  let status = Command::new("sh")
    .args(["-c", "exec $EDITOR \"$1\"", "cnvs-editor"])
    .arg(&path)
    .status()
    .with_context(|| format!("could not start editor {editor}"))?;
  if !status.success() {
    bail!("editor exited with {status}");
  }
  Ok(0)
}
