use std::{
  fs,
  io::{self, Read, Write},
  path::{Path, PathBuf},
};

use crate::utilities::{ApiRequestBuilder, RequestBody};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::Args;
use cnvs_config::Config;
use cnvs_protocol::{Header, Response, ResponseBody};
use url::Url;

#[derive(Clone)]
struct KeyValue {
  name: String,
  value: String,
}

#[derive(Args)]
pub struct ApiArgs {
  method: String,
  url: String,

  #[arg(long = "query", value_parser = parse_key_value)]
  query: Vec<KeyValue>,

  #[arg(long = "header", value_parser = parse_header)]
  headers: Vec<Header>,

  #[arg(long, conflicts_with = "body_file")]
  body: Option<String>,

  #[arg(long, conflicts_with = "body")]
  body_file: Option<PathBuf>,

  #[arg(long)]
  output: Option<PathBuf>,
}

pub(crate) fn get(path: &str, verbose: bool) -> Result<i32> {
  get_url(resolve_url(path)?, verbose)
}

pub(crate) fn get_url(url: Url, verbose: bool) -> Result<i32> {
  let mut request = ApiRequestBuilder::get(url);
  request.enable_verbose(verbose);
  handle_response(request.json()?, verbose, None)
}

pub(crate) fn run(args: ApiArgs, verbose: bool) -> Result<i32> {
  let output = args.output.clone();

  let mut url = resolve_url(&args.url)?;
  {
    let mut query = url.query_pairs_mut();
    query.extend_pairs(args.query.iter().map(|v| (&v.name, &v.value)));
  }

  if crate::READ_ONLY_ACTIONS && !args.method.eq_ignore_ascii_case("get") {
    bail!(
      "{} requests is not permitted, only GET requests are allowed",
      args.method.to_uppercase()
    )
  }

  let mut request = ApiRequestBuilder::new(args.method, url);

  request
    .enable_verbose(verbose)
    .headers(args.headers.into_iter().map(|v| (v.name, v.value)));

  match (args.body, args.body_file) {
    (None, None) => {}
    (Some(body), None) => {
      request.body(RequestBody::Text(body));
    }
    (None, Some(path)) => {
      request.body(RequestBody::Text(read_input(&path)?));
    }
    (Some(_), Some(_)) => bail!("body and body-file cannot be used together"),
  }

  let response = match &output {
    Some(_) => request.download()?,
    None => request.json()?,
  };

  handle_response(response, verbose, output)
}

pub(crate) fn handle_response(
  response: Response,
  verbose: bool,
  output: Option<PathBuf>,
) -> Result<i32> {
  match response {
    Response::Api {
      status,
      status_text,
      body,
    } => {
      if verbose {
        eprintln!("{status} {status_text}");
      }
      if !(200..300).contains(&status) {
        if let ResponseBody::Text(body) = &body {
          print!("{body}");
        }
        eprintln!("HTTP {status} {status_text}");
        return Ok(1);
      }
      if let Some(path) = output {
        write_output(path, body)?;
      } else {
        print_body(body)?;
        io::stdout().flush()?;
      }
      Ok(0)
    }
    Response::Error { message } => bail!(message),
    _ => bail!("daemon returned an unexpected response"),
  }
}

fn write_output(path: PathBuf, body: ResponseBody) -> Result<()> {
  match body {
    ResponseBody::File(source) => {
      match fs::rename(&source, &path) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(18) => {
          fs::copy(&source, &path)
            .with_context(|| format!("could not copy {}", source.display()))?;
          fs::remove_file(&source)?;
        }
        Err(error) => {
          return Err(error).with_context(|| format!("could not move {}", source.display()))
        }
      }
      if let Some(directory) = source.parent() {
        let _ = fs::remove_dir(directory);
      }
    }
    ResponseBody::Base64(encoded) => fs::write(
      &path,
      STANDARD
        .decode(encoded)
        .context("daemon returned invalid base64 data")?,
    )
    .with_context(|| format!("could not write {}", path.display()))?,
    ResponseBody::Text(text) => {
      fs::write(&path, text).with_context(|| format!("could not write {}", path.display()))?
    }
  }
  Ok(())
}

fn print_body(body: ResponseBody) -> Result<()> {
  match body {
    ResponseBody::Text(text) => print!("{text}"),
    ResponseBody::Base64(encoded) => io::stdout().write_all(
      &STANDARD
        .decode(encoded)
        .context("daemon returned invalid base64 data")?,
    )?,
    ResponseBody::File(source) => {
      let mut file = fs::File::open(&source)?;
      io::copy(&mut file, &mut io::stdout())?;
      fs::remove_file(&source)?;
      if let Some(directory) = source.parent() {
        let _ = fs::remove_dir(directory);
      }
    }
  }
  Ok(())
}

fn parse_header(value: &str) -> std::result::Result<Header, String> {
  let (name, value) = value
    .split_once(':')
    .ok_or_else(|| "headers must use 'Name: value'".to_owned())?;
  if name.trim().is_empty() {
    return Err("header name is empty".into());
  }
  Ok(Header {
    name: name.trim().into(),
    value: value.trim().into(),
  })
}
fn parse_key_value(value: &str) -> std::result::Result<KeyValue, String> {
  let (name, value) = value
    .split_once('=')
    .ok_or_else(|| "query parameters must use key=value".to_owned())?;
  if name.is_empty() {
    return Err("query parameter name is empty".into());
  }
  Ok(KeyValue {
    name: name.into(),
    value: value.into(),
  })
}
pub(crate) fn resolve_url(value: &str) -> Result<Url> {
  if let Ok(url) = Url::parse(value) {
    if url.host().is_some() {
      validate_url(&url)?;
      return Ok(url);
    }
  }
  let default_host = Config::load()?.default_canvas_host;
  if !value.starts_with('/') || value.starts_with("//") {
    bail!(
      "invalid URL; use an http(s) URL or an absolute path with default_canvas_host configured"
    );
  }
  let default_host = default_host
    .as_deref()
    .context("relative URLs require default_canvas_host in ~/.config/cnvs/config.toml")?;
  let base = Url::parse(default_host).context("default_canvas_host must be a valid URL")?;
  validate_url(&base)?;
  if base.host().is_none() {
    bail!("default_canvas_host must include a host");
  }
  base
    .join(value)
    .with_context(|| format!("could not append path to default_canvas_host: {value}"))
}
fn validate_url(url: &Url) -> Result<()> {
  if !matches!(url.scheme(), "http" | "https") {
    bail!("only http:// and https:// URLs are supported");
  }
  Ok(())
}
pub(crate) fn read_input(path: &Path) -> Result<String> {
  if path == Path::new("-") {
    read_stdin()
  } else {
    Ok(fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?)
  }
}
fn read_stdin() -> Result<String> {
  let mut value = String::new();
  io::stdin().read_to_string(&mut value)?;
  Ok(value)
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn explicit_hosts_are_not_replaced_by_the_default() {
    assert_eq!(
      resolve_url("https://other.example/api/v1/users/self")
        .unwrap()
        .as_str(),
      "https://other.example/api/v1/users/self"
    );
  }
  #[test]
  fn parses_header_at_first_colon() {
    let header = parse_header("Authorization: scheme:value").unwrap();
    assert_eq!(header.name, "Authorization");
    assert_eq!(header.value, "scheme:value");
  }
}
