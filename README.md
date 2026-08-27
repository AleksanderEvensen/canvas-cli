# Canvas CLI (cnvs)

`cnvs` makes HTTP requests through a signed-in Chromium browser session. It keeps one Chrome DevTools Protocol connection in a background daemon, so Chrome asks for remote-debugging approval only once per daemon session.

## Install

```sh
cargo install --git https://github.com/AleksanderEvensen/canvas-cli.git cnvs
# Or, from a local checkout:
cargo install --path crates/cnvs
```

## Browser setup

Open `chrome://inspect/#remote-debugging` and enable Remote Debugging. `cnvs` searches active Chrome, Chrome Beta, Chromium, Brave, Edge, and Helium profiles in that order.

Choose a profile explicitly when needed:

```sh
cnvs daemon start --chrome-user-data-dir '/path/to/browser/user-data'
```

`CNVS_CHROME_USER_DATA_DIR` provides the same setting. One daemon uses one profile until stopped.

## Requests

The daemon starts automatically if not already running (user will be asked to approve it to use the CDP protocol).

```sh
cnvs api GET https://canvas.example/api/v1/users/self
cnvs api POST https://canvas.example/api/v1/items \
  --query notify=true \
  --header 'content-type: application/json' \
  --body '{"name":"Example"}'
cnvs api PUT https://canvas.example/upload --body-file payload.json
cnvs api GET https://canvas.example/files/123/download --output submission.zip
```

`--query key=value` and `--header 'Name: value'` can be repeated.

Use `--body-file -` to read a body from stdin. Any Fetch-compatible HTTP method and HTTP(S) URL may be used.

GraphQL accepts a full endpoint URL. The query comes from stdin unless `--file` is provided:

```sh
printf 'mutation { updateThing { id } }' | \
  cnvs gql --url https://canvas.example/api/graphql

cnvs gql \
  --url https://canvas.example/api/graphql \
  --file viewer.graphql \
  --variables-file variables.json
```

`--variables` accepts an inline JSON object. GraphQL mutations are allowed.

Successful text response bodies are written unchanged to stdout. HTTP 4xx/5xx bodies are also written to stdout, but `cnvs` exits nonzero. Use `--verbose` to print the HTTP status and daemon auto-start notices to stderr.

Use `--output PATH` for binary downloads. Downloads use Chrome's network protocol instead of page `fetch`, so cross-origin redirects are allowed. Download mode supports GET requests without custom headers or a request body.

## Daemon commands

```sh
cnvs daemon start
cnvs daemon status
cnvs daemon stop
```

`start` and `stop` are idempotent. `status` reports the state, PID, and browser profile; it exits nonzero when stopped. Startup waits up to five minutes for Chrome approval. If that wait expires, the pending daemon remains available for later approval or `cnvs daemon stop`.

## Canvas endpoint documentation

- REST API index: <https://canvas.instructure.com/doc/api/>
- Current Canvas developer docs: <https://developerdocs.instructure.com/services/canvas/resources>
- Courses endpoints: <https://developerdocs.instructure.com/services/canvas/resources/courses>
- Groups endpoints: <https://developerdocs.instructure.com/services/canvas/resources/groups>
- Submissions endpoints: <https://developerdocs.instructure.com/services/canvas/resources/submissions>
- GraphQL API: <https://canvas.instructure.com/doc/api/file.graphql.html>
