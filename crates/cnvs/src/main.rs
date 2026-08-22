use std::{
  env, fs,
  io::{self, BufRead, BufReader, Read, Write},
  net::Shutdown,
  os::unix::{net::UnixStream, process::CommandExt},
  path::PathBuf,
  process::{Child, Command as ProcessCommand, Stdio},
  thread,
  time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use cnvs_protocol::{ApiRequest, DaemonState, Header, Request, Response};
use serde_json::{json, Value};
use url::Url;

/// If the daemon uses more than this amount of time to start, then we fail the request
const STARTUP_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Parser)]
#[command(
  name = "cnvs",
  about = "Use a signed-in Chromium session from the command line"
)]
struct Cli {
  #[arg(short, long, global = true)]
  verbose: bool,

  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  Api(ApiArgs),
  Gql(GqlArgs),
  Daemon {
    #[command(subcommand)]
    command: DaemonCommand,
  },
}

#[derive(Args)]
struct ApiArgs {
  method: String,
  url: Url,

  #[arg(long = "query", value_parser = parse_key_value)]
  query: Vec<KeyValue>,

  #[arg(long = "header", value_parser = parse_header)]
  headers: Vec<Header>,

  #[arg(long, conflicts_with = "body_file")]
  body: Option<String>,

  #[arg(long, conflicts_with = "body")]
  body_file: Option<PathBuf>,
}

#[derive(Args)]
struct GqlArgs {
  #[arg(long)]
  url: Url,

  #[arg(long)]
  file: Option<PathBuf>,

  #[arg(long, conflicts_with = "variables_file")]
  variables: Option<String>,

  #[arg(long, conflicts_with = "variables")]
  variables_file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum DaemonCommand {
  Start {
    #[arg(long)]
    chrome_user_data_dir: Option<PathBuf>,
  },
  Stop,
  Status,
  #[command(name = "__run", hide = true)]
  Run {
    #[arg(long)]
    chrome_user_data_dir: PathBuf,
  },
}

#[derive(Clone)]
struct KeyValue {
  name: String,
  value: String,
}

#[derive(Debug)]
struct Status {
  state: DaemonState,
  pid: u32,
  profile: PathBuf,
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

fn run() -> Result<i32> {
  let cli = Cli::parse();
  match cli.command {
    Command::Api(args) => run_request(build_api_request(args)?, cli.verbose),
    Command::Gql(args) => run_request(build_gql_request(args)?, cli.verbose),

    Command::Daemon { command } => match command {
      DaemonCommand::Start {
        chrome_user_data_dir,
      } => {
        let (status, started) = ensure_daemon_running(chrome_user_data_dir)?;
        println!(
          "{} daemon (pid {}, profile {})",
          if started {
            "started"
          } else {
            "already running"
          },
          status.pid,
          status.profile.display()
        );
        Ok(0)
      }

      DaemonCommand::Stop => match send_request_to_daemon(&Request::Stop)? {
        Some(Response::Stopped) | None => {
          println!("daemon is stopped");
          Ok(0)
        }
        Some(Response::Error { message }) => bail!(message),
        Some(_) => bail!("daemon returned an unexpected response"),
      },

      DaemonCommand::Status => {
        let Some(status) = get_daemon_status()? else {
          println!("stopped");
          return Ok(1);
        };
        println!(
          "{} (pid {}, profile {})",
          match status.state {
            DaemonState::Starting => "starting",
            DaemonState::Running => "running",
          },
          status.pid,
          status.profile.display()
        );
        Ok(0)
      }

      DaemonCommand::Run {
        chrome_user_data_dir,
      } => {
        tokio::runtime::Builder::new_multi_thread()
          .enable_all()
          .build()?
          .block_on(cnvs_daemon::run(chrome_user_data_dir))?;
        Ok(0)
      }
    },
  }
}

fn build_api_request(args: ApiArgs) -> Result<ApiRequest> {
  let mut url = args.url;
  validate_url(&url)?;
  {
    let mut query = url.query_pairs_mut();
    for pair in args.query {
      query.append_pair(&pair.name, &pair.value);
    }
  }

  Ok(ApiRequest {
    method: args.method.to_ascii_uppercase(),
    url: url.into(),
    headers: args.headers,
    body: match (args.body, args.body_file) {
      (None, None) => None,
      (Some(body), None) => Some(body),
      (None, Some(path)) => Some(read_input(&path)?),
      (Some(_), Some(_)) => unreachable!("clap cli parsing rejects conflicting body options"),
    },
  })
}

fn build_gql_request(args: GqlArgs) -> Result<ApiRequest> {
  validate_url(&args.url)?;
  let query = match args.file {
    Some(path) => read_input(&path)?,
    None => read_stdin()?,
  };
  if query.trim().is_empty() {
    bail!("GraphQL query is empty");
  }

  let variables = match (args.variables, args.variables_file) {
    (None, None) => json!({}),

    (Some(variables), None) => parse_variables(&variables)?,

    // Read from stdin or specified file name
    (None, Some(path)) => parse_variables(&read_input(&path)?)?,

    (Some(_), Some(_)) => unreachable!("clap cli parsing rejects conflicting variable options"),
  };

  Ok(ApiRequest {
    method: "POST".into(),
    url: args.url.into(),
    headers: vec![Header {
      name: "content-type".into(),
      value: "application/json".into(),
    }],
    body: Some(serde_json::to_string(&json!({
        "query": query,
        "variables": variables,
    }))?),
  })
}

fn run_request(request: ApiRequest, verbose: bool) -> Result<i32> {
  let (_, started) = ensure_daemon_running(None)?;

  if started && verbose {
    eprintln!("started daemon");
  }

  let response = match send_request_to_daemon(&Request::Api(request))? {
    Some(response) => response,
    None => bail!("daemon stopped before accepting the request; run the command again"),
  };

  match response {
    Response::Api {
      status,
      status_text,
      body,
    } => {
      print!("{body}");
      io::stdout().flush()?;
      if verbose {
        eprintln!("{status} {status_text}");
      }
      if !(200..300).contains(&status) {
        eprintln!("HTTP {status} {status_text}");
        return Ok(1);
      }
      Ok(0)
    }
    Response::Error { message } => bail!(message),
    _ => bail!("daemon returned an unexpected response"),
  }
}

fn ensure_daemon_running(explicit_profile: Option<PathBuf>) -> Result<(Status, bool)> {
  let requested = cnvs_daemon::requested_profile(explicit_profile)?;

  if let Some(status) = get_daemon_status()? {
    // Check that the daemon is using the requested profile
    check_active_daemon_chrome_profile(&status, requested.as_deref())?;
    return match status.state {
      DaemonState::Running => Ok((status, false)),
      DaemonState::Starting => wait_until_daemon_running(requested.as_deref(), None, false),
    };
  }

  let profile = requested
    .clone()
    .map(Ok)
    .unwrap_or_else(cnvs_daemon::discover_profile)?;

  remove_stale_socket()?;

  let mut child = spawn_daemon(&profile)?;
  wait_until_daemon_running(Some(&profile), Some(&mut child), true)
}

fn wait_until_daemon_running(
  expected_profile: Option<&std::path::Path>,
  mut child_process: Option<&mut Child>,
  started: bool,
) -> Result<(Status, bool)> {
  let deadline = Instant::now() + STARTUP_TIMEOUT;

  loop {
    if let Some(process) = child_process.as_deref_mut() {
      if let Some(exit) = process.try_wait()? {
        let mut stderr = String::new();
        if let Some(mut pipe) = process.stderr.take() {
          pipe.read_to_string(&mut stderr)?;
        }
        bail!(
          "daemon exited during startup ({exit}){}",
          if stderr.trim().is_empty() {
            String::new()
          } else {
            format!(": {}", stderr.trim())
          }
        );
      }
    }

    if let Some(status) = get_daemon_status()? {
      check_active_daemon_chrome_profile(&status, expected_profile)?;
      if status.state == DaemonState::Running {
        return Ok((status, started));
      }
    }

    if Instant::now() >= deadline {
      bail!("daemon is still waiting for Chrome approval after {} seconds; it remains running (use `cnvs daemon status` or `cnvs daemon stop`)", STARTUP_TIMEOUT.as_secs());
    }
    thread::sleep(Duration::from_millis(100));
  }
}

fn spawn_daemon(profile: &std::path::Path) -> Result<Child> {
  let mut command = ProcessCommand::new(env::current_exe()?);
  command
    .args([
      "daemon",
      "__run",
      "--chrome-user-data-dir",
      profile
        .to_str()
        .context("Chrome user-data directory is not valid UTF-8")?,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
  command.process_group(0);
  command.spawn().context("could not launch daemon")
}

fn get_daemon_status() -> Result<Option<Status>> {
  match send_request_to_daemon(&Request::Status)? {
    Some(Response::Status {
      state,
      pid,
      profile,
    }) => Ok(Some(Status {
      state,
      pid,
      profile: PathBuf::from(profile),
    })),
    Some(Response::Error { message }) => bail!(message),
    Some(_) => bail!("daemon returned an unexpected response"),
    None => Ok(None),
  }
}

fn check_active_daemon_chrome_profile(
  status: &Status,
  expected: Option<&std::path::Path>,
) -> Result<()> {
  if let Some(expected) = expected {
    if status.profile != expected {
      bail!(
        "daemon uses profile {}, not {}; stop it before switching profiles",
        status.profile.display(),
        expected.display()
      );
    }
  }
  Ok(())
}

fn send_request_to_daemon(request: &Request) -> Result<Option<Response>> {
  let path = cnvs_daemon::daemon_socket_path()?;

  let mut stream = match UnixStream::connect(&path) {
    Ok(stream) => stream,
    Err(error)
      if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
      ) =>
    {
      return Ok(None)
    }
    Err(error) => {
      return Err(error).with_context(|| format!("could not connect to {}", path.display()))
    }
  };

  serde_json::to_writer(&mut stream, request)?;
  stream.write_all(b"\n")?;
  stream.shutdown(Shutdown::Write)?;

  let mut line = String::new();
  BufReader::new(stream).read_line(&mut line)?;
  if line.is_empty() {
    return Ok(None);
  }
  Ok(Some(
    serde_json::from_str(&line).context("invalid daemon response")?,
  ))
}

fn remove_stale_socket() -> Result<()> {
  let path = cnvs_daemon::daemon_socket_path()?;
  match fs::remove_file(&path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error).with_context(|| format!("could not remove {}", path.display())),
  }
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

fn parse_variables(value: &str) -> Result<Value> {
  let value: Value = serde_json::from_str(value).context("invalid GraphQL variables JSON")?;
  if !value.is_object() {
    bail!("GraphQL variables must be a JSON object");
  }
  Ok(value)
}

fn validate_url(url: &Url) -> Result<()> {
  if !matches!(url.scheme(), "http" | "https") {
    bail!("only http:// and https:// URLs are supported");
  }
  Ok(())
}

fn read_input(path: &std::path::Path) -> Result<String> {
  if path == std::path::Path::new("-") {
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
  fn repeated_query_values_are_preserved() {
    let request = build_api_request(ApiArgs {
      method: "get".into(),
      url: Url::parse("https://canvas.example/api?existing=yes").unwrap(),
      query: vec![
        parse_key_value("include[]=term").unwrap(),
        parse_key_value("include[]=syllabus").unwrap(),
      ],
      headers: vec![],
      body: None,
      body_file: None,
    })
    .unwrap();

    assert!(request.url.contains("existing=yes"));
    assert_eq!(request.url.matches("include%5B%5D=").count(), 2);
  }

  #[test]
  fn parses_header_at_first_colon() {
    let header = parse_header("Authorization: scheme:value").unwrap();
    assert_eq!(header.name, "Authorization");
    assert_eq!(header.value, "scheme:value");
  }
}
