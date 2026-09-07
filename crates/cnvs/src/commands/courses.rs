use anyhow::{anyhow, bail, Result};
use clap::Subcommand;

use super::api;

#[derive(Subcommand)]
pub enum CoursesCommand {
  #[command(about = "List courses available to the signed-in user")]
  List,

  #[command(about = "Fetch one course by ID")]
  Get {
    #[arg(value_name = "COURSE_ID", help = "Canvas course ID")]
    course_id: String,
  },
}

pub async fn run(command: CoursesCommand, verbose: bool) -> Result<i32> {
  match command {
    CoursesCommand::List => api::get("/api/v1/courses", verbose).await,
    CoursesCommand::Get { course_id } => {
      if course_id.is_empty() {
        bail!("course ID cannot be empty");
      }

      let mut url = api::resolve_url("/api/v1/courses")?;
      url
        .path_segments_mut()
        .map_err(|()| anyhow!("could not append course ID to URL"))?
        .push(&course_id);
      api::get_url(url, verbose).await
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn rejects_empty_course_ids() {
    assert!(run(
      CoursesCommand::Get {
        course_id: String::new()
      },
      false
    )
    .await
    .is_err());
  }
}
