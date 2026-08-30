use std::{
  env, fs,
  path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use cnvs_config::Config;

pub const PROFILE_ENV: &str = "CNVS_CHROME_USER_DATA_DIR";
pub const CONFIG_DIR: &str = ".cnvs";
pub const SOCKET_FILE: &str = "daemon.sock";

/// Gets the path to the unix socket file
#[inline]
///
/// # Errors
///
/// Returns an error when the home directory cannot be determined.
pub fn daemon_socket_path() -> Result<PathBuf> {
  Ok(
    dirs::home_dir()
      .context("could not determine home directory")?
      .join(CONFIG_DIR)
      .join(SOCKET_FILE),
  )
}

///
/// # Errors
///
/// Returns an error when the selected profile cannot be opened or does not have an active `DevTools` port.
pub fn requested_profile(explicit: Option<PathBuf>) -> Result<Option<PathBuf>> {
  explicit
    .or_else(|| env::var_os(PROFILE_ENV).map(PathBuf::from))
    .map(|path| normalize_profile(&path))
    .transpose()
}

///
/// # Errors
///
/// Returns an error when the home directory or configuration cannot be read, or when no active profile is found.
pub fn discover_profile() -> Result<PathBuf> {
  let home = dirs::home_dir().context("could not determine home directory")?;
  let config = Config::load()?;

  #[cfg(target_os = "macos")]
  let candidates = {
    let root = home.join("Library/Application Support");
    [
      "Google/Chrome",
      "Google/Chrome Beta",
      "Chromium",
      "BraveSoftware/Brave-Browser",
      "Microsoft Edge",
      "net.imput.helium",
    ]
    .map(|path| root.join(path))
    .into_iter()
  };

  #[cfg(target_os = "linux")]
  let candidates = {
    let root = home.join(".config");
    [
      "google-chrome",
      "chromium",
      "BraveSoftware/Brave-Browser",
      "BraveSoftware/Brave-Origin",
      "microsoft-edge",
      "vivaldi",
      "net.imput.helium",
    ]
    .map(|path| root.join(path))
    .into_iter()
  };

  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
  let candidates = std::iter::empty();

  config
    .chrome_user_data_dirs
    .into_iter()
    .chain(candidates)
    .find(|path| path.join("DevToolsActivePort").is_file())
    .map(|path| normalize_profile(&path))
    .transpose()?
    .context(format!(
      "no active Chromium CDP profile found; enable Remote Debugging, set {PROFILE_ENV}, or add a path to chrome_user_data_dirs in ~/.config/cnvs/config.toml"
    ))
}

fn normalize_profile(path: &Path) -> Result<PathBuf> {
  let path = fs::canonicalize(path).with_context(|| {
    format!(
      "could not open Chrome user-data directory {}",
      path.display()
    )
  })?;
  if !path.join("DevToolsActivePort").is_file() {
    bail!(
      "{} has no DevToolsActivePort; enable Remote Debugging for that profile",
      path.display()
    );
  }
  Ok(path)
}

pub fn devtools_ws_endpoint(profile: &Path) -> Result<String> {
  let path = profile.join("DevToolsActivePort");
  let contents =
    fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
  let mut lines = contents
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty());
  let port = lines
    .next()
    .context("DevToolsActivePort is missing the port")?;
  let websocket_path = lines
    .next()
    .context("DevToolsActivePort is missing the WebSocket path")?;
  Ok(format!("ws://127.0.0.1:{port}{websocket_path}"))
}
