use anyhow::{bail, Context, Result};
use clap::Subcommand;
use cnvs_protocol::{
  canvas_daemon_client::CanvasDaemonClient, DaemonState, DaemonStatus, Empty, API_TIMEOUT,
  CONNECT_TIMEOUT, MESSAGE_LIMIT, PROTOCOL_MAJOR,
};
use std::{
  env,
  io::{self, Read},
  os::unix::{ffi::OsStringExt, fs::FileTypeExt, process::CommandExt},
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  time::Duration,
};
use tokio::{
  net::UnixStream,
  time::{timeout, Instant},
};
use tonic::transport::{Channel, Endpoint};
pub type Client = CanvasDaemonClient<Channel>;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const OWNER_BUSY_EXIT: i32 = 75;

#[derive(Subcommand)]
pub enum DaemonCommand {
  #[command(about = "Start the browser connection daemon")]
  Start {
    #[arg(long, value_name = "PATH", help = "Chrome user-data directory to use")]
    chrome_user_data_dir: Option<PathBuf>,
  },

  #[command(about = "Stop the browser connection daemon")]
  Stop,

  #[command(about = "Show daemon state, process ID, and browser profile")]
  Status,

  #[command(name = "__run", hide = true)]
  Run {
    #[arg(long, value_name = "PATH")]
    chrome_user_data_dir: PathBuf,
  },
}

pub async fn run(command: DaemonCommand) -> Result<i32> {
  match command {
    DaemonCommand::Start {
      chrome_user_data_dir,
    } => {
      let (_, status, started) = ensure_running(chrome_user_data_dir).await?;
      println!(
        "{} daemon (pid {}, profile {})",
        if started {
          "started"
        } else {
          "already running"
        },
        status.pid,
        profile(&status).display()
      );
      Ok(0)
    }
    DaemonCommand::Status => {
      let Some((_, status)) = probe().await? else {
        println!("stopped");
        return Ok(1);
      };
      println!(
        "{} (pid {}, profile {})",
        match state(&status)? {
          DaemonState::Starting => "starting",
          DaemonState::Running => "running",
          DaemonState::Stopping => "stopping",
          DaemonState::Unspecified => bail!("unspecified daemon state"),
        },
        status.pid,
        profile(&status).display()
      );
      Ok(0)
    }
    DaemonCommand::Stop => {
      if let Some((mut client, _)) = probe().await? {
        let mut request = tonic::Request::new(Empty {});
        request.set_timeout(CONNECT_TIMEOUT);
        timeout(CONNECT_TIMEOUT, client.stop(request)).await??;
        let deadline = Instant::now()
          .checked_add(API_TIMEOUT)
          .context("stop deadline overflow")?;
        loop {
          if !socket_present()? {
            break;
          }
          if Instant::now() >= deadline {
            bail!("daemon is still stopping; shutdown wait timed out");
          }
          tokio::time::sleep(Duration::from_millis(100)).await;
        }
      }
      println!("daemon is stopped");
      Ok(0)
    }
    DaemonCommand::Run {
      chrome_user_data_dir,
    } => match cnvs_daemon::run(chrome_user_data_dir).await {
      Ok(()) => Ok(0),
      Err(e) if e.is::<cnvs_daemon::AlreadyOwned>() => Ok(OWNER_BUSY_EXIT),
      Err(e) => Err(e),
    },
  }
}

fn profile(status: &DaemonStatus) -> PathBuf {
  PathBuf::from(std::ffi::OsString::from_vec(status.profile_path.clone()))
}
fn state(status: &DaemonStatus) -> Result<DaemonState> {
  match DaemonState::try_from(status.state) {
    Ok(DaemonState::Unspecified) | Err(_) => bail!("daemon returned an unknown lifecycle state"),
    Ok(state) => Ok(state),
  }
}
fn compatible(status: &DaemonStatus) -> Result<()> {
  state(status)?;
  if status.protocol_major != PROTOCOL_MAJOR
    || status.write_requests_enabled != cfg!(feature = "write-requests")
  {
    bail!("incompatible daemon {} (protocol {}, write-requests={}); client {} (protocol {}, write-requests={}); stop the daemon with its matching binary and restart", status.daemon_version, status.protocol_major, status.write_requests_enabled, env!("CARGO_PKG_VERSION"), PROTOCOL_MAJOR, cfg!(feature = "write-requests"));
  }
  Ok(())
}
fn check_profile(status: &DaemonStatus, expected: Option<&Path>) -> Result<()> {
  if let Some(expected) = expected {
    if profile(status) != expected {
      bail!(
        "daemon uses profile {}, not {}; stop it before switching profiles",
        profile(status).display(),
        expected.display()
      );
    }
  }
  Ok(())
}

// Only a failed socket connect can mean absent. Once connected, every handshake,
// EOF, status, or compatibility failure is an error and must never trigger spawn.
async fn connect() -> Result<Option<Client>> {
  connect_at(cnvs_daemon::daemon_socket_path()?).await
}
async fn connect_at(path: PathBuf) -> Result<Option<Client>> {
  match std::fs::symlink_metadata(&path) {
    Ok(meta) if !meta.file_type().is_socket() => {
      bail!("daemon socket path is not a socket (symlinks are rejected)")
    }
    Ok(_) => (),
    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(e) => return Err(e.into()),
  }
  let stream = match timeout(CONNECT_TIMEOUT, UnixStream::connect(&path)).await? {
    Ok(stream) => stream,
    Err(e)
      if matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
      ) =>
    {
      return Ok(None)
    }
    Err(e) => return Err(e).context("could not connect to daemon socket"),
  };
  // Consume the verified connection. Do not redial/retry an application request.
  let stream = std::sync::Arc::new(tokio::sync::Mutex::new(Some(stream)));
  let endpoint = Endpoint::from_static("http://localhost").connect_timeout(CONNECT_TIMEOUT);
  let channel = endpoint.connect_with_connector(tower::service_fn(move |_| {
    let stream = stream.clone();
    async move {
      stream
        .lock()
        .await
        .take()
        .map(hyper_util::rt::TokioIo::new)
        .ok_or_else(|| {
          io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "daemon connection lost; request execution outcome may be unknown",
          )
        })
    }
  }));
  let channel = timeout(CONNECT_TIMEOUT, channel)
    .await
    .context(
      "daemon handshake timed out; stop a legacy daemon with the old binary before upgrading",
    )?
    .context(
      "incompatible daemon transport; stop a legacy daemon with the old binary before upgrading",
    )?;
  Ok(Some(
    Client::new(channel)
      .max_decoding_message_size(MESSAGE_LIMIT)
      .max_encoding_message_size(MESSAGE_LIMIT),
  ))
}
async fn read_status(client: &mut Client) -> Result<DaemonStatus> {
  let mut request = tonic::Request::new(Empty {});
  request.set_timeout(CONNECT_TIMEOUT);
  let status = timeout(CONNECT_TIMEOUT, client.status(request))
    .await
    .context("daemon status timed out; stop a legacy daemon with the old binary before upgrading")?
    .context(
      "incompatible or failed daemon; stop a legacy daemon with the old binary before upgrading",
    )?
    .into_inner();
  compatible(&status)?;
  Ok(status)
}
async fn probe() -> Result<Option<(Client, DaemonStatus)>> {
  let Some(mut client) = connect().await? else {
    return Ok(None);
  };
  let status = read_status(&mut client).await?;
  Ok(Some((client, status)))
}
fn socket_present() -> Result<bool> {
  match std::fs::symlink_metadata(cnvs_daemon::daemon_socket_path()?) {
    Ok(_) => Ok(true),
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(e) => Err(e.into()),
  }
}
pub async fn ensure_running(explicit: Option<PathBuf>) -> Result<(Client, DaemonStatus, bool)> {
  let requested = cnvs_daemon::requested_profile(explicit)?;
  timeout(STARTUP_TIMEOUT, ensure_inner(requested)).await.context("daemon startup exceeded one minute; a daemon waiting for Chrome approval remains alive (use daemon status or daemon stop)")?
}
async fn ensure_inner(requested: Option<PathBuf>) -> Result<(Client, DaemonStatus, bool)> {
  let existing = probe().await?;
  let (mut client, mut child, expected, started) = if let Some((client, status)) = existing {
    check_profile(&status, requested.as_deref())?;
    if state(&status)? == DaemonState::Running {
      return Ok((client, status, false));
    }
    (Some(client), None, requested, false)
  } else {
    let profile = requested.map_or_else(cnvs_daemon::discover_profile, Ok)?;
    (None, Some(spawn(&profile)?), Some(profile), true)
  };
  loop {
    if let Some(process) = child.as_mut() {
      if let Some(exit) = process.try_wait()? {
        if exit.code() != Some(OWNER_BUSY_EXIT) {
          let mut stderr = String::new();
          if let Some(mut pipe) = process.stderr.take() {
            pipe.read_to_string(&mut stderr)?;
          }
          bail!("daemon exited during startup ({exit}): {}", stderr.trim());
        }
        child = None; // A lock loser waits for the winner within the same deadline.
      }
    }
    if client.is_none() {
      client = connect().await?;
    }
    if let Some(client) = client.as_mut() {
      let status = read_status(client).await?;
      check_profile(&status, expected.as_deref())?;
      match state(&status)? {
        DaemonState::Running => return Ok((client.clone(), status, started)),
        DaemonState::Stopping => bail!("daemon is stopping; wait for shutdown before starting"),
        _ => (),
      }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

fn spawn(profile: &Path) -> Result<Child> {
  let mut command = Command::new(env::current_exe()?);
  command
    .args(["daemon", "__run", "--chrome-user-data-dir"])
    .arg(profile)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
  command.process_group(0);
  command.spawn().context("could not launch daemon")
}

#[cfg(test)]
mod tests {
  use super::*;
  #[tokio::test]
  async fn legacy_eof_and_non_socket_paths_never_mean_absent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.sock");
    assert!(connect_at(path.clone()).await.unwrap().is_none());
    std::fs::write(&path, b"keep").unwrap();
    assert!(connect_at(path.clone()).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("missing", &path).unwrap();
    assert!(connect_at(path.clone()).await.is_err());
    std::fs::remove_file(&path).unwrap();
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let legacy = tokio::spawn(async move {
      use tokio::io::AsyncWriteExt;
      let (mut stream, _) = listener.accept().await.unwrap();
      stream
        .write_all(b"{\"type\":\"error\",\"message\":\"invalid request\"}\n")
        .await
        .unwrap();
    });
    match connect_at(path.clone()).await {
      Err(_) => (),
      Ok(Some(mut client)) => assert!(read_status(&mut client).await.is_err()),
      Ok(None) => panic!("a live legacy peer was classified absent"),
    }
    legacy.await.unwrap();
    assert!(path.exists());
  }
  #[tokio::test]
  async fn inaccessible_socket_is_an_error_and_is_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.sock");
    let _listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Privileged test runners may bypass filesystem permissions.
    if let Err(error) = UnixStream::connect(&path).await {
      assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
      let error = connect_at(path.clone()).await.err().unwrap();
      assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::PermissionDenied
      );
    }
    assert!(path.exists());
  }

  #[test]
  fn version_features_and_unknown_states_require_restart() {
    let good = DaemonStatus {
      state: DaemonState::Running.into(),
      protocol_major: PROTOCOL_MAJOR,
      write_requests_enabled: cfg!(feature = "write-requests"),
      ..Default::default()
    };
    assert!(compatible(&good).is_ok());
    assert!(compatible(&DaemonStatus {
      protocol_major: 0,
      ..good.clone()
    })
    .is_err());
    assert!(compatible(&DaemonStatus {
      write_requests_enabled: !good.write_requests_enabled,
      ..good.clone()
    })
    .is_err());
    for state in [0, 999] {
      assert!(compatible(&DaemonStatus {
        state,
        ..good.clone()
      })
      .is_err());
    }
  }
}
