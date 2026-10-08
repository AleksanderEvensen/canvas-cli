# Canvas CLI

Use `target/release/cnvs` to make read-only Canvas requests through the signed-in Helium/Chrome session. No API token is needed.

```sh
# Current user
./target/release/cnvs api GET https://canvas.ntnu.no/api/v1/users/self

# Active course enrolments
./target/release/cnvs api GET https://canvas.ntnu.no/api/v1/courses \
  --query enrollment_state=active --query per_page=100

# A course's assignments
./target/release/cnvs api GET https://canvas.ntnu.no/api/v1/courses/COURSE_ID/assignments
```

The daemon uses Chrome DevTools Protocol and the browser's session cookies. Enable Remote Debugging and sign in to Canvas in Helium first; use only data the user is authorized to access. The default build permits only `GET` through `api`, including download URLs outside `/api/`.

For read-only assignment listing, use `./target/release/cnvs assignments list` with `default_canvas_host` configured. The raw `gql` command is available only in builds with the `write-requests` feature; it accepts queries and mutations and requires a full `/api/graphql` endpoint URL or a configured default host.

## Check a student group's submission status

Canvas has no REST equivalent of the Gradebook's **Student Groups** filter. Get the group's members, then pass their Canvas IDs as repeated `student_ids[]` parameters to the submissions endpoint.

```sh
# Discover the group and its members.
./target/release/cnvs api GET https://canvas.ntnu.no/api/v1/courses/COURSE_ID/group_categories --query per_page=100
./target/release/cnvs api GET https://canvas.ntnu.no/api/v1/group_categories/CATEGORY_ID/groups --query per_page=100
./target/release/cnvs api GET https://canvas.ntnu.no/api/v1/groups/GROUP_ID/users --query per_page=100

# Get one assignment's status for a group member. Repeat student_ids[] for every member.
./target/release/cnvs api GET https://canvas.ntnu.no/api/v1/courses/COURSE_ID/students/submissions \
  --query 'assignment_ids[]=ASSIGNMENT_ID' --query 'student_ids[]=STUDENT_ID' --query per_page=100
```

Interpret `workflow_state`: `unsubmitted` means no submission; `submitted` means submitted and pending grading; `graded` means graded. Omit `workflow_state` to receive all three. `needs_grading_count` on an assignment is course-wide, not group-filtered.

Only use read-only operations for this workflow. The default build blocks REST writes and raw GraphQL; a `write-requests` build does not.

## Canvas endpoint documentation

- REST API index: <https://canvas.instructure.com/doc/api/>
- Current Canvas developer docs: <https://developerdocs.instructure.com/services/canvas/resources>
- Courses endpoints: <https://developerdocs.instructure.com/services/canvas/resources/courses>
- Groups endpoints: <https://developerdocs.instructure.com/services/canvas/resources/groups>
- Submissions endpoints: <https://developerdocs.instructure.com/services/canvas/resources/submissions>
- GraphQL API: <https://canvas.instructure.com/doc/api/file.graphql.html>
