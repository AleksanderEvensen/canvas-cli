---
description: Use the cnvs CLI to make requests through a signed-in Chromium session.
---
# cnvs CLI

Use `cnvs` to make HTTP requests through a signed-in Chromium, Chrome, Brave, Edge, Vivaldi, or Helium session. The CLI uses a background daemon for the browser connection.

## Before making requests

Enable Remote Debugging in `chrome://inspect/#remote-debugging`. Start the daemon when needed:

```sh
cnvs daemon start
```

The daemon starts automatically for API requests. Use `cnvs daemon status` to inspect it and `cnvs daemon stop` to stop it.

## Configuration

The configuration file is `~/.config/cnvs/config.toml`:

```toml
chrome_user_data_dirs = ["/path/to/browser/user-data"]
default_canvas_host = "https://canvas.example"
```

Use `cnvs config info` to inspect configuration and `cnvs config edit` to edit it. The `CNVS_CHROME_USER_DATA_DIR` environment variable selects a browser profile. An explicit `--chrome-user-data-dir` takes precedence over the environment variable.

<% if self.ctx.config.default_canvas_host.is_some() { %>Because `default_canvas_host` is configured, API requests may use an absolute path such as `/api/v1/users/self`.<% } else { %>Use full `http://` or `https://` URLs for API requests. Absolute paths require `default_canvas_host` in the configuration.<% } %>

## Canvas resources

Common read operations have grouped commands. They use the configured
`default_canvas_host`:

```sh
cnvs users me
cnvs courses list
cnvs courses get COURSE_ID
cnvs assignments list
cnvs assignments list --past
cnvs assignments list --hide-submitted
```

## API requests

```sh
cnvs api GET https://canvas.example/api/v1/users/self
cnvs api GET https://canvas.example/files/123/download --output submission.zip
```

Repeat `--query key=value` and `--header 'Name: value'` as needed. Use `--body-file PATH` for a request body and `--output PATH` for binary downloads. Successful response bodies are written to stdout; HTTP errors also go to stdout and cause a nonzero exit status. Add `--verbose` for status and daemon messages.

<% if self.ctx.read_only_actions { %>
This binary is read-only: only `GET` requests are available.
<% } else { %>
This binary includes write requests (`POST`, `PUT`, and `DELETE`). It also supports GraphQL with `cnvs gql`.
<% } %>

## Agent skills

List available skills or print one skill:

```sh
cnvs agent skills
cnvs agent skills cnvs-api
```
