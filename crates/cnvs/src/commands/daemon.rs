use std::{
  env, fs,
  io::{self, BufRead, BufReader, Read, Write},
  net::Shutdown,
  os::unix::{net::UnixStream, process::CommandExt},
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  thread,
  time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use cnvs_protocol::{DaemonState, Request, Response};

const STARTUP_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Subcommand)]
pub enum DaemonCommand {
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

#[derive(Debug)]
pub(crate) struct Status {
  pub(crate) state: DaemonState,
  pub(crate) pid: u32,
  pub(crate) profile: PathBuf,
}

pub(crate) fn run(command: DaemonCommand) -> Result<i32> {
  match command {
    DaemonCommand::Start {
      chrome_user_data_dir,
    } => {
      let (status, started) = ensure_running(chrome_user_data_dir)?;
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
    DaemonCommand::Stop => match send_request(&Request::Stop)? {
      Some(Response::Stopped) | None => {
        println!("daemon is stopped");
        Ok(0)
      }
      Some(Response::Error { message }) => bail!(message),
      Some(_) => bail!("daemon returned an unexpected response"),
    },
    DaemonCommand::Status => {
      let Some(status) = status()? else {
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
  }
}

pub(crate) fn ensure_running(explicit_profile: Option<PathBuf>) -> Result<(Status, bool)> {
  let requested = cnvs_daemon::requested_profile(explicit_profile)?;
  if let Some(status) = status()? {
    check_profile(&status, requested.as_deref())?;
    return match status.state {
      DaemonState::Running => Ok((status, false)),
      DaemonState::Starting => wait_until_running(requested.as_deref(), None, false),
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
  let mut child = spawn(&profile)?;
  wait_until_running(Some(&profile), Some(&mut child), true)
}

fn wait_until_running(
  expected: Option<&Path>,
  mut child: Option<&mut Child>,
  started: bool,
) -> Result<(Status, bool)> {
  let deadline = Instant::now()
    .checked_add(STARTUP_TIMEOUT)
    .context("startup deadline overflow")?;
  loop {
    if let Some(process) = child.as_deref_mut() {
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
    if let Some(status) = status()? {
      check_profile(&status, expected)?;
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

fn spawn(profile: &Path) -> Result<Child> {
  let mut command = Command::new(env::current_exe()?);
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

fn status() -> Result<Option<Status>> {
  match send_request(&Request::Status)? {
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

fn check_profile(status: &Status, expected: Option<&Path>) -> Result<()> {
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

pub(crate) fn send_request(request: &Request) -> Result<Option<Response>> {
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
