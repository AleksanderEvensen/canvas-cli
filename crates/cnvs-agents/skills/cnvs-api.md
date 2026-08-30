---
description: Make Canvas API requests through the signed-in Chromium session.
---

Use `cnvs api METHOD URL`.

<% if self.ctx.config.default_canvas_host.is_some() { %>
  Use a full `https://` URL, or an absolute path because `default_canvas_host` is configured.<% } else { %>Use a full `https://` URL. An absolute path requires `default_canvas_host` to be configured.
<% } %>

Repeat `--query key=value` for query parameters.
Repeat `--header 'Name: value'` for headers.

Canvas LMS API documentation:

- REST API index: <https://canvas.instructure.com/doc/api/>
- Canvas developer resources: <https://developerdocs.instructure.com/services/canvas/resources>
- Courses: <https://developerdocs.instructure.com/services/canvas/resources/courses>
- Groups: <https://developerdocs.instructure.com/services/canvas/resources/groups>
- Submissions: <https://developerdocs.instructure.com/services/canvas/resources/submissions>
- GraphQL API: <https://canvas.instructure.com/doc/api/file.graphql.html>
Use `--body-file PATH` for a request body.
Use `--output PATH` for downloads.

<% if !self.ctx.read_only_actions { %>Write requests are available in this binary.<% } %>
