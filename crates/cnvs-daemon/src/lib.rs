mod cdp;
mod profile;
mod request;

use std::{
  collections::HashSet,
  fs,
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
  time::Duration,
};

use anyhow::{Context, Result};
use cnvs_protocol::{DaemonState, Request, Response};
use tokio::{
  io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
  net::{UnixListener, UnixStream},
  time::{timeout, Instant},
};

use cdp::ChromeDeveloperProtocol;
use profile::devtools_ws_endpoint;
use request::{close_owned_targets, execute};

pub use profile::{daemon_socket_path, discover_profile, requested_profile, PROFILE_ENV};

const IDLE_TIMEOUT: Duration = Duration::from_hours(1);
const CLIENT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

///
/// # Errors
///
/// Returns an error when the daemon socket cannot be created, the browser connection fails, or a request cannot be processed.
pub async fn run(profile: PathBuf) -> Result<()> {
  let socket = daemon_socket_path()?;
  let directory = socket.parent().context("daemon socket has no parent")?;
  fs::create_dir_all(directory)?;
  fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;

  let listener = UnixListener::bind(&socket)
    .with_context(|| format!("could not bind daemon socket {}", socket.display()))?;
  fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;

  let result = run_listener(listener, &profile).await;
  let _ = fs::remove_file(&socket);
  result
}

async fn run_listener(listener: UnixListener, profile: &Path) -> Result<()> {
  let endpoint = devtools_ws_endpoint(profile)?;
  let connecting = ChromeDeveloperProtocol::connect(&endpoint);
  tokio::pin!(connecting);

  let mut cdp = loop {
    tokio::select! {
        result = &mut connecting => break result?,
        accepted = listener.accept() => {
            let (mut stream, _) = accepted?;
            match read_request(&mut stream).await {
                Ok(Request::Status) => reply(&mut stream, status(DaemonState::Starting, profile)).await?,
                Ok(Request::Stop) => {
                    reply(&mut stream, Response::Stopped).await?;
                    return Ok(());
                }
                Ok(Request::Api(_)) => reply(&mut stream, Response::Error { message: "daemon is still starting".into() }).await?,
                Err(error) => reply(&mut stream, Response::Error { message: error.to_string() }).await?,
            }
        }
    }
  };

  let mut owned_targets = HashSet::new();
  let mut idle_deadline = Instant::now()
    .checked_add(IDLE_TIMEOUT)
    .context("idle deadline overflow")?;

  loop {
    tokio::select! {
        message = cdp.next_json() => match message {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        },
        accepted = listener.accept() => {
            let (mut stream, _) = accepted?;
            let request = match read_request(&mut stream).await {
                Ok(request) => request,
                Err(error) => {
                    reply(&mut stream, Response::Error { message: error.to_string() }).await?;
                    continue;
                }
            };

            match request {
                Request::Status => reply(&mut stream, status(DaemonState::Running, profile)).await?,
                Request::Stop => {
                    close_owned_targets(&mut cdp, &owned_targets).await;
                    reply(&mut stream, Response::Stopped).await?;
                    break;
                }
                Request::Api(request) => {
                    let response = match execute(&mut cdp, &request, &mut owned_targets).await {
                        Ok(result) => Response::Api {
                            status: result.status,
                            status_text: result.status_text,
                            body: result.body,
                            body_base64: result.body_base64,
                        },
                        Err(error) => Response::Error { message: format!("{error:#}") },
                    };
                    reply(&mut stream, response).await?;
                    idle_deadline = Instant::now()
                      .checked_add(IDLE_TIMEOUT)
                      .context("idle deadline overflow")?;
                    if cdp.closed {
                        break;
                    }
                }
            }
        },
        () = tokio::time::sleep_until(idle_deadline) => {
            close_owned_targets(&mut cdp, &owned_targets).await;
            break;
        }
    }
  }

  Ok(())
}

fn status(state: DaemonState, profile: &Path) -> Response {
  Response::Status {
    state,
    pid: std::process::id(),
    profile: profile.display().to_string(),
  }
}

async fn read_request(stream: &mut UnixStream) -> Result<Request> {
  let mut line = String::new();
  timeout(
    CLIENT_HANDSHAKE_TIMEOUT,
    BufReader::new(stream).read_line(&mut line),
  )
  .await
  .context("client did not send a request")??;
  serde_json::from_str(&line).context("invalid daemon request")
}

async fn reply(stream: &mut UnixStream, response: Response) -> Result<()> {
  let mut bytes = serde_json::to_vec(&response)?;
  bytes.push(b'\n');
  stream.write_all(&bytes).await?;
  Ok(())
}
