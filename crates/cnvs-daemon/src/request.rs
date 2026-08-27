use std::{collections::HashSet, time::Duration};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use cnvs_protocol::ApiRequest;
use serde_json::{json, Value};
use tokio::time::{timeout, Instant};
use url::Url;

use crate::cdp::ChromeDeveloperProtocol;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub(crate) struct HttpResponse {
  pub(crate) status: u16,
  pub(crate) status_text: String,
  pub(crate) body: String,
  pub(crate) body_base64: bool,
}

pub(crate) async fn execute(
  cdp: &mut ChromeDeveloperProtocol,
  request: &ApiRequest,
  owned_targets: &mut HashSet<String>,
) -> Result<HttpResponse> {
  ensure_method_allowed(&request.method)?;
  let deadline = Instant::now() + REQUEST_TIMEOUT;
  let url = Url::parse(&request.url).context("invalid request URL")?;
  let wanted_origin = origin(&url)?;

  let targets = cdp
    .call_before(deadline, "Target.getTargets", json!({}), None)
    .await?;
  let target_infos = targets["targetInfos"]
    .as_array()
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

  let (target_id, created) = match existing {
    Some(target) => (target, false),
    None => {
      let result = cdp
        .call_before(
          deadline,
          "Target.createTarget",
          json!({ "url": "about:blank" }),
          None,
        )
        .await?;
      let target = result["targetId"]
        .as_str()
        .context("Target.createTarget returned no targetId")?
        .to_owned();
      owned_targets.insert(target.clone());
      (target, true)
    }
  };

  let attached = cdp
    .call_before(
      deadline,
      "Target.attachToTarget",
      json!({ "targetId": target_id, "flatten": true }),
      None,
    )
    .await?;
  let session_id = attached["sessionId"]
    .as_str()
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
    status: result["status"]
      .as_u64()
      .and_then(|status| status.try_into().ok())
      .context("fetch returned no valid status")?,
    status_text: result["statusText"].as_str().unwrap_or_default().to_owned(),
    body: result["body"].as_str().unwrap_or_default().to_owned(),
    body_base64: false,
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

  let frame_tree = cdp
    .call_before(deadline, "Page.getFrameTree", json!({}), Some(session_id))
    .await?;
  let frame_id = frame_tree["frameTree"]["frame"]["id"]
    .as_str()
    .context("Page.getFrameTree returned no frame id")?;
  let loaded = cdp
    .call_before(
      deadline,
      "Network.loadNetworkResource",
      json!({
        "frameId": frame_id,
        "url": request.url,
        "options": { "disableCache": false, "includeCredentials": true },
      }),
      Some(session_id),
    )
    .await?;
  let resource = &loaded["resource"];
  if resource["success"].as_bool() != Some(true) {
    bail!(
      "download failed: {}",
      resource["netErrorName"]
        .as_str()
        .unwrap_or("unknown network error")
    );
  }

  let mut bytes = Vec::new();
  if let Some(handle) = resource["stream"].as_str() {
    loop {
      let chunk = cdp
        .call_before(
          deadline,
          "IO.read",
          json!({ "handle": handle, "size": 1024 * 1024 }),
          Some(session_id),
        )
        .await?;
      let data = chunk["data"].as_str().unwrap_or_default();
      if chunk["base64Encoded"].as_bool() == Some(true) {
        bytes.extend(
          STANDARD
            .decode(data)
            .context("Chrome returned invalid base64 data")?,
        );
      } else {
        bytes.extend_from_slice(data.as_bytes());
      }
      if chunk["eof"].as_bool() == Some(true) {
        break;
      }
    }
    cdp
      .call_before(
        deadline,
        "IO.close",
        json!({ "handle": handle }),
        Some(session_id),
      )
      .await?;
  }

  Ok(HttpResponse {
    status: resource["httpStatusCode"]
      .as_u64()
      .and_then(|status| status.try_into().ok())
      .unwrap_or(200),
    status_text: String::new(),
    body: STANDARD.encode(bytes),
    body_base64: true,
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
  Ok(result["result"]["value"].clone())
}

pub(crate) async fn close_owned_targets(
  cdp: &mut ChromeDeveloperProtocol,
  targets: &HashSet<String>,
) {
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

#[cfg(feature = "write-requests")]
fn ensure_method_allowed(_method: &str) -> Result<()> {
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
