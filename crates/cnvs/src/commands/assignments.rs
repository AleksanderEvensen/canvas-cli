use std::{cmp::Ordering, collections::HashSet, time::SystemTime};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use cnvs_protocol::ASSIGNMENTS_QUERY;
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::utilities::{ApiRequestBuilder, RequestBody};

use super::api;

#[derive(Subcommand)]
pub enum AssignmentsCommand {
  #[command(about = "List upcoming assignments and their submission status")]
  List {
    #[arg(
      long,
      value_name = "COURSE_ID",
      help = "Limit results to one course ID or course code"
    )]
    course: Option<String>,
    #[arg(long, help = "Also include assignments whose deadline has passed")]
    past: bool,
    #[arg(long, help = "Hide assignments with a submitted or graded submission")]
    hide_submitted: bool,
    #[arg(long, help = "Output all records as one valid JSON array")]
    json: bool,
  },
}

pub(crate) fn run(command: AssignmentsCommand, verbose: bool) -> Result<i32> {
  match command {
    AssignmentsCommand::List {
      course,
      past,
      hide_submitted,
      json,
    } => {
      let mut records = fetch_records(course.as_deref(), verbose)?;
      let now = current_timestamp()?;
      if !past {
        records.retain(|record| record.due_date().is_some_and(|due| due > now));
      }
      if hide_submitted {
        records.retain(|record| record.status == SubmissionStatus::NotSubmitted);
      }
      render(records, json)
    }
  }
}

#[derive(Debug)]
struct Course {
  id: u64,
  name: String,
  code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphqlData {
  user: Option<GraphqlUser>,
  #[serde(rename = "allCourses")]
  all_courses: Option<Vec<Option<GraphqlCourse>>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlUser {
  #[serde(default)]
  enrollments: Vec<GraphqlEnrollment>,
}

#[derive(Debug, Deserialize)]
struct GraphqlEnrollment {
  #[serde(rename = "type")]
  enrollment_type: String,
  course: Option<GraphqlCourseReference>,
}

#[derive(Debug, Deserialize)]
struct GraphqlCourseReference {
  #[serde(rename = "_id")]
  id: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlCourse {
  #[serde(rename = "_id")]
  id: String,
  #[serde(rename = "courseCode")]
  code: Option<String>,
  name: String,
  #[serde(rename = "assignmentsConnection")]
  assignments: Option<GraphqlAssignmentConnection>,
}

#[derive(Debug, Deserialize)]
struct GraphqlAssignmentConnection {
  nodes: Option<Vec<Option<GraphqlAssignment>>>,
  #[serde(rename = "pageInfo")]
  page_info: GraphqlPageInfo,
}

#[derive(Debug, Deserialize)]
struct GraphqlPageInfo {
  #[serde(rename = "hasNextPage")]
  has_next_page: bool,
}

#[derive(Debug, Deserialize)]
struct GraphqlAssignment {
  #[serde(rename = "_id")]
  id: String,
  name: Option<String>,
  #[serde(rename = "dueAt")]
  due_at: Option<String>,
  #[serde(rename = "pointsPossible")]
  points_possible: Option<f64>,
  published: Option<bool>,
  state: Option<String>,
  #[serde(rename = "suppressAssignment")]
  suppressed: Option<bool>,
  #[serde(rename = "lockInfo")]
  lock_info: Option<GraphqlLockInfo>,
  #[serde(rename = "submissionsConnection")]
  submission: Option<GraphqlSubmissionConnection>,
}

#[derive(Debug, Deserialize)]
struct GraphqlLockInfo {
  #[serde(rename = "isLocked")]
  is_locked: bool,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubmissionConnection {
  nodes: Option<Vec<Option<GraphqlSubmission>>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubmission {
  state: Option<String>,
  #[serde(rename = "submittedAt")]
  submitted_at: Option<String>,
  score: Option<f64>,
  grade: Option<String>,
  #[serde(rename = "gradedAt")]
  graded_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SubmissionStatus {
  #[serde(rename = "not submitted")]
  NotSubmitted,
  Submitted,
  Graded,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssignmentRecord {
  course: String,
  course_id: u64,
  assignment: String,
  assignment_id: u64,
  due_at: Option<String>,
  points_possible: Option<f64>,
  status: SubmissionStatus,
  submitted_at: Option<String>,
  score: Option<f64>,
  grade: Option<String>,
  graded_at: Option<String>,
}

impl AssignmentRecord {
  fn due_date(&self) -> Option<Timestamp> {
    self.due_at.as_deref().and_then(|value| value.parse().ok())
  }
}

fn fetch_records(course_filter: Option<&str>, verbose: bool) -> Result<Vec<AssignmentRecord>> {
  let user = api::get_json_url(api::resolve_url("/api/v1/users/self")?, verbose)?;
  let user_id = extract_user_id(&user)?;
  let response = fetch_graphql(&user_id, verbose)?;
  let data = parse_graphql_data(&response)?;
  let user = data
    .user
    .context("Canvas GraphQL returned no current user")?;
  let student_course_ids = user
    .enrollments
    .iter()
    .filter(|enrollment| is_student_enrollment(enrollment))
    .filter_map(|enrollment| enrollment.course.as_ref())
    .filter_map(|course| course.id.parse::<u64>().ok())
    .collect::<HashSet<_>>();

  let mut records = Vec::new();
  for graphql_course in data.all_courses.unwrap_or_default().into_iter().flatten() {
    let course = normalize_course(&graphql_course)?;
    if !student_course_ids.contains(&course.id) || !course_matches(&course, course_filter) {
      continue;
    }

    let Some(connection) = graphql_course.assignments else {
      continue;
    };
    ensure_complete(&connection, &course)?;
    for assignment in connection
      .nodes
      .into_iter()
      .flatten()
      .flatten()
      .filter(is_visible)
    {
      if let Some(record) = normalize(&course, assignment) {
        records.push(record);
      }
    }
  }

  records.sort_by(compare_records);
  Ok(records)
}

fn ensure_complete(connection: &GraphqlAssignmentConnection, course: &Course) -> Result<()> {
  // SIMPLIFIED: fetch one page per course; add cursor pagination before supporting larger courses.
  if connection.page_info.has_next_page {
    bail!(
      "course {} has more than 100 assignments; pagination is not yet supported",
      course_label(course)
    );
  }
  Ok(())
}

fn fetch_graphql(user_id: &str, verbose: bool) -> Result<Value> {
  let mut request = ApiRequestBuilder::post(api::resolve_url("/api/graphql")?);
  request
    .enable_verbose(verbose)
    .allow_read_only_graphql()
    .body(RequestBody::json(json!({
      "query": ASSIGNMENTS_QUERY,
      "variables": { "userId": user_id },
    }))?);
  api::json_response(request.json()?, verbose)
}

fn parse_graphql_data(value: &Value) -> Result<GraphqlData> {
  if value
    .get("errors")
    .and_then(Value::as_array)
    .is_some_and(|errors| !errors.is_empty())
  {
    let errors = value.get("errors").context("GraphQL errors were missing")?;
    bail!("Canvas GraphQL query failed: {errors}");
  }
  let data = value
    .get("data")
    .cloned()
    .context("Canvas GraphQL response had no data")?;
  serde_json::from_value(data).context("Canvas returned invalid assignment data")
}

fn extract_user_id(value: &Value) -> Result<String> {
  value
    .get("id")
    .and_then(Value::as_u64)
    .map(|id| id.to_string())
    .or_else(|| value.get("id").and_then(Value::as_str).map(str::to_owned))
    .context("Canvas users/self response had no user ID")
}

fn normalize_course(course: &GraphqlCourse) -> Result<Course> {
  Ok(Course {
    id: course
      .id
      .parse()
      .with_context(|| format!("invalid Canvas course ID {}", course.id))?,
    name: course.name.clone(),
    code: course.code.clone(),
  })
}

fn course_matches(course: &Course, filter: Option<&str>) -> bool {
  filter.is_none_or(|filter| {
    course.id.to_string() == filter
      || course.code.as_deref() == Some(filter)
      || course.name == filter
  })
}

fn is_student_enrollment(enrollment: &GraphqlEnrollment) -> bool {
  enrollment.enrollment_type.eq_ignore_ascii_case("student")
    || enrollment
      .enrollment_type
      .eq_ignore_ascii_case("studentenrollment")
}

fn is_visible(assignment: &GraphqlAssignment) -> bool {
  assignment.published != Some(false)
    && !matches!(assignment.state.as_deref(), Some("unpublished" | "deleted"))
    && assignment.suppressed != Some(true)
    && assignment
      .lock_info
      .as_ref()
      .is_none_or(|lock_info| !lock_info.is_locked)
}

fn normalize(course: &Course, assignment: GraphqlAssignment) -> Option<AssignmentRecord> {
  let submission = assignment
    .submission
    .and_then(|connection| connection.nodes)
    .and_then(|nodes| nodes.into_iter().flatten().next());
  let status = submission
    .as_ref()
    .map_or(SubmissionStatus::NotSubmitted, submission_status);
  Some(AssignmentRecord {
    course: course_label(course),
    course_id: course.id,
    assignment: assignment.name?,
    assignment_id: assignment.id.parse().ok()?,
    due_at: assignment.due_at,
    points_possible: assignment.points_possible,
    status,
    submitted_at: submission
      .as_ref()
      .and_then(|value| value.submitted_at.clone()),
    score: submission.as_ref().and_then(|value| value.score),
    grade: submission.as_ref().and_then(|value| value.grade.clone()),
    graded_at: submission
      .as_ref()
      .and_then(|value| value.graded_at.clone()),
  })
}

fn submission_status(submission: &GraphqlSubmission) -> SubmissionStatus {
  if submission.state.as_deref() == Some("unsubmitted") {
    return SubmissionStatus::NotSubmitted;
  }
  if submission.state.as_deref() == Some("graded")
    || submission.score.is_some()
    || submission.grade.is_some()
    || submission.graded_at.is_some()
  {
    SubmissionStatus::Graded
  } else if matches!(
    submission.state.as_deref(),
    Some("submitted" | "pending_review")
  ) || submission.submitted_at.is_some()
  {
    SubmissionStatus::Submitted
  } else {
    SubmissionStatus::NotSubmitted
  }
}

fn course_label(course: &Course) -> String {
  course.code.clone().unwrap_or_else(|| course.name.clone())
}

fn compare_records(left: &AssignmentRecord, right: &AssignmentRecord) -> Ordering {
  match (left.due_date(), right.due_date()) {
    (Some(left), Some(right)) => left.cmp(&right),
    (Some(_), None) => Ordering::Less,
    (None, Some(_)) => Ordering::Greater,
    (None, None) => left.due_at.cmp(&right.due_at),
  }
  .then_with(|| left.course.cmp(&right.course))
  .then_with(|| left.assignment.cmp(&right.assignment))
}

fn render(mut records: Vec<AssignmentRecord>, json: bool) -> Result<i32> {
  records.sort_by(compare_records);
  if json {
    println!("{}", serde_json::to_string(&records)?);
    return Ok(0);
  }

  let mut table = Table::new();
  table.load_preset(UTF8_FULL);
  table.set_content_arrangement(ContentArrangement::Dynamic);
  table.set_header(["COURSE", "ASSIGNMENT", "DUE", "POINTS", "STATUS", "SCORE"]);
  for record in records {
    let due = record
      .due_at
      .as_deref()
      .map_or_else(|| "—".to_owned(), format_due);
    table.add_row([
      clean(&record.course),
      clean(&record.assignment),
      due,
      record
        .points_possible
        .map_or_else(|| "—".to_owned(), |points| points.to_string()),
      status_label(record.status).to_owned(),
      score_label(&record),
    ]);
  }
  println!("{table}");
  Ok(0)
}

fn format_due(value: &str) -> String {
  value.parse::<Timestamp>().map_or_else(
    |_| value.to_owned(),
    |date| format!("{} UTC", date.strftime("%Y-%m-%d %H:%M")),
  )
}

fn current_timestamp() -> Result<Timestamp> {
  Timestamp::try_from(SystemTime::now()).context("system clock is outside Jiff's supported range")
}

const fn status_label(status: SubmissionStatus) -> &'static str {
  match status {
    SubmissionStatus::NotSubmitted => "not submitted",
    SubmissionStatus::Submitted => "submitted",
    SubmissionStatus::Graded => "graded",
  }
}

fn score_label(record: &AssignmentRecord) -> String {
  if let Some(score) = record.score {
    return record
      .points_possible
      .map_or_else(|| score.to_string(), |points| format!("{score}/{points}"));
  }
  if let Some(grade) = &record.grade {
    return grade.clone();
  }
  if record.status == SubmissionStatus::Submitted {
    "not graded".to_owned()
  } else {
    "—".to_owned()
  }
}

fn clean(value: &str) -> String {
  value.replace(['\n', '\r', '\t'], " ")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn submission(state: &str, score: Option<f64>) -> GraphqlSubmission {
    GraphqlSubmission {
      state: Some(state.to_owned()),
      submitted_at: None,
      score,
      grade: None,
      graded_at: None,
    }
  }

  fn assignment() -> GraphqlAssignment {
    GraphqlAssignment {
      id: "1".to_owned(),
      name: Some("Test".to_owned()),
      due_at: None,
      points_possible: Some(1.0),
      published: Some(true),
      state: Some("published".to_owned()),
      suppressed: Some(false),
      lock_info: Some(GraphqlLockInfo { is_locked: false }),
      submission: None,
    }
  }

  #[test]
  fn interprets_submission_states() {
    assert_eq!(
      submission_status(&submission("unsubmitted", None)),
      SubmissionStatus::NotSubmitted
    );
    assert_eq!(
      submission_status(&submission("submitted", None)),
      SubmissionStatus::Submitted
    );
    assert_eq!(
      submission_status(&submission("submitted", Some(1.0))),
      SubmissionStatus::Graded
    );
    assert_eq!(
      submission_status(&submission("ungraded", None)),
      SubmissionStatus::NotSubmitted
    );
    assert_eq!(
      submission_status(&submission("pending_review", None)),
      SubmissionStatus::Submitted
    );
    let mut pending = submission("pending_review", None);
    pending.submitted_at = Some("2026-01-01T00:00:00Z".into());
    assert_eq!(submission_status(&pending), SubmissionStatus::Submitted);
  }

  #[test]
  fn rejects_incomplete_assignment_pages() {
    let course = Course {
      id: 1,
      name: "Test".into(),
      code: None,
    };
    let mut connection = GraphqlAssignmentConnection {
      nodes: Some(vec![]),
      page_info: GraphqlPageInfo {
        has_next_page: true,
      },
    };
    assert!(ensure_complete(&connection, &course).is_err());
    connection.page_info.has_next_page = false;
    assert!(ensure_complete(&connection, &course).is_ok());
  }

  #[test]
  fn hides_assignments_not_available_to_students() {
    assert!(!is_visible(&GraphqlAssignment {
      published: Some(false),
      ..assignment()
    }));
    assert!(!is_visible(&GraphqlAssignment {
      suppressed: Some(true),
      ..assignment()
    }));
    assert!(!is_visible(&GraphqlAssignment {
      lock_info: Some(GraphqlLockInfo { is_locked: true }),
      ..assignment()
    }));
    assert!(is_visible(&assignment()));
  }
}
