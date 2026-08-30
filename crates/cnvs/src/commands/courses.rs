use anyhow::{anyhow, bail, Result};
use clap::Subcommand;

use super::api;

#[derive(Subcommand)]
pub enum CoursesCommand {
  List,

  Get {
    #[arg(value_name = "COURSE_ID")]
    course_id: String,
  },
}

pub(crate) fn run(command: CoursesCommand, verbose: bool) -> Result<i32> {
  match command {
    CoursesCommand::List => api::get("/api/v1/courses", verbose),
    CoursesCommand::Get { course_id } => {
      if course_id.is_empty() {
        bail!("course ID cannot be empty");
      }

      let mut url = api::resolve_url("/api/v1/courses")?;
      url
        .path_segments_mut()
        .map_err(|()| anyhow!("could not append course ID to URL"))?
        .push(&course_id);
      api::get_url(url, verbose)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rejects_empty_course_ids() {
    assert!(run(
      CoursesCommand::Get {
        course_id: String::new()
      },
      false
    )
    .is_err());
  }
}
