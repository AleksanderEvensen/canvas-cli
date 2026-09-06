use serde::{Deserialize, Serialize};

/// The only GraphQL operation allowed through the read-only RPC path.
/// Shared so the daemon can validate the exact query, not trust the client's request tag.
pub const ASSIGNMENTS_QUERY: &str = r"
query CnvsAssignments($userId: ID!) {
  user(id: $userId) {
    enrollments(currentOnly: true) {
      type
      course {
        _id
      }
    }
  }
  allCourses {
    _id
    courseCode
    name
    assignmentsConnection(first: 100) {
      pageInfo {
        hasNextPage
      }
      nodes {
        _id
        name
        dueAt
        pointsPossible
        published
        state
        suppressAssignment
        lockInfo {
          isLocked
        }
        submissionsConnection(
          first: 1
          filter: { userId: $userId, includeUnsubmitted: true }
        ) {
          nodes {
            state
            submittedAt
            score
            grade
            gradedAt
          }
        }
      }
    }
  }
}
";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
  Status,
  Stop,
  Api(ApiRequest),
  /// A read-only GraphQL request issued by a vetted built-in command.
  Graphql(GraphqlRequest),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiRequest {
  pub method: String,
  pub url: String,
  pub headers: Vec<Header>,
  pub body: Option<String>,
  #[serde(default)]
  pub download: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GraphqlRequest {
  pub url: String,
  pub headers: Vec<Header>,
  pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
  pub name: String,
  pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
  Starting,
  Running,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
  Status {
    state: DaemonState,
    pid: u32,
    profile: String,
  },
  Stopped,
  Api {
    status: u16,
    status_text: String,
    body: ResponseBody,
  },
  Error {
    message: String,
  },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ResponseBody {
  Text(String),
  Base64(String),
  File(std::path::PathBuf),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn request_round_trips() {
    let json = serde_json::to_string(&Request::Api(ApiRequest {
      method: "POST".into(),
      url: "https://canvas.example/api".into(),
      headers: vec![],
      body: Some("hello\nworld".into()),
      download: false,
    }))
    .unwrap();

    let Request::Api(request) = serde_json::from_str(&json).unwrap() else {
      panic!("wrong request variant");
    };
    assert_eq!(request.body.as_deref(), Some("hello\nworld"));
    assert!(!request.download);
  }

  #[test]
  fn vetted_graphql_request_round_trips() {
    let json = serde_json::to_string(&Request::Graphql(GraphqlRequest {
      url: "https://canvas.example/api/graphql".into(),
      headers: vec![],
      body: "{ allCourses { _id } }".into(),
    }))
    .unwrap();

    let Request::Graphql(request) = serde_json::from_str(&json).unwrap() else {
      panic!("wrong request variant");
    };
    assert_eq!(request.url, "https://canvas.example/api/graphql");
    assert_eq!(request.body, "{ allCourses { _id } }");
  }
}
