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
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::{Args, Parser, Subcommand};
use cnvs_config::Config;
use cnvs_protocol::{ApiRequest, DaemonState, Header, Request, Response, ResponseBody};
#[cfg(feature = "write-requests")]
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
  Agent {
    #[command(subcommand)]
    command: AgentCommand,
  },
  #[cfg(feature = "write-requests")]
  Gql(GqlArgs),
  Daemon {
    #[command(subcommand)]
    command: DaemonCommand,
  },
  Config {
    #[command(subcommand)]
    command: ConfigCommand,
  },
}

#[derive(Subcommand)]
enum AgentCommand {
  Skills { slug: Option<String> },
}

#[derive(Args)]
struct ApiArgs {
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

  #[arg(long, conflicts_with_all = ["body", "body_file"])]
  output: Option<PathBuf>,
}

#[derive(Args)]
struct GqlArgs {
  #[arg(long)]
  url: String,

  #[arg(long)]
  file: Option<PathBuf>,

  #[arg(long, conflicts_with = "variables_file")]
  variables: Option<String>,

  #[arg(long, conflicts_with = "variables")]
  variables_file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum ConfigCommand {
  Info,
  Edit,
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
    Command::Api(args) => {
      let output = args.output.clone();
      run_request(build_api_request(args)?, cli.verbose, output)
    }
    Command::Agent { command } => match command {
      AgentCommand::Skills { slug } => agent_skills(slug),
    },
    #[cfg(feature = "write-requests")]
    Command::Gql(args) => run_request(build_gql_request(args)?, cli.verbose, None),

    Command::Config { command } => match command {
      ConfigCommand::Info => config_info(),
      ConfigCommand::Edit => config_edit(),
    },

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

fn agent_skills(slug: Option<String>) -> Result<i32> {
  if let Some(slug) = slug {
    let skill = cnvs_agents::get_skill(&slug).with_context(|| format!("unknown skill '{slug}'"))?;
    print!("{}", skill.content);
    return Ok(0);
  }

  println!("Available skills:");
  for skill in cnvs_agents::list_skills() {
    println!("\n- {}", skill.slug);
    for line in skill.frontmatter.lines() {
      println!("  {line}");
    }
  }
  Ok(0)
}

fn config_info() -> Result<i32> {
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

fn config_edit() -> Result<i32> {
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

  let status = ProcessCommand::new("sh")
    .args(["-c", "exec $EDITOR \"$1\"", "cnvs-editor"])
    .arg(&path)
    .status()
    .with_context(|| format!("could not start editor {editor}"))?;
  if !status.success() {
    bail!("editor exited with {status}");
  }
  Ok(0)
}

fn build_api_request(args: ApiArgs) -> Result<ApiRequest> {
  let mut url = resolve_url(&args.url)?;
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
      (Some(_), Some(_)) => bail!("body and body-file cannot be used together"),
    },
    download: args.output.is_some(),
  })
}

#[cfg(feature = "write-requests")]
fn build_gql_request(args: GqlArgs) -> Result<ApiRequest> {
  let url = resolve_url(&args.url)?;
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

    (Some(_), Some(_)) => bail!("variables and variables-file cannot be used together"),
  };

  Ok(ApiRequest {
    method: "POST".into(),
    url: url.into(),
    headers: vec![Header {
      name: "content-type".into(),
      value: "application/json".into(),
    }],
    body: Some(serde_json::to_string(&json!({
        "query": query,
        "variables": variables,
    }))?),
    download: false,
  })
}

fn run_request(request: ApiRequest, verbose: bool, output: Option<PathBuf>) -> Result<i32> {
  let (_, started) = ensure_daemon_running(None)?;

  if started && verbose {
    eprintln!("started daemon");
  }

  let Some(response) = send_request_to_daemon(&Request::Api(request))? else {
    bail!("daemon stopped before accepting the request; run the command again");
  };

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
                return Err(error).with_context(|| format!("could not move {}", source.display()));
              }
            }
            if let Some(directory) = source.parent() {
              let _ = fs::remove_dir(directory);
            }
          }
          ResponseBody::Base64(encoded) => {
            let bytes = STANDARD
              .decode(encoded)
              .context("daemon returned invalid base64 data")?;
            fs::write(&path, bytes)
              .with_context(|| format!("could not write {}", path.display()))?;
          }
          ResponseBody::Text(text) => {
            fs::write(&path, text)
              .with_context(|| format!("could not write {}", path.display()))?;
          }
        }
      } else {
        match body {
          ResponseBody::Text(text) => print!("{text}"),
          ResponseBody::Base64(encoded) => {
            let bytes = STANDARD
              .decode(encoded)
              .context("daemon returned invalid base64 data")?;
            io::stdout().write_all(&bytes)?;
          }
          ResponseBody::File(source) => {
            let mut file = fs::File::open(&source)?;
            io::copy(&mut file, &mut io::stdout())?;
            fs::remove_file(&source)?;
            if let Some(directory) = source.parent() {
              let _ = fs::remove_dir(directory);
            }
          }
        }
        io::stdout().flush()?;
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

  let profile = requested.map_or_else(cnvs_daemon::discover_profile, Ok)?;

  let socket = cnvs_daemon::daemon_socket_path()?;
  match fs::remove_file(&socket) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
    Err(error) => {
      return Err(error).with_context(|| format!("could not remove {}", socket.display()))
    }
  }

  let mut child = spawn_daemon(&profile)?;
  wait_until_daemon_running(Some(&profile), Some(&mut child), true)
}

fn wait_until_daemon_running(
  expected_profile: Option<&std::path::Path>,
  mut child_process: Option<&mut Child>,
  started: bool,
) -> Result<(Status, bool)> {
  let deadline = Instant::now()
    .checked_add(STARTUP_TIMEOUT)
    .context("startup deadline overflow")?;

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

#[cfg(feature = "write-requests")]
fn parse_variables(value: &str) -> Result<Value> {
  let value: Value = serde_json::from_str(value).context("invalid GraphQL variables JSON")?;
  if !value.is_object() {
    bail!("GraphQL variables must be a JSON object");
  }
  Ok(value)
}

fn resolve_url(value: &str) -> Result<Url> {
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
  let base = Url::parse(default_host).with_context(|| "default_canvas_host must be a valid URL")?;
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
      url: "https://canvas.example/api?existing=yes".into(),
      query: vec![
        parse_key_value("include[]=term").unwrap(),
        parse_key_value("include[]=syllabus").unwrap(),
      ],
      headers: vec![],
      body: None,
      body_file: None,
      output: Some("submission.zip".into()),
    })
    .unwrap();

    assert!(request.url.contains("existing=yes"));
    assert_eq!(request.url.matches("include%5B%5D=").count(), 2);
    assert!(request.download);
  }

  #[test]
  fn explicit_hosts_are_not_replaced_by_the_default() {
    let url = resolve_url("https://other.example/api/v1/users/self").unwrap();

    assert_eq!(url.as_str(), "https://other.example/api/v1/users/self");
  }

  #[test]
  fn parses_header_at_first_colon() {
    let header = parse_header("Authorization: scheme:value").unwrap();
    assert_eq!(header.name, "Authorization");
    assert_eq!(header.value, "scheme:value");
  }
}
