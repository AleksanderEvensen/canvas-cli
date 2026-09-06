# Canvas CLI (cnvs)

`cnvs` makes HTTP requests through a signed-in Chromium browser session. It keeps one Chrome DevTools Protocol connection in a background daemon, so Chrome asks for remote-debugging approval only once per daemon session.

## Install

```sh
cargo install --git https://github.com/AleksanderEvensen/canvas-cli.git cnvs
# Or, from a local checkout:
cargo install --path crates/cnvs

# To include POST, PUT, DELETE, and GraphQL support:
cargo install --path crates/cnvs --features write-requests
# or
cargo install --git https://github.com/AleksanderEvensen/canvas-cli.git cnvs --features write-requests
```

## Browser setup

Open `chrome://inspect/#remote-debugging` and enable Remote Debugging. `cnvs` searches active stable Chrome, Chromium, Brave, Edge, Vivaldi, and Helium profiles in that order.

Choose a profile explicitly when needed:

```sh
cnvs daemon start --chrome-user-data-dir '/path/to/browser/user-data'
```

`CNVS_CHROME_USER_DATA_DIR` provides the same setting. When no profile is specified, `cnvs` checks paths listed in `~/.config/cnvs/config.toml` before the standard browser locations:

```toml
chrome_user_data_dirs = [
  "/path/to/browser/user-data",
  "/another/browser/user-data",
]
default_canvas_host = "https://canvas.ntnu.no"
```

Inspect or edit the configuration with the configured editor (`$EDITOR`):

```sh
cnvs config info
cnvs config edit
```

With `default_canvas_host` configured, API paths can omit the host:

```sh
cnvs api GET /api/v1/users/self
```

Common Canvas resources also have grouped commands:

```sh
cnvs users me
cnvs courses list
cnvs courses get COURSE_ID
cnvs assignments list
cnvs assignments list --past
cnvs assignments list --hide-submitted
```

Assignment listing uses a fixed, read-only GraphQL query and works without
`write-requests`. It currently supports up to 100 assignments per selected
student course and fails explicitly if pagination would be needed.

A full `http://` or `https://` URL supplied on the command line always uses its specified host. Relative paths require `default_canvas_host`.

The command-line option takes precedence over the environment variable. The environment variable takes precedence over automatic discovery. One daemon uses one profile until stopped.

## Agent skills

The binary includes skills that can be provided to AI agents:

```sh
cnvs agent skills
cnvs agent skills cnvs-api
```

The first command lists skill slugs and their frontmatter. The second prints the
full skill contents.

## Requests

The daemon starts automatically if not already running (user will be asked to approve it to use the CDP protocol).

```sh
cnvs api GET https://canvas.example/api/v1/users/self
cnvs api GET https://canvas.example/files/123/download --output submission.zip
```

`--query key=value` and `--header 'Name: value'` can be repeated.

Use `--body-file -` to read a body from stdin. The default build only includes
`GET` requests. To include write requests (`POST`, `PUT`, and `DELETE`), install
with `--features write-requests`.

With the `write-requests` feature enabled, GraphQL accepts a full endpoint URL. The query comes from stdin unless `--file` is provided:

```sh
printf 'mutation { updateThing { id } }' | \
  cnvs gql --url https://canvas.example/api/graphql

cnvs gql \
  --url https://canvas.example/api/graphql \
  --file viewer.graphql \
  --variables-file variables.json
```

`--variables` accepts an inline JSON object.

Successful text response bodies are written unchanged to stdout. HTTP 4xx/5xx bodies are also written to stdout, but `cnvs` exits nonzero. Use `--verbose` to print the HTTP status and daemon auto-start notices to stderr.

Use `--output PATH` for binary downloads. Downloads use Chrome's network protocol instead of page `fetch`, so cross-origin redirects are allowed. Download mode supports GET requests without custom headers or a request body.

## Daemon commands

```sh
cnvs daemon start
cnvs daemon status
cnvs daemon stop
```

`start` and `stop` are idempotent. `status` reports the state, PID, and browser profile; it exits nonzero when stopped. Startup waits up to one minute for Chrome approval. If that wait expires, the pending daemon remains available for later approval or `cnvs daemon stop`.

## Canvas endpoint documentation

- REST API index: <https://canvas.instructure.com/doc/api/>
- Current Canvas developer docs: <https://developerdocs.instructure.com/services/canvas/resources>
- Courses endpoints: <https://developerdocs.instructure.com/services/canvas/resources/courses>
- Groups endpoints: <https://developerdocs.instructure.com/services/canvas/resources/groups>
- Submissions endpoints: <https://developerdocs.instructure.com/services/canvas/resources/submissions>
- GraphQL API: <https://canvas.instructure.com/doc/api/file.graphql.html>
