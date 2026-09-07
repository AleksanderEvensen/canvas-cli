use std::{io::Read, path::PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::{json, Value};

use super::api;
use crate::utilities::{ApiRequestBuilder, RequestBody};

#[derive(Args)]
pub struct GqlArgs {
  #[arg(long, help = "GraphQL endpoint URL")]
  url: String,

  #[arg(
    long,
    value_name = "PATH",
    help = "Read the GraphQL query from PATH instead of stdin"
  )]
  file: Option<PathBuf>,

  #[arg(
    long,
    conflicts_with = "variables_file",
    help = "Inline JSON variables object"
  )]
  variables: Option<String>,

  #[arg(
    long,
    conflicts_with = "variables",
    value_name = "PATH",
    help = "Read the JSON variables object from PATH"
  )]
  variables_file: Option<PathBuf>,
}

pub async fn run(args: GqlArgs, verbose: bool) -> Result<i32> {
  let url = api::resolve_url(&args.url)?;
  let query = if let Some(path) = args.file {
    api::read_input(&path)?
  } else {
    let mut query = String::new();
    std::io::stdin().read_to_string(&mut query)?;
    query
  };
  if query.trim().is_empty() {
    bail!("GraphQL query is empty");
  }
  let variables = match (args.variables, args.variables_file) {
    (None, None) => json!({}),
    (Some(value), None) => parse_variables(&value)?,
    (None, Some(path)) => parse_variables(&api::read_input(&path)?)?,
    (Some(_), Some(_)) => bail!("variables and variables-file cannot be used together"),
  };
  let mut request = ApiRequestBuilder::new("POST", url);
  request.enable_verbose(verbose);
  request.body(RequestBody::json(json!({
    "query": query,
    "variables": variables,
  }))?);

  let response = request.json().await?;
  api::handle_response(response, verbose, None)
}

fn parse_variables(value: &str) -> Result<Value> {
  let value: Value = serde_json::from_str(value).context("invalid GraphQL variables JSON")?;
  if !value.is_object() {
    bail!("GraphQL variables must be a JSON object");
  }
  Ok(value)
}
