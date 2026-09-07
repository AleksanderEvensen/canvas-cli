// Generated code follows Prost/Tonic conventions rather than workspace style lints.
#[allow(clippy::all, clippy::pedantic, clippy::nursery, clippy::as_conversions)]
pub mod v1 {
  tonic::include_proto!("cnvs.daemon.v1");
}
pub use v1::*;
pub const PROTOCOL_MAJOR: u32 = 1;
pub const MESSAGE_LIMIT: usize = 16 * 1024 * 1024;
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub const WORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
pub const API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(320);
