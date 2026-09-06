use std::{collections::HashSet, fs, path::PathBuf, time::Duration};

use anyhow::{bail, Context, Result};
use cnvs_protocol::{ApiRequest, GraphqlRequest, ResponseBody, ASSIGNMENTS_QUERY};
use serde_json::{json, Value};
use tokio::time::{timeout, Instant};
use url::Url;

use crate::cdp::ChromeDeveloperProtocol;

fn download_directory() -> anyhow::Result<PathBuf> {
  let base = std::env::temp_dir();
  for attempt in 0..100u32 {
    let path = base.join(format!("cnvs-download-{}-{attempt}", std::process::id()));
    match fs::create_dir(&path) {
      Ok(()) => return Ok(path),
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
      Err(error) => return Err(error.into()),
    }
  }
  anyhow::bail!("could not create a unique download directory")
}

const REQUEST_TIMEOUT: Duration = Duration::from_mins(5);

pub struct HttpResponse {
  pub(crate) status: u16,
  pub(crate) status_text: String,
  pub(crate) body: ResponseBody,
}

fn validate_read_only_graphql(request: &GraphqlRequest) -> Result<()> {
  let url = Url::parse(&request.url).context("invalid GraphQL URL")?;
  origin(&url)?;
  if url.path() != "/api/graphql" || url.query().is_some() || url.fragment().is_some() {
    bail!(
      "read-only GraphQL requires the /api/graphql endpoint without query parameters or a fragment"
    );
  }
  let body: Value = serde_json::from_str(&request.body).context("invalid GraphQL body")?;
  let user_id = body
    .pointer("/variables/userId")
    .and_then(Value::as_str)
    .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
    .context("read-only GraphQL requires a numeric userId string")?;
  if body != json!({ "query": ASSIGNMENTS_QUERY, "variables": { "userId": user_id } }) {
    bail!("only the built-in assignments query is permitted through read-only GraphQL");
  }
  Ok(())
}

pub async fn execute_graphql(
  cdp: &mut ChromeDeveloperProtocol,
  request: &GraphqlRequest,
  owned_targets: &mut HashSet<String>,
) -> Result<HttpResponse> {
  validate_read_only_graphql(request)?;
  let request = ApiRequest {
    method: "POST".to_owned(),
    url: request.url.clone(),
    headers: request.headers.clone(),
    body: Some(request.body.clone()),
    download: false,
  };
  execute(cdp, &request, owned_targets, true).await
}

pub async fn execute(
  cdp: &mut ChromeDeveloperProtocol,
  request: &ApiRequest,
  owned_targets: &mut HashSet<String>,
  trusted: bool,
) -> Result<HttpResponse> {
  // Reload configuration for every request so daemon-side options do not become stale.
  let _config = cnvs_config::Config::load()?;
  #[cfg(feature = "write-requests")]
  let _ = trusted;
  #[cfg(not(feature = "write-requests"))]
  if !trusted {
    ensure_method_allowed(&request.method)?;
  }
  let deadline = Instant::now()
    .checked_add(REQUEST_TIMEOUT)
    .context("request deadline overflow")?;
  let url = Url::parse(&request.url).context("invalid request URL")?;
  let wanted_origin = origin(&url)?;

  let targets = cdp
    .call_before(deadline, "Target.getTargets", json!({}), None)
    .await?;
  let target_infos = targets
    .get("targetInfos")
    .and_then(Value::as_array)
    .context("Target.getTargets returned no targetInfos")?;
  let live_targets: HashSet<_> = target_infos
    .iter()
    .filter_map(|target| target["targetId"].as_str())
    .map(str::to_owned)
    .collect();
  owned_targets.retain(|target| live_targets.contains(target));

  let existing = target_infos.iter().find_map(|target| {
    (target["type"].as_str() == Some("page")
      && target["url"]
        .as_str()
        .and_then(|url| Url::parse(url).ok())
        .and_then(|url| origin(&url).ok())
        .as_deref()
        == Some(wanted_origin.as_str()))
    .then(|| target["targetId"].as_str().map(str::to_owned))
    .flatten()
  });

  let (target_id, created) = if let Some(target) = existing {
    (target, false)
  } else {
    let result = cdp
      .call_before(
        deadline,
        "Target.createTarget",
        json!({ "url": "about:blank" }),
        None,
      )
      .await?;
    let target = result
      .get("targetId")
      .and_then(Value::as_str)
      .context("Target.createTarget returned no targetId")?
      .to_owned();
    owned_targets.insert(target.clone());
    (target, true)
  };

  let attached = cdp
    .call_before(
      deadline,
      "Target.attachToTarget",
      json!({ "targetId": target_id, "flatten": true }),
      None,
    )
    .await?;
  let session_id = attached
    .get("sessionId")
    .and_then(Value::as_str)
    .context("Target.attachToTarget returned no sessionId")?
    .to_owned();

  let result = async {
    prepare_target(cdp, &wanted_origin, &session_id, created, deadline).await?;
    if request.download {
      download_in_target(cdp, request, &session_id, deadline).await
    } else {
      run_in_target(cdp, request, &session_id, deadline).await
    }
  }
  .await;

  let _ = timeout(
    Duration::from_secs(5),
    cdp.call(
      "Target.detachFromTarget",
      json!({ "sessionId": session_id }),
      None,
    ),
  )
  .await;
  cdp.clear_pending_events();
  result
}

async fn prepare_target(
  cdp: &mut ChromeDeveloperProtocol,
  wanted_origin: &str,
  session_id: &str,
  created: bool,
  deadline: Instant,
) -> Result<()> {
  if created {
    cdp
      .call_before(deadline, "Page.enable", json!({}), Some(session_id))
      .await?;
    let navigation = cdp
      .call_before(
        deadline,
        "Page.navigate",
        json!({ "url": format!("{wanted_origin}/") }),
        Some(session_id),
      )
      .await?;
    if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
      bail!("could not navigate to {wanted_origin}: {error}");
    }
    cdp
      .wait_for_event_before(deadline, "Page.loadEventFired", session_id)
      .await?;
  }

  let current_origin = evaluate_before(cdp, deadline, session_id, "location.origin")
    .await?
    .as_str()
    .map(str::to_owned)
    .context("could not determine page origin")?;
  if current_origin != wanted_origin {
    bail!(
      "expected a page on {wanted_origin}, but Chrome ended up on {current_origin}; the site may have redirected to login/SSO"
    );
  }
  Ok(())
}

async fn run_in_target(
  cdp: &mut ChromeDeveloperProtocol,
  request: &ApiRequest,
  session_id: &str,
  deadline: Instant,
) -> Result<HttpResponse> {
  let url = serde_json::to_string(&request.url)?;
  let method = serde_json::to_string(&request.method)?;
  let headers = serde_json::to_string(&request.headers)?;
  let body = serde_json::to_string(&request.body)?;
  let remaining_ms = deadline
    .saturating_duration_since(Instant::now())
    .as_millis()
    .max(1);
  let expression = format!(
    r#"
      (async () => {{
          const headers = new Headers({headers}.map((header) => [header.name, header.value]));
          const csrfToken = document.cookie
              .split("; ")
              .find((cookie) => cookie.startsWith("_csrf_token="))
              ?.split("=")[1];
          if (csrfToken && !headers.has("x-csrf-token")) {{
              headers.set("x-csrf-token", decodeURIComponent(csrfToken));
          }}

          const response = await fetch({url}, {{
              method: {method},
              headers,
              body: {body} ?? undefined,
              credentials: "include",
              signal: AbortSignal.timeout({remaining_ms}),
          }});
          return {{
              status: response.status,
              statusText: response.statusText,
              body: await response.text(),
          }};
      }})()
    "#
  );
  let result = evaluate_before(cdp, deadline, session_id, &expression).await?;

  Ok(HttpResponse {
    status: result
      .get("status")
      .and_then(Value::as_u64)
      .and_then(|status| status.try_into().ok())
      .context("fetch returned no valid status")?,
    status_text: result
      .get("statusText")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_owned(),
    body: ResponseBody::Text(
      result
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned(),
    ),
  })
}

async fn download_in_target(
  cdp: &mut ChromeDeveloperProtocol,
  request: &ApiRequest,
  session_id: &str,
  deadline: Instant,
) -> Result<HttpResponse> {
  if request.method != "GET" || request.body.is_some() || !request.headers.is_empty() {
    bail!("downloads support GET requests without custom headers or a request body");
  }

  let directory = download_directory()?;
  cdp
    .call_before(
      deadline,
      "Browser.setDownloadBehavior",
      json!({
        "behavior": "allow",
        "downloadPath": directory,
        "eventsEnabled": true,
      }),
      None,
    )
    .await?;
  cdp
    .call_before(
      deadline,
      "Runtime.evaluate",
      json!({
        "expression": format!(
          "(() => {{ const a = document.createElement('a'); a.href = {}; a.download = ''; document.body.appendChild(a); a.click(); a.remove(); }})()",
          serde_json::to_string(&request.url)?
        ),
      }),
      Some(session_id),
    )
    .await?;
  let begin = cdp
    .wait_for_browser_event_before(deadline, "Browser.downloadWillBegin")
    .await?;
  let guid = begin["guid"]
    .as_str()
    .context("download event had no guid")?
    .to_owned();
  loop {
    let progress = cdp
      .wait_for_browser_event_before(deadline, "Browser.downloadProgress")
      .await?;
    if progress["guid"].as_str() != Some(&guid) {
      continue;
    }
    match progress["state"].as_str() {
      Some("completed") => break,
      Some("canceled") => anyhow::bail!("download was canceled"),
      _ => {}
    }
  }
  let file = fs::read_dir(&directory)?
    .filter_map(Result::ok)
    .map(|entry| entry.path())
    .find(|path| path.is_file())
    .context("completed download file was not found")?;
  Ok(HttpResponse {
    status: 200,
    status_text: String::new(),
    body: ResponseBody::File(file),
  })
}
async fn evaluate_before(
  cdp: &mut ChromeDeveloperProtocol,
  deadline: Instant,
  session_id: &str,
  expression: &str,
) -> Result<Value> {
  let result = cdp
    .call_before(
      deadline,
      "Runtime.evaluate",
      json!({
          "expression": expression,
          "awaitPromise": true,
          "returnByValue": true,
      }),
      Some(session_id),
    )
    .await?;
  if let Some(exception) = result.get("exceptionDetails") {
    bail!("JavaScript evaluation failed: {exception}");
  }
  result
    .get("result")
    .and_then(|result| result.get("value"))
    .cloned()
    .context("Runtime.evaluate returned no value")
}

pub async fn close_owned_targets(cdp: &mut ChromeDeveloperProtocol, targets: &HashSet<String>) {
  for target in targets {
    let _ = timeout(
      Duration::from_secs(2),
      cdp.call("Target.closeTarget", json!({ "targetId": target }), None),
    )
    .await;
  }
}

#[cfg(not(feature = "write-requests"))]
fn ensure_method_allowed(method: &str) -> Result<()> {
  if !method.eq_ignore_ascii_case("GET") {
    bail!("only GET requests are enabled; reinstall with --features write-requests to enable write requests");
  }
  Ok(())
}

fn origin(url: &Url) -> Result<String> {
  if !matches!(url.scheme(), "http" | "https") {
    bail!("only http:// and https:// URLs are supported");
  }
  let origin = url.origin().ascii_serialization();
  if origin == "null" {
    bail!("URL has an opaque origin");
  }
  Ok(origin)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn read_only_graphql_cannot_bypass_the_write_gate() {
    let mut request = GraphqlRequest {
      url: "https://canvas.example/api/graphql".into(),
      headers: vec![],
      body: json!({ "query": ASSIGNMENTS_QUERY, "variables": { "userId": "123" } }).to_string(),
    };
    assert!(validate_read_only_graphql(&request).is_ok());
    let valid_body = request.body.clone();
    for body in [
      json!({ "query": "mutation { deleteThing(id: 1) }", "variables": { "userId": "123" } }),
      json!({ "query": ASSIGNMENTS_QUERY, "variables": { "userId": "invalid" } }),
      json!({ "query": ASSIGNMENTS_QUERY, "variables": { "userId": "123" }, "operationName": "Other" }),
      json!([{ "query": ASSIGNMENTS_QUERY, "variables": { "userId": "123" } }]),
    ] {
      request.body = body.to_string();
      assert!(validate_read_only_graphql(&request).is_err());
    }
    request.body = valid_body;
    for url in [
      "https://canvas.example/api/v1/courses",
      "https://canvas.example/api/graphql?query=mutation",
      "file:///api/graphql",
    ] {
      request.url = url.into();
      assert!(validate_read_only_graphql(&request).is_err());
    }
  }

  #[test]
  fn accepts_only_network_origins() {
    assert_eq!(
      origin(&Url::parse("https://canvas.example/path").unwrap()).unwrap(),
      "https://canvas.example"
    );
    assert!(origin(&Url::parse("file:///tmp/test").unwrap()).is_err());
  }

  #[cfg(not(feature = "write-requests"))]
  #[test]
  fn allows_only_get_when_writes_are_disabled() {
    assert!(ensure_method_allowed("GET").is_ok());
    assert!(ensure_method_allowed("PATCH").is_err());
  }
}
