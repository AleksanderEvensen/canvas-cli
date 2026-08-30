use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const CONFIG_DIRECTORY: &str = "cnvs";
const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Default, Deserialize, serde::Serialize)]
pub struct Config {
  /// Additional Chrome user-data directories checked after the standard locations.
  #[serde(default)]
  pub chrome_user_data_dirs: Vec<PathBuf>,
  /// Host used when a request contains only an absolute path.
  #[serde(default)]
  pub default_canvas_host: Option<String>,
}

impl Config {
  /// Serializes the configuration as TOML.
  ///
  /// # Errors
  ///
  /// Returns an error when the configuration cannot be represented as TOML.
  pub fn serialize(&self) -> Result<String> {
    Ok(toml::to_string_pretty(self)?)
  }

  /// Loads the user's configuration, or returns the default configuration when it does not exist.
  ///
  /// # Errors
  ///
  /// Returns an error when the configuration file cannot be read or parsed.
  pub fn load() -> Result<Self> {
    let Some(path) = config_path() else {
      return Ok(Self::default());
    };

    match fs::read_to_string(&path) {
      Ok(contents) => toml::from_str(&contents)
        .with_context(|| format!("could not parse configuration file {}", path.display())),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
      Err(error) => {
        Err(error).with_context(|| format!("could not read configuration file {}", path.display()))
      }
    }
  }
}

/// Returns the path to the user's configuration file when a config directory is available.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
  dirs::config_dir().map(|directory| directory.join(CONFIG_DIRECTORY).join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::Config;

  #[test]
  fn config_contains_supported_values_and_ignores_future_values() {
    let config: Config = toml::from_str(
      r#"
        chrome_user_data_dirs = ["/first/profile", "/second/profile"]
        default_canvas_host = "https://canvas.example"
        future_setting = true
      "#,
    )
    .unwrap();

    assert_eq!(
      config.chrome_user_data_dirs,
      vec![
        PathBuf::from("/first/profile"),
        PathBuf::from("/second/profile")
      ]
    );
    assert_eq!(
      config.default_canvas_host.as_deref(),
      Some("https://canvas.example")
    );
  }
}
