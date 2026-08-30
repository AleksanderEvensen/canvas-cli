use std::{io::Read, path::PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use cnvs_protocol::{ApiRequest, Header};
use serde_json::{json, Value};

use super::api;

#[derive(Args)]
pub struct GqlArgs {
  #[arg(long)]
  url: String,
  #[arg(long)]
  file: Option<PathBuf>,
  #[arg(long, conflicts_with = "variables_file")]
  variables: Option<String>,
  #[arg(long, conflicts_with = "variables")]
  variables_file: Option<PathBuf>,
}

pub(crate) fn run(args: GqlArgs, verbose: bool) -> Result<i32> {
  api::run_request(build_request(args)?, verbose, None)
}

fn build_request(args: GqlArgs) -> Result<ApiRequest> {
  let url = api::resolve_url(&args.url)?;
  let query = match args.file {
    Some(path) => api::read_input(&path)?,
    None => {
      let mut query = String::new();
      std::io::stdin().read_to_string(&mut query)?;
      query
    }
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
  Ok(ApiRequest {
    method: "POST".into(),
    url: url.into(),
    headers: vec![Header {
      name: "content-type".into(),
      value: "application/json".into(),
    }],
    body: Some(serde_json::to_string(
      &json!({ "query": query, "variables": variables }),
    )?),
    download: false,
  })
}
fn parse_variables(value: &str) -> Result<Value> {
  let value: Value = serde_json::from_str(value).context("invalid GraphQL variables JSON")?;
  if !value.is_object() {
    bail!("GraphQL variables must be a JSON object");
  }
  Ok(value)
}

