use crate::READ_ONLY_ACTIONS;
use anyhow::Context;
use anyhow::{bail, Result};
use clap::Subcommand;
use cnvs_config::Config;

#[derive(Subcommand)]
pub enum AgentCommand {
  Skills { slug: Option<String> },
}

pub(crate) fn run(command: AgentCommand) -> Result<i32> {
  match command {
    AgentCommand::Skills { slug } => skills(slug),
  }
}

fn skills(slug: Option<String>) -> Result<i32> {
  let config = Config::load()?;

  if let Some(slug) = slug {
    let skill = cnvs_agents::get_skill(&slug, &config, READ_ONLY_ACTIONS)
      .with_context(|| format!("unknown skill '{slug}'"))?;
    print!("{}", skill.content);
    return Ok(0);
  }

  println!("Available skills:");
  for skill in cnvs_agents::list_skills(&config, READ_ONLY_ACTIONS) {
    let mut skill = skill;
    match (
      skill.frontmatter.remove("name"),
      skill.frontmatter.remove("description"),
    ) {
      (None, _) => bail!("Missing name in frontmatter for skill: {}", skill.slug),
      (Some(name), description) => {
        println!("- name: {name}");
        if let Some(description) = description {
          println!("  description: {description}");
        }
      }
    }

    for (key, value) in skill.frontmatter.drain() {
      println!("  {key}: {value}");
    }
    println!();
  }
  Ok(0)
}
