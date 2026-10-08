mod assignments_query;
mod cdp;
mod ownership;
mod profile;
mod request;

use anyhow::{Context, Result};
use cdp::ChromeDeveloperProtocol;
use cnvs_protocol::{
  canvas_daemon_server::{CanvasDaemon, CanvasDaemonServer},
  ApiRequest, ApiResponse, AssignmentsRequest, DaemonState, DaemonStatus, Empty, MESSAGE_LIMIT,
  PROTOCOL_MAJOR, WORK_TIMEOUT,
};
pub use ownership::AlreadyOwned;
pub use profile::{daemon_socket_path, discover_profile, requested_profile, PROFILE_ENV};
use std::{
  collections::HashSet,
  os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{OpenOptionsExt, PermissionsExt},
  },
  path::{Path, PathBuf},
  sync::Arc,
  time::Duration,
};
use tokio::{
  net::UnixListener,
  sync::{mpsc, oneshot, watch, Semaphore},
  time::{timeout, Instant},
};
use tonic::{Request, Response, Status};

const IDLE_TIMEOUT: Duration = Duration::from_secs(3600);
const QUEUE_CAPACITY: usize = 4;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

struct Work {
  request: ApiRequest,
  deadline: Instant,
  reply: oneshot::Sender<Result<ApiResponse, Status>>,
}
#[derive(Clone)]
struct Service {
  queue: mpsc::Sender<Work>,
  state: watch::Sender<DaemonState>,
  profile: Arc<PathBuf>,
}
impl Service {
  async fn submit(&self, request: ApiRequest) -> Result<Response<ApiResponse>, Status> {
    if *self.state.borrow() != DaemonState::Running {
      return Err(Status::failed_precondition(
        "daemon is starting or stopping",
      ));
    }
    let (reply, result) = oneshot::channel();
    self
      .queue
      .try_send(Work {
        request,
        deadline: Instant::now()
          .checked_add(WORK_TIMEOUT)
          .ok_or_else(|| Status::internal("work deadline overflow"))?,
        reply,
      })
      .map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => {
          Status::resource_exhausted("browser queue is full (four queued operations)")
        }
        mpsc::error::TrySendError::Closed(_) => Status::unavailable("browser worker stopped"),
      })?;
    result
      .await
      .map_err(|_| Status::unavailable("browser worker stopped"))?
      .map(Response::new)
  }
}
#[tonic::async_trait]
impl CanvasDaemon for Service {
  async fn status(&self, request: Request<Empty>) -> Result<Response<DaemonStatus>, Status> {
    admit_connection(&request);
    Ok(Response::new(DaemonStatus {
      state: i32::from(*self.state.borrow()),
      pid: std::process::id(),
      profile_path: self.profile.as_os_str().as_bytes().to_vec(),
      protocol_major: PROTOCOL_MAJOR,
      daemon_version: env!("CARGO_PKG_VERSION").into(),
      write_requests_enabled: cfg!(feature = "write-requests"),
    }))
  }
  async fn stop(&self, request: Request<Empty>) -> Result<Response<Empty>, Status> {
    admit_connection(&request);
    self.state.send_replace(DaemonState::Stopping);
    Ok(Response::new(Empty {}))
  }
  async fn api(&self, request: Request<ApiRequest>) -> Result<Response<ApiResponse>, Status> {
    admit_connection(&request);
    let request = request.into_inner();
    request::validate(&request, false)?;
    self.submit(request).await
  }
  async fn assignments(
    &self,
    request: Request<AssignmentsRequest>,
  ) -> Result<Response<ApiResponse>, Status> {
    admit_connection(&request);
    let request = request::assignments_request(request.into_inner())
      .map_err(|e| Status::invalid_argument(e.to_string()))?;
    request::validate(&request, true)?;
    self.submit(request).await
  }
}

/// Runs the daemon until stopped.
///
/// # Errors
/// Returns an error when logging cannot be initialized, the socket cannot be owned, or the
/// transport or browser worker fails.
pub async fn run(profile: PathBuf) -> Result<()> {
  let result = run_daemon(profile).await;
  if let Err(error) = &result {
    tracing::error!(error = format!("{error:#}"), "daemon stopped");
  }
  result
}

/// Initializes file logging to `~/.cnvs/daemon.log`. Verbosity follows `RUST_LOG` (default `info`).
fn init_logging() -> Result<()> {
  let path = profile::daemon_log_path()?;
  // The owner has already validated and secured the parent directory.
  let file = open_log(&path)?;
  let filter = tracing_subscriber::EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
  tracing_subscriber::fmt()
    .with_env_filter(filter)
    .with_writer(std::sync::Mutex::new(file))
    .with_ansi(false)
    .try_init()
    .map_err(|error| anyhow::anyhow!("could not initialize daemon logging: {error}"))?;
  Ok(())
}

fn open_log(path: &Path) -> Result<std::fs::File> {
  let file = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .mode(0o600)
    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
    .open(path)
    .with_context(|| format!("could not open daemon log file {}", path.display()))?;
  let metadata = file.metadata()?;
  ownership::check_owned(&metadata)?;
  if !metadata.is_file() {
    anyhow::bail!("daemon log is not a regular file");
  }
  file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
  Ok(file)
}

async fn run_daemon(profile: PathBuf) -> Result<()> {
  let socket = daemon_socket_path()?;
  let mut owner = ownership::Owner::acquire(&socket).await?;
  init_logging()?;
  tracing::info!(
    pid = std::process::id(),
    profile = %profile.display(),
    socket = %socket.display(),
    "daemon starting"
  );
  let listener = UnixListener::bind(&socket)?;
  owner.bound()?;
  // Keep completed handoffs until a later safely owned startup: the CLI may still
  // be moving a file when this process stops. No delivery acknowledgement in Phase A.
  let downloads = socket.with_file_name("downloads");
  if downloads.exists() {
    let metadata = std::fs::symlink_metadata(&downloads)?;
    ownership::check_owned(&metadata)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
      anyhow::bail!("invalid download directory");
    }
    std::fs::remove_dir_all(&downloads)?;
  }
  std::fs::create_dir(&downloads)?;
  std::fs::set_permissions(&downloads, std::fs::Permissions::from_mode(0o700))?;
  let endpoint = profile::devtools_ws_endpoint(&profile);
  run_listener(listener, profile, endpoint, downloads).await
}

async fn run_listener(
  listener: UnixListener,
  profile: PathBuf,
  endpoint: Result<String>,
  downloads: PathBuf,
) -> Result<()> {
  let (queue, receiver) = mpsc::channel(QUEUE_CAPACITY);
  let (state, _) = watch::channel(DaemonState::Starting);
  let service = Service {
    queue,
    state: state.clone(),
    profile: Arc::new(profile),
  };
  let (finished, mut finish) = watch::channel(false);
  let worker_state = state.clone();
  let mut worker = tokio::spawn(async move {
    let result = browser_worker(endpoint, receiver, &worker_state, &downloads).await;
    worker_state.send_replace(DaemonState::Stopping);
    finished.send_replace(true);
    result
  });
  let connections = Arc::new(Semaphore::new(32));
  // Dropping this sender forcibly closes connection I/O, including Tonic's
  // independently spawned connection tasks, only after the worker has joined.
  let (_connection_lifetime, closed) = watch::channel(());
  let (accept_failed, mut accept_failure) = oneshot::channel();
  let incoming = futures_util::stream::unfold(
    (listener, connections, closed, accept_failed),
    |(listener, connections, closed, accept_failed)| async move {
      let permit = connections.clone().acquire_owned().await.ok()?;
      let mut connection_closed = closed.clone();
      let accepted = match listener.accept().await {
        Ok((stream, _)) => LimitedConnection {
          stream,
          is_closed: false,
          closed: Box::pin(async move {
            let _ = connection_closed.changed().await;
          }),
          _permit: permit,
          admitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
          admission_deadline: Box::pin(tokio::time::sleep(cnvs_protocol::CONNECT_TIMEOUT)),
        },
        Err(error) => {
          let _ = accept_failed.send(error);
          return None;
        }
      };
      Some((
        Ok::<_, std::io::Error>(accepted),
        (listener, connections, closed, accept_failed),
      ))
    },
  );
  let server = tonic::transport::Server::builder()
    .max_concurrent_streams(8)
    .concurrency_limit_per_connection(8)
    .http2_max_header_list_size(16 * 1024)
    .max_connection_age(Duration::from_secs(330))
    .max_connection_age_grace(Duration::from_secs(5))
    .timeout(cnvs_protocol::API_TIMEOUT)
    .add_service(
      CanvasDaemonServer::new(service)
        .max_decoding_message_size(MESSAGE_LIMIT)
        .max_encoding_message_size(MESSAGE_LIMIT),
    )
    .serve_with_incoming_shutdown(incoming, async move {
      let _ = finish.wait_for(|done| *done).await;
    });
  let server = async move {
    tokio::select! {
      result = server => {
        // EOF may complete the server in the same poll that reports an accept failure.
        if let Ok(error) = accept_failure.try_recv() {
          return Err(error).context("daemon socket accept failed");
        }
        result.context("daemon transport failed")
      },
      Ok(error) = &mut accept_failure => Err(error).context("daemon socket accept failed"),
    }
  };
  tokio::pin!(server);
  let result = tokio::select! {
    result = &mut server => {
      // Let bounded in-flight work finish and restore browser/download state before exit.
      state.send_replace(DaemonState::Stopping);
      let cleanup = worker.await;
      result?;
      cleanup??;
      Ok(())
    }
    result = &mut worker => {
      // Browser cleanup is complete before a forced transport shutdown is allowed.
      let transport = timeout(Duration::from_secs(5), &mut server).await;
      result??;
      if let Ok(result) = transport { result?; }
      Ok(())
    }
  };
  result
}

async fn browser_worker(
  endpoint: Result<String>,
  mut queue: mpsc::Receiver<Work>,
  state: &watch::Sender<DaemonState>,
  downloads: &Path,
) -> Result<()> {
  let mut changes = state.subscribe();
  let endpoint = endpoint?;
  tracing::info!("connecting to Chrome DevTools");
  let mut cdp = tokio::select! {
    result = ChromeDeveloperProtocol::connect(&endpoint) => {
      let cdp = result.inspect_err(|error| {
        tracing::warn!(error = format!("{error:#}"), "Chrome connection failed");
      })?;
      tracing::info!("connected to Chrome; daemon is running");
      cdp
    }
    () = async { let _ = changes.wait_for(|s| *s == DaemonState::Stopping).await; } => {
      tracing::info!("stop requested before Chrome connected");
      return Ok(());
    }
  };
  state.send_if_modified(|s| {
    if *s == DaemonState::Starting {
      *s = DaemonState::Running;
      true
    } else {
      false
    }
  });
  let mut targets = HashSet::new();
  let mut idle = Instant::now()
    .checked_add(IDLE_TIMEOUT)
    .context("idle deadline overflow")?;
  while *state.borrow() == DaemonState::Running {
    tokio::select! {
      biased;
      () = async { let _ = changes.wait_for(|s| *s == DaemonState::Stopping).await; } => {
        tracing::info!("stop requested; stopping worker");
        break;
      }
      () = tokio::time::sleep_until(idle) => {
        tracing::info!("idle timeout reached; stopping worker");
        break;
      }
      item = queue.recv() => {
        let Some(item) = item else { break; };
        if item.reply.is_closed() { continue; }
        if Instant::now() >= item.deadline { let _ = item.reply.send(Err(Status::deadline_exceeded("browser work expired in queue"))); continue; }
        // SIMPLIFIED: one browser request at a time because the CDP reader is not multiplexed;
        // introduce a dedicated CDP response dispatcher before allowing concurrent browser work.
        // This future belongs to the worker, never the cancellable RPC handler.
        let result = request::execute(&mut cdp, &item.request, &mut targets, item.deadline, downloads).await;
        if result.is_err() {
          // Browser errors may include URLs, tokens, or response data. Return details only to the caller.
          tracing::warn!("request failed");
        }
        let result = result.map_err(|e| {
          if cdp.closed { Status::unavailable("Chrome connection closed") }
          else if e.is::<tokio::time::error::Elapsed>() || Instant::now() >= item.deadline { Status::deadline_exceeded("browser execution deadline exceeded") }
          else { Status::internal(format!("{e:#}")) }
        });
        if let Err(Ok(response)) = item.reply.send(result) { cleanup_undelivered(response); }
        idle = Instant::now().checked_add(IDLE_TIMEOUT).context("idle deadline overflow")?;
        if cdp.closed {
          tracing::warn!("Chrome connection closed; stopping worker");
          break;
        }
      }
      event = cdp.next_json() => if !matches!(event, Ok(Some(_))) {
        tracing::warn!("Chrome event stream ended; stopping worker");
        break;
      },
    }
  }
  tracing::info!("browser worker stopped");
  state.send_replace(DaemonState::Stopping);
  queue.close();
  while let Some(item) = queue.recv().await {
    let _ = item.reply.send(Err(Status::unavailable("daemon stopping")));
  }
  let _ = timeout(
    CLEANUP_TIMEOUT,
    request::close_owned_targets(&mut cdp, &targets),
  )
  .await;
  Ok(())
}
fn cleanup_undelivered(response: ApiResponse) {
  if let Some(cnvs_protocol::api_response::Body::LocalFilePath(bytes)) = response.body {
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = path.parent() {
      let _ = std::fs::remove_dir(parent);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn log_file_is_private_and_rejects_symlinks_and_special_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let file = open_log(&path).unwrap();
    assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    drop(file);
    std::fs::remove_file(&path).unwrap();
    let target = directory.path().join("target");
    std::fs::write(&target, "preserve").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(open_log(&path).is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "preserve");
    std::fs::remove_file(&path).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    assert!(open_log(&path).is_err());
  }

  #[tokio::test]
  async fn worker_failure_is_not_reported_as_successful_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(directory.path().join("daemon.sock")).unwrap();
    let result = timeout(
      Duration::from_secs(2),
      run_listener(
        listener,
        directory.path().to_owned(),
        Err(anyhow::anyhow!("scripted endpoint failure")),
        directory.path().to_owned(),
      ),
    )
    .await
    .unwrap();
    assert!(format!("{:#}", result.unwrap_err()).contains("scripted endpoint failure"));
  }
}

// The permit is held for the whole HTTP/2 connection, not merely accept().
struct LimitedConnection {
  stream: tokio::net::UnixStream,
  is_closed: bool,
  closed: futures_util::future::BoxFuture<'static, ()>,
  _permit: tokio::sync::OwnedSemaphorePermit,
  admitted: Arc<std::sync::atomic::AtomicBool>,
  admission_deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
}
#[derive(Clone)]
struct ConnectionAdmission(Arc<std::sync::atomic::AtomicBool>);
fn admit_connection<T>(request: &Request<T>) {
  if let Some(info) = request.extensions().get::<ConnectionAdmission>() {
    info.0.store(true, std::sync::atomic::Ordering::Relaxed);
  }
}
impl tonic::transport::server::Connected for LimitedConnection {
  type ConnectInfo = ConnectionAdmission;
  fn connect_info(&self) -> Self::ConnectInfo {
    ConnectionAdmission(self.admitted.clone())
  }
}
impl tokio::io::AsyncRead for LimitedConnection {
  fn poll_read(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    buf: &mut tokio::io::ReadBuf<'_>,
  ) -> std::task::Poll<std::io::Result<()>> {
    use std::future::Future;
    if self.is_closed || self.closed.as_mut().poll(cx).is_ready() {
      self.is_closed = true;
      return std::task::Poll::Ready(Err(std::io::Error::new(
        std::io::ErrorKind::ConnectionAborted,
        "daemon stopped",
      )));
    }
    if !self.admitted.load(std::sync::atomic::Ordering::Relaxed)
      && self.admission_deadline.as_mut().poll(cx).is_ready()
    {
      return std::task::Poll::Ready(Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "daemon handshake/first request deadline exceeded",
      )));
    }
    std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
  }
}
impl tokio::io::AsyncWrite for LimitedConnection {
  fn poll_write(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    buf: &[u8],
  ) -> std::task::Poll<std::io::Result<usize>> {
    std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
  }
  fn poll_flush(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<std::io::Result<()>> {
    std::pin::Pin::new(&mut self.stream).poll_flush(cx)
  }
  fn poll_shutdown(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<std::io::Result<()>> {
    std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
  }
}
