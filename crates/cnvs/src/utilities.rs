use anyhow::{Context, Result};
use cnvs_protocol::{ApiRequest, GraphqlRequest, Header, Request, Response};
use serde::Serialize;
use url::Url;

pub(crate) struct ApiRequestBuilder {
  request: ApiRequest,
  verbose: bool,
  trusted_graphql: bool,
}

pub(crate) enum RequestBody {
  Text(String),
  Json(String),
}

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
      trusted_graphql: false,
    }
  }

  pub(crate) fn get(url: Url) -> Self {
    Self::new("GET", url)
  }

  pub(crate) fn post(url: Url) -> Self {
    Self::new("POST", url)
  }

  pub(crate) fn enable_verbose(&mut self, verbose: bool) -> &mut Self {
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

  /// Marks this request as the vetted built-in GraphQL request.
  pub(crate) fn allow_read_only_graphql(&mut self) -> &mut Self {
    self.trusted_graphql = true;
    self
  }

  pub(crate) fn body(&mut self, body: RequestBody) -> &mut Self {
    let (body, content_type) = match body {
      RequestBody::Text(body) => (body, None),
      RequestBody::Json(body) => (body, Some("application/json")),
    };
    self.request.body = Some(body);
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

  pub(crate) fn json(mut self) -> Result<Response> {
    self.default_header("accept", "application/json");
    self.execute()
  }

  pub(crate) fn text(self) -> Result<Response> {
    self.execute()
  }

  pub(crate) fn download(mut self) -> Result<Response> {
    self.request.download = true;
    self.execute()
  }

  fn execute(self) -> Result<Response> {
    let (_, started) = crate::commands::daemon::ensure_running(None)?;

    if started && self.verbose {
      eprintln!("started daemon");
    }

    let request = if self.trusted_graphql {
      let request = self.request;
      Request::Graphql(GraphqlRequest {
        url: request.url,
        headers: request.headers,
        body: request
          .body
          .context("trusted GraphQL requests must include a body")?,
      })
    } else {
      Request::Api(self.request)
    };
    let response = crate::commands::daemon::send_request(&request)
      .context("daemon stopped before accepting the request; run the command again")?
      .context("empty response from the server")?;

    Ok(response)
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
    assert_eq!(request.body.as_deref(), Some(r#"{"ok":true}"#));
  }
}
