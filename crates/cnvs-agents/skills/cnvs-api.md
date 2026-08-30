---
name: cnvs-api
description: Make Canvas API requests through the signed-in Chromium session.
---

Use `cnvs api METHOD URL`.

Use a full `https://` URL. Use an absolute path only when `default_canvas_host` is set.

Repeat `--query key=value` for query parameters.
Repeat `--header 'Name: value'` for headers.
Use `--body-file PATH` for a request body.
Use `--output PATH` for downloads.
