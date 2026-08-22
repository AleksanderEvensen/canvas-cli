use std::collections::VecDeque;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::{
  net::TcpStream,
  time::{timeout_at, Instant},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) struct ChromeDeveloperProtocol {
  ws: Ws,
  next_id: u64,
  pending_events: VecDeque<Value>,
  closed: bool,
}

impl ChromeDeveloperProtocol {
  pub(crate) async fn connect(endpoint: &str) -> Result<Self> {
    let (ws, _) = connect_async(endpoint)
      .await
      .with_context(|| format!("failed to connect to Chrome at {endpoint}"))?;

    Ok(Self {
      ws,
      next_id: 1,
      pending_events: VecDeque::new(),
      closed: false,
    })
  }

  pub(crate) async fn next_json(&mut self) -> Result<Option<Value>> {
    loop {
      let Some(message) = self.ws.next().await else {
        self.closed = true;
        return Ok(None);
      };

      let message = match message {
        Ok(message) => message,
        Err(error) => {
          self.closed = true;
          return Err(error.into());
        }
      };

      match message {
        Message::Text(text) => return Ok(Some(serde_json::from_str(&text)?)),
        Message::Ping(payload) => self.ws.send(Message::Pong(payload)).await?,
        Message::Close(_) => {
          self.closed = true;
          return Ok(None);
        }
        _ => {}
      }
    }
  }

  pub(crate) async fn call(
    &mut self,
    method: &str,
    params: Value,
    session_id: Option<&str>,
  ) -> Result<Value> {
    let id = self.next_id;
    self.next_id += 1;

    let mut request = json!({ "id": id, "method": method, "params": params });
    if let Some(session_id) = session_id {
      request["sessionId"] = json!(session_id);
    }
    self
      .ws
      .send(Message::Text(request.to_string().into()))
      .await?;

    while let Some(response) = self.next_json().await? {
      if response.get("id").and_then(Value::as_u64) != Some(id) {
        if response.get("method").is_some() && self.pending_events.len() < 100 {
          self.pending_events.push_back(response);
        }
        continue;
      }
      if let Some(error) = response.get("error") {
        bail!("CDP {method} failed: {error}");
      }
      return Ok(response["result"].clone());
    }

    bail!("Chrome closed the CDP connection")
  }

  pub(crate) async fn call_before(
    &mut self,
    deadline: Instant,
    method: &str,
    params: Value,
    session_id: Option<&str>,
  ) -> Result<Value> {
    timeout_at(deadline, self.call(method, params, session_id))
      .await
      .context("request timed out")?
  }

  pub(crate) async fn wait_for_event_before(
    &mut self,
    deadline: Instant,
    method: &str,
    session_id: &str,
  ) -> Result<Value> {
    if let Some(index) = self.pending_events.iter().position(|event| {
      event.get("method").and_then(Value::as_str) == Some(method)
        && event.get("sessionId").and_then(Value::as_str) == Some(session_id)
    }) {
      return Ok(self.pending_events.remove(index).expect("event exists")["params"].clone());
    }

    loop {
      let event = timeout_at(deadline, self.next_json())
        .await
        .context("request timed out")??
        .context("Chrome closed the CDP connection")?;
      if event.get("method").and_then(Value::as_str) == Some(method)
        && event.get("sessionId").and_then(Value::as_str) == Some(session_id)
      {
        return Ok(event["params"].clone());
      }
    }
  }

  pub(crate) fn clear_pending_events(&mut self) {
    self.pending_events.clear();
  }

  pub(crate) fn closed(&self) -> bool {
    self.closed
  }
}
