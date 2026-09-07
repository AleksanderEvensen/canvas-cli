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
