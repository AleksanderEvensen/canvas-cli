use anyhow::{Context, Result};
use cnvs_protocol::{api_request, ApiRequest, ApiResponse, Header};
#[cfg(feature = "write-requests")]
use serde::Serialize;
use url::Url;

pub struct ApiRequestBuilder {
  request: ApiRequest,
  verbose: bool,
}

pub enum RequestBody {
  Text(String),
  #[cfg(feature = "write-requests")]
  Json(String),
}

#[cfg(feature = "write-requests")]
impl RequestBody {
  pub(crate) fn json(value: impl Serialize) -> Result<Self> {
    Ok(Self::Json(serde_json::to_string(&value)?))
  }
}

#[allow(dead_code)]
impl ApiRequestBuilder {
  pub(crate) fn new(method: impl Into<String>, url: Url) -> Self {
    Self {
      request: ApiRequest {
        method: method.into().to_ascii_uppercase(),
        url: url.into(),
        headers: vec![],
        body: None,
        download: false,
      },
      verbose: false,
    }
  }

  pub(crate) fn get(url: Url) -> Self {
    Self::new("GET", url)
  }

  pub(crate) fn post(url: Url) -> Self {
    Self::new("POST", url)
  }

  pub(crate) const fn enable_verbose(&mut self, verbose: bool) -> &mut Self {
    self.verbose = verbose;
    self
  }

  pub(crate) fn header(&mut self, name: impl Into<String>, value: impl Into<String>) -> &mut Self {
    self.request.headers.push(Header {
      name: name.into(),
      value: value.into(),
    });
    self
  }

  pub(crate) fn headers<I, N, V>(&mut self, headers: I) -> &mut Self
  where
    I: IntoIterator<Item = (N, V)>,
    N: Into<String>,
    V: Into<String>,
  {
    for (name, value) in headers {
      self.header(name, value);
    }
    self
  }

  pub(crate) fn body(&mut self, body: RequestBody) -> &mut Self {
    let (body, content_type) = match body {
      RequestBody::Text(body) => (body, None::<&str>),
      #[cfg(feature = "write-requests")]
      RequestBody::Json(body) => (body, Some("application/json")),
    };
    self.request.body = Some(api_request::Body::TextBody(body));
    if let Some(content_type) = content_type {
      self.default_header("content-type", content_type);
    }
    self
  }

  fn default_header(&mut self, name: &str, value: &str) {
    if !self
      .request
      .headers
      .iter()
      .any(|header| header.name.eq_ignore_ascii_case(name))
    {
      self.header(name, value);
    }
  }

  pub(crate) async fn json(mut self) -> Result<ApiResponse> {
    self.default_header("accept", "application/json");
    self.execute().await
  }

  pub(crate) async fn text(self) -> Result<ApiResponse> {
    self.execute().await
  }

  pub(crate) async fn download(mut self) -> Result<ApiResponse> {
    self.request.download = true;
    self.execute().await
  }

  async fn execute(self) -> Result<ApiResponse> {
    let (mut client, _, started) = crate::commands::daemon::ensure_running(None).await?;
    if started && self.verbose {
      eprintln!("started daemon");
    }
    self.execute_with(&mut client).await
  }

  pub(crate) async fn json_with(
    mut self,
    client: &mut crate::commands::daemon::Client,
  ) -> Result<ApiResponse> {
    self.default_header("accept", "application/json");
    self.execute_with(client).await
  }

  async fn execute_with(self, client: &mut crate::commands::daemon::Client) -> Result<ApiResponse> {
    let mut request = tonic::Request::new(self.request);
    request.set_timeout(cnvs_protocol::API_TIMEOUT);
    Ok(
      tokio::time::timeout(cnvs_protocol::API_TIMEOUT, client.api(request))
        .await
        .context("daemon request timed out; execution outcome may be unknown")?
        .context("daemon request failed; a submitted write may already have executed")?
        .into_inner(),
    )
  }

  pub(crate) fn into_request(self) -> ApiRequest {
    self.request
  }
}

#[cfg(all(test, feature = "write-requests"))]
mod tests {
  use super::*;

  #[test]
  fn json_defaults_preserve_explicit_accept_and_content_type() {
    let url = Url::parse("https://canvas.example/api").unwrap();
    let mut request = ApiRequestBuilder::get(url);
    request.header("ACCEPT", "application/vnd.canvas+json");
    request.header("Content-Type", "application/custom");
    request.default_header("accept", "application/json");
    request.body(RequestBody::json(serde_json::json!({ "ok": true })).unwrap());
    let request = request.into_request();

    assert_eq!(request.headers.len(), 2);
    assert_eq!(
      request.body,
      Some(api_request::Body::TextBody(r#"{"ok":true}"#.into()))
    );
  }
}
