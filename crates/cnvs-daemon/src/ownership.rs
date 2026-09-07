use anyhow::{bail, Context, Result};
use std::{
  fs::{self, File, OpenOptions},
  io,
  os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
  path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct AlreadyOwned;
impl std::fmt::Display for AlreadyOwned {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("another daemon owns the socket")
  }
}
impl std::error::Error for AlreadyOwned {}

pub struct Owner {
  _lock: File,
  socket: PathBuf,
  identity: Option<(u64, u64)>,
}
pub fn check_owned(metadata: &fs::Metadata) -> Result<()> {
  // SAFETY: geteuid has no arguments or memory safety preconditions.
  if metadata.uid() != unsafe { libc::geteuid() } {
    bail!("daemon filesystem entry belongs to another user");
  }
  Ok(())
}
fn check_socket(path: &Path) -> Result<bool> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => {
      check_owned(&metadata)?;
      if !metadata.file_type().is_socket() {
        bail!("daemon socket path is not a socket (symlinks are rejected)");
      }
      Ok(true)
    }
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(e) => Err(e.into()),
  }
}
impl Owner {
  pub(crate) async fn acquire(socket: &Path) -> Result<Self> {
    let directory = socket.parent().context("socket has no parent")?;
    match fs::create_dir(directory) {
      Ok(()) => (),
      Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
      Err(e) => return Err(e.into()),
    }
    let metadata = fs::symlink_metadata(directory)?;
    check_owned(&metadata)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
      bail!("daemon directory must be a real directory");
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let lock = OpenOptions::new()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .mode(0o600)
      .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
      .open(directory.join("daemon.lock"))?;
    let metadata = lock.metadata()?;
    check_owned(&metadata)?;
    if !metadata.is_file() {
      bail!("daemon lock is not a regular file");
    }
    match lock.try_lock() {
      Ok(()) => (),
      Err(std::fs::TryLockError::WouldBlock) => return Err(AlreadyOwned.into()),
      Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    if check_socket(socket)? {
      match tokio::time::timeout(cnvs_protocol::CONNECT_TIMEOUT, tokio::net::UnixStream::connect(socket)).await.context("could not prove socket stale within five seconds")? {
        Ok(_) => bail!("an existing daemon is listening; if incompatible, stop it with the old binary before upgrading"),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(socket)?,
        Err(e) => return Err(e).context("could not prove the socket stale"),
      }
    }
    Ok(Self {
      _lock: lock,
      socket: socket.to_owned(),
      identity: None,
    })
  }
  pub(crate) fn bound(&mut self) -> Result<()> {
    let metadata = fs::symlink_metadata(&self.socket)?;
    self.identity = Some((metadata.dev(), metadata.ino()));
    fs::set_permissions(&self.socket, fs::Permissions::from_mode(0o600))?;
    Ok(())
  }
}
impl Drop for Owner {
  fn drop(&mut self) {
    if let Ok(metadata) = fs::symlink_metadata(&self.socket) {
      if self.identity == Some((metadata.dev(), metadata.ino())) && metadata.file_type().is_socket()
      {
        let _ = fs::remove_file(&self.socket);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[tokio::test]
  async fn only_locked_owner_removes_its_own_socket() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("daemon.sock");
    let mut owner = Owner::acquire(&socket).await.unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    owner.bound().unwrap();
    let original = fs::symlink_metadata(&socket).unwrap().ino();
    assert!(Owner::acquire(&socket)
      .await
      .err()
      .unwrap()
      .is::<AlreadyOwned>());
    assert_eq!(fs::symlink_metadata(&socket).unwrap().ino(), original);
    drop(listener);
    drop(owner);
    assert!(!socket.exists());
    assert!(directory.path().join("daemon.lock").is_file());
    // Stale sockets can be recovered, but a live legacy daemon has no new lock.
    let legacy = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert!(Owner::acquire(&socket).await.is_err());
    assert!(socket.exists());
    drop(legacy);
    let mut owner = Owner::acquire(&socket).await.unwrap();
    assert!(!socket.exists());
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    owner.bound().unwrap();
    fs::remove_file(&socket).unwrap();
    fs::write(&socket, b"replacement").unwrap();
    drop(owner);
    drop(listener);
    assert_eq!(fs::read(&socket).unwrap(), b"replacement");
    assert!(Owner::acquire(&socket).await.is_err());
    fs::remove_file(&socket).unwrap();
    std::os::unix::fs::symlink("missing", &socket).unwrap();
    assert!(Owner::acquire(&socket).await.is_err());
    assert!(fs::symlink_metadata(&socket)
      .unwrap()
      .file_type()
      .is_symlink());
  }
  #[tokio::test]
  async fn symlinked_directory_and_lock_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target");
    fs::create_dir(&target).unwrap();
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(Owner::acquire(&link.join("daemon.sock")).await.is_err());
    fs::write(target.join("data"), b"preserve").unwrap();
    std::os::unix::fs::symlink(target.join("data"), target.join("daemon.lock")).unwrap();
    assert!(Owner::acquire(&target.join("daemon.sock")).await.is_err());
    assert_eq!(fs::read(target.join("data")).unwrap(), b"preserve");
  }
}
