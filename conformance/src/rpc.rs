//! Minimal blocking JSON-RPC client for the dashd oracle, with batching.
//!
//! dashd answers a JSON array of requests with an array of responses in one HTTP round trip,
//! which is what makes streaming millions of blocks over RPC practical.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};

/// How to authenticate.
#[derive(Debug, Clone)]
pub enum Auth {
    /// Read `user:password` from dashd's cookie file on every connection (it changes on restart).
    Cookie(PathBuf),
    UserPass(String, String),
}

#[derive(Debug, Clone)]
pub struct Rpc {
    url: String,
    auth: Auth,
    agent: ureq::Agent,
}

impl Rpc {
    pub fn new(url: String, auth: Auth) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .into();
        Self {
            url,
            auth,
            agent,
        }
    }

    fn auth_header(&self) -> Result<String> {
        let creds = match &self.auth {
            Auth::Cookie(path) => std::fs::read_to_string(path)
                .with_context(|| format!("reading RPC cookie {}", path.display()))?
                .trim()
                .to_string(),
            Auth::UserPass(u, p) => format!("{u}:{p}"),
        };
        Ok(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(creds)))
    }

    fn post(&self, body: &Value) -> Result<Value> {
        self.post_with(body, |status, bytes| {
            serde_json::from_slice(bytes).map_err(|e| anyhow!("HTTP {status}: {e}"))
        })
    }

    /// Posts `body` and hands the HTTP status and the raw reply bytes to `parse`; a transport
    /// error or a `parse` failure is retried with backoff.
    fn post_with<T>(
        &self,
        body: &Value,
        parse: impl Fn(ureq::http::StatusCode, &[u8]) -> Result<T>,
    ) -> Result<T> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let res = self
                .agent
                .post(&self.url)
                .header("Authorization", &self.auth_header()?)
                .send_json(body)
                .map_err(anyhow::Error::from)
                .and_then(|mut r| {
                    let status = r.status();
                    let bytes = r
                        .body_mut()
                        .with_config()
                        .limit(u64::MAX)
                        .read_to_vec()
                        .map_err(|e| anyhow!("HTTP {status}: {e}"))?;
                    parse(status, &bytes)
                });
            match res {
                Ok(v) => return Ok(v),
                // dashd's work queue can fill up while it is busy syncing; back off and retry.
                Err(e) if attempt < 8 => {
                    std::thread::sleep(Duration::from_millis(250 << attempt.min(6)));
                    if attempt == 7 {
                        eprintln!("rpc retry {attempt}: {e:#}");
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// A single call whose whole reply body (`{"result":…,"error":…,"id":…}`) is handed to
    /// `parse` as raw bytes, so a caller can keep number text exactly as dashd printed it.
    /// `parse` runs inside the retry loop: a failure there is retried like a transport error.
    pub fn call_raw<T>(
        &self,
        method: &str,
        params: Value,
        parse: impl Fn(&[u8]) -> Result<T>,
    ) -> Result<T> {
        self.post_with(
            &json!({"jsonrpc": "1.0", "id": 0, "method": method, "params": params}),
            |_, bytes| parse(bytes),
        )
    }

    /// A single call.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let v =
            self.post(&json!({"jsonrpc": "1.0", "id": 0, "method": method, "params": params}))?;
        unwrap_reply(method, v)
    }

    /// A single call whose error reply is an answer, not a failure: `Ok(Err(code))` is
    /// dashd's JSON-RPC error code (e.g. -5 for an unknown block).
    pub fn try_call(&self, method: &str, params: Value) -> Result<Result<Value, i64>> {
        let v =
            self.post(&json!({"jsonrpc": "1.0", "id": 0, "method": method, "params": params}))?;
        if let Some(code) = v
            .get("error")
            .filter(|e| !e.is_null())
            .and_then(|e| e.get("code"))
            .and_then(Value::as_i64)
        {
            return Ok(Err(code));
        }
        unwrap_reply(method, v).map(Ok)
    }

    /// A batch of calls with the same method; results are returned in request order.
    pub fn batch(&self, method: &str, params: &[Value]) -> Result<Vec<Value>> {
        if params.is_empty() {
            return Ok(Vec::new());
        }
        let reqs: Vec<Value> = params
            .iter()
            .enumerate()
            .map(|(i, p)| json!({"jsonrpc": "1.0", "id": i, "method": method, "params": p}))
            .collect();
        let Value::Array(replies) = self.post(&Value::Array(reqs))? else {
            bail!("batch {method}: response is not an array");
        };
        let mut out = vec![Value::Null; params.len()];
        for r in replies {
            let id = r
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("batch {method}: reply without id"))?
                as usize;
            *out.get_mut(id).ok_or_else(|| anyhow!("batch {method}: bad id {id}"))? =
                unwrap_reply(method, r)?;
        }
        Ok(out)
    }
}

fn unwrap_reply(method: &str, v: Value) -> Result<Value> {
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        bail!("{method}: {err}");
    }
    v.get("result").cloned().ok_or_else(|| anyhow!("{method}: reply without result: {v}"))
}
