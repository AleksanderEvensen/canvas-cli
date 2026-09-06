---
description: List upcoming Canvas assignments with submission status and scores.
---
# Canvas assignments

Use the single grouped command instead of manually joining course, assignment,
and submission API calls:

```sh
cnvs assignments list
cnvs assignments list --past
cnvs assignments list --hide-submitted
cnvs assignments list --course COURSE_ID --json
```

The default output includes all upcoming assignments, whether submitted or
not. `--past` also includes assignments with past deadlines. `--hide-submitted`
removes assignments with a submitted or graded submission. Use `--course`
with a Canvas course ID or course code to limit results.

These commands discover the signed-in user and include only courses where the
user has a `student` enrollment. Teaching-assistant courses are excluded.
Unpublished, suppressed, and assignments locked for the current user are also
excluded.

Use `--json` when you need structured records; it emits one valid JSON array.
Dates in the human-readable table are labeled UTC. JSON keeps Canvas's original
ISO-8601 values in `dueAt`, `submittedAt`, and `gradedAt`.

The current query supports up to 100 assignments per selected student course.
If more exist, the command fails instead of printing incomplete results.
