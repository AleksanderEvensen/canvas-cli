use std::{collections::HashSet, fs, time::Duration};

use crate::assignments_query::ASSIGNMENTS_QUERY;
use anyhow::{bail, Context, Result};
use base64::Engine;
use cnvs_protocol::{
  api_request, api_response, ApiRequest, ApiResponse, AssignmentsRequest, Header,
};
use serde_json::{json, Value};
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
use tokio::time::{timeout, Instant};
use url::Url;

use crate::cdp::ChromeDeveloperProtocol;

/// Ceiling for cheap browser calls (target attach, origin checks) on top of the request deadline.
const QUICK_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling for navigation and page-load waits; heavy pages may legitimately take a while.
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Returns the earlier of `deadline` and now plus `cap`.
fn capped(deadline: Instant, cap: Duration) -> Instant {
  deadline.min(Instant::now().checked_add(cap).unwrap_or(deadline))
}

pub fn assignments_request(request: AssignmentsRequest) -> Result<ApiRequest> {
  let url = Url::parse(&request.url).context("invalid GraphQL URL")?;
  origin(&url)?;
  if url.path() != "/api/graphql" || url.query().is_some() || url.fragment().is_some() {
    bail!("assignments requires /api/graphql without query parameters or fragment");
  }
  if request.user_id.is_empty() || !request.user_id.bytes().all(|b| b.is_ascii_digit()) {
    bail!("assignments requires a numeric userId");
  }
  Ok(ApiRequest {
    method: "POST".into(),
    url: request.url,
    headers: vec![
      Header {
        name: "content-type".into(),
        value: "application/json".into(),
      },
      Header {
        name: "accept".into(),
        value: "application/json".into(),
      },
    ],
    body: Some(api_request::Body::TextBody(
      json!({"query": ASSIGNMENTS_QUERY, "variables": {"userId": request.user_id}}).to_string(),
    )),
    download: false,
  })
}

pub fn validate(request: &ApiRequest, assignments: bool) -> Result<(), tonic::Status> {
  let url =
    Url::parse(&request.url).map_err(|_| tonic::Status::invalid_argument("invalid request URL"))?;
  origin(&url).map_err(|e| tonic::Status::invalid_argument(e.to_string()))?;
  http::Method::from_bytes(request.method.as_bytes())
    .map_err(|_| tonic::Status::invalid_argument("invalid HTTP method"))?;
  for header in &request.headers {
    http::HeaderName::from_bytes(header.name.as_bytes())
      .map_err(|_| tonic::Status::invalid_argument("invalid HTTP header name"))?;
    http::HeaderValue::from_str(&header.value)
      .map_err(|_| tonic::Status::invalid_argument("invalid HTTP header value"))?;
  }
  if !cfg!(feature = "write-requests")
    && !assignments
    && !request.method.eq_ignore_ascii_case("GET")
  {
    return Err(tonic::Status::permission_denied(
      "only GET requests are enabled; restart with a write-requests build to enable writes",
    ));
  }
  if request.download
    && (request.method != "GET" || request.body.is_some() || !request.headers.is_empty())
  {
    return Err(tonic::Status::invalid_argument(
      "downloads support GET without headers or body",
    ));
  }
  Ok(())
}

pub async fn execute(
  cdp: &mut ChromeDeveloperProtocol,
  request: &ApiRequest,
  owned_targets: &mut HashSet<String>,
  deadline: Instant,
  download_root: &std::path::Path,
) -> Result<ApiResponse> {
  // Reload configuration for every request so daemon-side options do not become stale.
  let _config = cnvs_config::Config::load()?;
  tracing::info!(
    method = %request.method,
    download = request.download,
    "handling request"
  );
  let started = Instant::now();
  let url = Url::parse(&request.url).context("invalid request URL")?;
  let wanted_origin = origin(&url)?;

  let targets = cdp
    .call_before(
      capped(deadline, QUICK_CALL_TIMEOUT),
      "Target.getTargets",
      json!({}),
      None,
    )
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
  tracing::debug!(targets = live_targets.len(), "listed browser targets");

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
    tracing::info!(target = %target, "reusing existing browser target");
    (target, false)
  } else {
    let result = cdp
      .call_before(
        capped(deadline, QUICK_CALL_TIMEOUT),
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
    tracing::info!(target = %target, "created new browser target");
    owned_targets.insert(target.clone());
    (target, true)
  };

  let attached = cdp
    .call_before(
      capped(deadline, QUICK_CALL_TIMEOUT),
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
  tracing::debug!(session = %session_id, "attached to target");

  let result = async {
    prepare_target(cdp, &wanted_origin, &session_id, created, deadline).await?;
    if request.download {
      download_in_target(cdp, request, &session_id, deadline, download_root).await
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
  tracing::info!(
    elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    "request finished"
  );
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
    tracing::info!(origin = wanted_origin, "navigating new target");
    let navigation_deadline = capped(deadline, NAVIGATION_TIMEOUT);
    cdp
      .call_before(
        navigation_deadline,
        "Page.enable",
        json!({}),
        Some(session_id),
      )
      .await?;
    let navigation = cdp
      .call_before(
        navigation_deadline,
        "Page.navigate",
        json!({ "url": format!("{wanted_origin}/") }),
        Some(session_id),
      )
      .await?;
    if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
      bail!("could not navigate to {wanted_origin}: {error}");
    }
    cdp
      .wait_for_event_before(navigation_deadline, "Page.loadEventFired", session_id)
      .await?;
    tracing::debug!(origin = wanted_origin, "page load completed");
  }

  let current_origin = evaluate_before(
    cdp,
    capped(deadline, QUICK_CALL_TIMEOUT),
    session_id,
    "location.origin",
  )
  .await?
  .as_str()
  .map(str::to_owned)
  .context("could not determine page origin")?;
  if current_origin != wanted_origin {
    bail!(
      "expected a page on {wanted_origin}, but Chrome ended up on {current_origin}; the site may have redirected to login/SSO"
    );
  }
  tracing::debug!(origin = wanted_origin, "origin verified");
  Ok(())
}

async fn run_in_target(
  cdp: &mut ChromeDeveloperProtocol,
  request: &ApiRequest,
  session_id: &str,
  deadline: Instant,
) -> Result<ApiResponse> {
  let url = serde_json::to_string(&request.url)?;
  let method = serde_json::to_string(&request.method)?;
  let headers = serde_json::to_string(
    &request
      .headers
      .iter()
      .map(|h| json!({"name": h.name, "value": h.value}))
      .collect::<Vec<_>>(),
  )?;
  let body = match &request.body {
    None => "undefined".to_owned(),
    Some(api_request::Body::TextBody(text)) => serde_json::to_string(text)?,
    Some(api_request::Body::BinaryBody(bytes)) => format!(
      "Uint8Array.from(atob({}), c => c.charCodeAt(0))",
      serde_json::to_string(&base64::engine::general_purpose::STANDARD.encode(bytes))?
    ),
  };
  let remaining_ms = deadline
    .saturating_duration_since(Instant::now())
    .as_millis()
    .max(1);
  tracing::debug!(
    remaining_ms = u64::try_from(remaining_ms).unwrap_or(u64::MAX),
    "evaluating fetch in page"
  );
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

  let response = ApiResponse {
    status: result
      .get("status")
      .and_then(Value::as_u64)
      .filter(|status| (100..=599).contains(status))
      .and_then(|status| status.try_into().ok())
      .context("fetch returned no valid status")?,
    status_text: result
      .get("statusText")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_owned(),
    body: Some(api_response::Body::Text(
      result
        .get("body")
        .and_then(Value::as_str)
        .context("fetch returned no body")?
        .to_owned(),
    )),
  };
  tracing::info!(status = response.status, "fetch completed");
  Ok(response)
}

async fn download_in_target(
  cdp: &mut ChromeDeveloperProtocol,
  request: &ApiRequest,
  session_id: &str,
  deadline: Instant,
  download_root: &std::path::Path,
) -> Result<ApiResponse> {
  if request.method != "GET" || request.body.is_some() || !request.headers.is_empty() {
    bail!("downloads support GET requests without custom headers or a request body");
  }

  let directory = tempfile::Builder::new()
    .prefix("download-")
    .permissions(fs::Permissions::from_mode(0o700))
    .tempdir_in(download_root)?;
  let mut download_guid = None;
  let result: Result<std::path::PathBuf> = async {
    cdp
      .call_before(
        deadline,
        "Browser.setDownloadBehavior",
        json!({
          "behavior": "allow",
          "downloadPath": directory.path(),
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
    let guid = begin.get("guid")
      .and_then(Value::as_str)
      .context("download event had no guid")?
      .to_owned();
    download_guid = Some(guid.clone());
    loop {
      let progress = cdp
        .wait_for_browser_event_before(deadline, "Browser.downloadProgress")
        .await?;
      if progress.get("guid").and_then(Value::as_str) != Some(&guid) {
        continue;
      }
      match progress.get("state").and_then(Value::as_str) {
        Some("completed") => break,
        Some("canceled") => anyhow::bail!("download was canceled"),
        _ => {}
      }
    }
    let file = fs::read_dir(directory.path())?
      .filter_map(Result::ok)
      .map(|entry| entry.path())
      .find(|path| path.is_file())
      .context("completed download file was not found")?;
    Ok(file)
  }.await;
  if result.is_err() {
    if let Some(guid) = download_guid {
      let _ = timeout(
        Duration::from_secs(2),
        cdp.call("Browser.cancelDownload", json!({"guid": guid}), None),
      )
      .await;
    }
  }
  let _ = timeout(
    Duration::from_secs(2),
    cdp.call(
      "Browser.setDownloadBehavior",
      json!({"behavior": "default", "eventsEnabled": false}),
      None,
    ),
  )
  .await;
  let file = result?;
  let _ = directory.keep();
  Ok(ApiResponse {
    status: 200,
    status_text: String::new(),
    body: Some(api_response::Body::LocalFilePath(
      file.as_os_str().as_bytes().to_vec(),
    )),
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
  fn call_caps_never_extend_request_deadlines() {
    let now = Instant::now();
    let short = now + Duration::from_secs(1);
    assert_eq!(capped(short, QUICK_CALL_TIMEOUT), short);
    let long = now + Duration::from_secs(300);
    let limited = capped(long, QUICK_CALL_TIMEOUT);
    assert!(limited >= now + QUICK_CALL_TIMEOUT);
    assert!(limited <= Instant::now() + QUICK_CALL_TIMEOUT);
    assert!(limited < long);
    assert_eq!(capped(now, NAVIGATION_TIMEOUT), now);
  }

  async fn browser_payload_peer(listener: tokio::net::TcpListener) {
    use futures_util::{SinkExt, StreamExt};
    use std::{
      os::unix::{ffi::OsStringExt, fs::PermissionsExt},
      path::PathBuf,
    };
    use tokio_tungstenite::tungstenite::Message;
    let (socket, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
    let mut attempts = 0_u32;
    let mut directory = None;
    while let Some(Ok(Message::Text(text))) = ws.next().await {
      let message: Value = serde_json::from_str(&text).unwrap();
      let method = message.get("method").and_then(Value::as_str).unwrap();
      let params = message.get("params").unwrap();
      let mut events = false;
      let response = match method {
        "Runtime.evaluate" if params.get("awaitPromise").is_some() => {
          let expression = params.get("expression").and_then(Value::as_str).unwrap();
          assert!(expression.contains("Uint8Array.from(atob(\"AP+A\"), c => c.charCodeAt(0))"));
          json!({"result": {"result": {"value": {"status": 200, "body": ""}}}})
        }
        "Browser.setDownloadBehavior"
          if params.get("behavior").and_then(Value::as_str) == Some("allow") =>
        {
          attempts = attempts.checked_add(1).unwrap();
          let path = PathBuf::from(params.get("downloadPath").and_then(Value::as_str).unwrap());
          assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
          );
          // macOS filesystems reject invalid UTF-8 names; exercise raw bytes on Linux.
          let filename = if cfg!(target_os = "macos") {
            std::ffi::OsString::from("f-é")
          } else {
            std::ffi::OsString::from_vec(vec![b'f', 255])
          };
          fs::write(path.join(filename), [0, 255, 128]).unwrap();
          directory = Some(path);
          if attempts == 1 {
            json!({"error": {"message": "scripted failure"}})
          } else {
            json!({"result": {}})
          }
        }
        "Browser.setDownloadBehavior" => json!({"result": {}}),
        "Runtime.evaluate" => {
          events = true;
          json!({"result": {}})
        }
        _ => panic!("unexpected method {method}"),
      };
      let mut response = response;
      response
        .as_object_mut()
        .unwrap()
        .insert("id".into(), message.get("id").unwrap().clone());
      ws.send(Message::Text(response.to_string().into()))
        .await
        .unwrap();
      if events {
        for (method, params) in [
          ("Browser.downloadWillBegin", json!({"guid": "download"})),
          (
            "Browser.downloadProgress",
            json!({"guid": "download", "state": "completed"}),
          ),
        ] {
          ws.send(Message::Text(
            json!({"method": method, "params": params})
              .to_string()
              .into(),
          ))
          .await
          .unwrap();
        }
      }
    }
    assert!(directory.is_some());
  }

  #[tokio::test]
  async fn binary_fetch_bridge_and_download_artifact_ownership() {
    use std::{os::unix::ffi::OsStringExt, path::PathBuf};
    timeout(Duration::from_secs(5), async {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let endpoint = format!("ws://{}", listener.local_addr().unwrap());
      let peer = tokio::spawn(browser_payload_peer(listener));
      let mut cdp = ChromeDeveloperProtocol::connect(&endpoint).await.unwrap();
      let root = tempfile::tempdir().unwrap();
      let deadline = Instant::now().checked_add(Duration::from_secs(4)).unwrap();
      let mut request = ApiRequest {
        method: "POST".into(),
        url: "https://canvas.example/api".into(),
        body: Some(api_request::Body::BinaryBody(vec![0, 255, 128])),
        ..Default::default()
      };
      assert_eq!(
        run_in_target(&mut cdp, &request, "session", deadline)
          .await
          .unwrap()
          .body,
        Some(api_response::Body::Text(String::new()))
      );
      request.method = "GET".into();
      request.body = None;
      request.download = true;
      assert!(
        download_in_target(&mut cdp, &request, "session", deadline, root.path())
          .await
          .is_err()
      );
      assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
      let response = download_in_target(&mut cdp, &request, "session", deadline, root.path())
        .await
        .unwrap();
      let Some(api_response::Body::LocalFilePath(path)) = &response.body else {
        panic!("missing file");
      };
      assert_eq!(
        fs::read(PathBuf::from(std::ffi::OsString::from_vec(path.clone()))).unwrap(),
        [0, 255, 128]
      );
      crate::cleanup_undelivered(response);
      assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
      drop(cdp);
      peer.await.unwrap();
    })
    .await
    .unwrap();
  }

  #[test]
  fn assignments_boundary_and_raw_gate() {
    let valid = AssignmentsRequest {
      url: "https://canvas.example/api/graphql".into(),
      user_id: "123".into(),
    };
    let built = assignments_request(valid.clone()).unwrap();
    assert!(validate(&built, true).is_ok());
    assert_eq!(
      validate(&built, false).is_ok(),
      cfg!(feature = "write-requests")
    );
    for id in ["", "x", "12x"] {
      assert!(assignments_request(AssignmentsRequest {
        user_id: id.into(),
        ..valid.clone()
      })
      .is_err());
    }
    for url in [
      "file:///api/graphql",
      "https://canvas.example/api/graphql?x=1",
      "https://canvas.example/api/v1/courses",
    ] {
      assert!(assignments_request(AssignmentsRequest {
        url: url.into(),
        ..valid.clone()
      })
      .is_err());
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
}
