//! The JSON-RPC endpoint (`httprpc.cpp`): `POST /` and `POST /wallet/<name>`, authentication,
//! whitelists and batches.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::UniValue;
use dash_args::ArgsManager;

use crate::auth::{AuthInitFailed, RpcAuth};
use crate::http::{HttpRequest, HttpServer, Method};
use crate::protocol::*;
use crate::request::{JsonRpcRequest, param};
use crate::server::RpcServer;

/// What `WWW-Authenticate` offers with a `401`.
const WWW_AUTH_HEADER_DATA: &str = "Basic realm=\"jsonrpc\"";

/// The endpoint's state: the command table and the credentials.
pub struct HttpRpc {
    server: Arc<RpcServer>,
    auth: RpcAuth,
    /// `-rpcexternaluser`, for the request log.
    external_users: Vec<String>,
}

/// The per-request record that logs the outcome when the request is done (`RpcHttpRequest`).
struct Exchange<'a> {
    rpc: &'a HttpRpc,
    req: HttpRequest,
    start: Instant,
    status: u16,
    user: String,
    command: String,
}

impl Exchange<'_> {
    fn send_reply(&mut self, status: u16, body: &str) {
        self.status = status;
        self.req.write_reply(status, body.as_bytes());
    }
}

impl Drop for Exchange<'_> {
    fn drop(&mut self) {
        let external = self.rpc.external_users.contains(&self.user);
        tracing::debug!(
            target: "bench",
            "HTTP RPC request handled: user={} command={} external={} status={} elapsed_time_ms={}",
            self.user,
            self.command,
            external,
            self.status,
            self.start.elapsed().as_millis()
        );
    }
}

impl HttpRpc {
    /// Core's `StartHTTPRPC`: authentication, then the handlers on `/` and, as a dashd built
    /// with wallet support registers it, `/wallet/` (D-row RP0-D3).
    // Core: httprpc.cpp:319-337
    pub fn start(
        server: Arc<RpcServer>,
        http: &HttpServer,
        args: &ArgsManager,
    ) -> Result<Arc<HttpRpc>, AuthInitFailed> {
        tracing::debug!(target: "rpc", "Starting HTTP RPC server");
        let auth = RpcAuth::init(args)?;
        let rpc = Arc::new(HttpRpc {
            server,
            auth,
            external_users: http.external_users().to_vec(),
        });
        for (prefix, exact) in [("/", true), ("/wallet/", false)] {
            let handler = Arc::clone(&rpc);
            http.register_handler(prefix, exact, move |req, _| handler.handle(req));
        }
        Ok(rpc)
    }

    /// `InterruptHTTPRPC`.
    pub fn interrupt(&self) {
        tracing::debug!(target: "rpc", "Interrupting HTTP RPC server");
    }

    /// `StopHTTPRPC`, then `StopRPC`'s cookie removal.
    pub fn stop(&self, http: &HttpServer) {
        tracing::debug!(target: "rpc", "Stopping HTTP RPC server");
        http.unregister_handler("/", true);
        http.unregister_handler("/wallet/", false);
    }

    /// `DeleteAuthCookie` (Core's `StopRPC` calls it).
    pub fn delete_cookie(&self) {
        self.auth.delete_cookie();
    }

    /// Core's `whitelisted`: the method, or the method and its first argument without a space.
    // Core: httprpc.cpp:84-91
    fn whitelisted(&self, auth: &RpcAuth, req: &JsonRpcRequest) -> bool {
        let Some(list) = auth.whitelist.get(&req.auth_user) else {
            return false;
        };
        if list.contains(&req.method) {
            return true;
        }
        match (&req.params, param(&req.params, 0)) {
            (UniValue::Arr(_), UniValue::Str(sub)) => {
                list.contains(&format!("{}{sub}", req.method))
            }
            _ => false,
        }
    }

    /// Core's `HTTPReq_JSONRPC`.
    // Core: httprpc.cpp:162-245
    fn handle(&self, req: HttpRequest) {
        let mut ex = Exchange {
            rpc: self,
            req,
            start: Instant::now(),
            status: 0,
            user: String::new(),
            command: String::new(),
        };
        // JSONRPC handles only POST.
        if ex.req.method() != Method::Post {
            ex.send_reply(HTTP_BAD_METHOD, "JSONRPC server handles only POST requests");
            return;
        }
        let Some(auth_header) = ex.req.header("authorization").map(str::to_owned) else {
            ex.req.write_header("WWW-Authenticate", WWW_AUTH_HEADER_DATA);
            ex.send_reply(HTTP_UNAUTHORIZED, "");
            return;
        };
        let mut jreq = JsonRpcRequest {
            peer_addr: ex.req.peer_string(),
            ..Default::default()
        };
        let auth = &self.auth;
        let (user, ok) = auth.authorized(&auth_header);
        if let Some(user) = user {
            ex.user = user;
        }
        if !ok {
            tracing::info!("ThreadRPCServer incorrect password attempt from {}", jreq.peer_addr);
            // Deter brute-forcing. If this results in a DoS the user really shouldn't have
            // their RPC port exposed.
            std::thread::sleep(Duration::from_millis(250));
            ex.req.write_header("WWW-Authenticate", WWW_AUTH_HEADER_DATA);
            ex.send_reply(HTTP_UNAUTHORIZED, "");
            return;
        }
        jreq.auth_user = ex.user.clone();
        let result = self.reply(&mut ex, auth, &mut jreq);
        match result {
            Ok(Some(reply)) => {
                ex.req.write_header("Content-Type", "application/json");
                ex.send_reply(HTTP_OK, &reply);
            }
            // Already answered (403).
            Ok(None) => {}
            Err(error) => {
                let status = match error.code() {
                    RPC_INVALID_REQUEST => HTTP_BAD_REQUEST,
                    RPC_METHOD_NOT_FOUND => HTTP_NOT_FOUND,
                    _ => HTTP_INTERNAL_SERVER_ERROR,
                };
                let reply = json_rpc_reply(UniValue::Null, error.to_object(), jreq.id.clone());
                ex.req.write_header("Content-Type", "application/json");
                ex.send_reply(status, &reply);
            }
        }
    }

    /// The body of `HTTPReq_JSONRPC`'s `try` block: the reply text, `None` once a `403` is
    /// sent, or the error `JSONErrorReply` answers with.
    fn reply(
        &self,
        ex: &mut Exchange<'_>,
        auth: &RpcAuth,
        jreq: &mut JsonRpcRequest,
    ) -> Result<Option<String>, RpcError> {
        let body = ex.req.read_body();
        let Some(value) = UniValue::read(&body) else {
            return Err(RpcError::new(RPC_PARSE_ERROR, "Parse error"));
        };
        jreq.uri = ex.req.uri().to_owned();
        let log_ips = self.server.control().log_ips();
        let user_has_whitelist = auth.whitelist.contains_key(&jreq.auth_user);
        if !user_has_whitelist && auth.whitelist_default {
            tracing::info!("RPC User {} not allowed to call any methods", jreq.auth_user);
            ex.send_reply(HTTP_FORBIDDEN, "");
            return Ok(None);
        }
        match &value {
            UniValue::Obj(_) => {
                jreq.parse(&value, log_ips)?;
                ex.command = jreq.method.clone();
                if user_has_whitelist && !self.whitelisted(auth, jreq) {
                    tracing::info!(
                        "RPC User {} not allowed to call method {}",
                        jreq.auth_user,
                        jreq.method
                    );
                    ex.send_reply(HTTP_FORBIDDEN, "");
                    return Ok(None);
                }
                let result = self.server.execute(jreq)?;
                Ok(Some(json_rpc_reply(result, UniValue::Null, jreq.id.clone())))
            }
            UniValue::Arr(batch) => {
                // Every call of a batch must be whitelisted (v24, O17 RPC-10).
                if user_has_whitelist {
                    for element in batch {
                        if !element.is_object() {
                            return Err(RpcError::new(
                                RPC_INVALID_REQUEST,
                                "Invalid Request object",
                            ));
                        }
                        let mut call = jreq.clone();
                        call.parse(element, log_ips)?;
                        if !self.whitelisted(auth, &call) {
                            tracing::info!(
                                "RPC User {} not allowed to call method {}",
                                jreq.auth_user,
                                call.method
                            );
                            ex.send_reply(HTTP_FORBIDDEN, "");
                            return Ok(None);
                        }
                    }
                }
                Ok(Some(self.exec_batch(jreq, batch)))
            }
            _ => Err(RpcError::new(RPC_PARSE_ERROR, "Top-level object parse error")),
        }
    }

    /// Core's `JSONRPCExecBatch`: every call's reply object, errors included.
    // Core: rpc/server.cpp:399-429
    fn exec_batch(&self, jreq: &JsonRpcRequest, batch: &[UniValue]) -> String {
        let log_ips = self.server.control().log_ips();
        let replies = batch
            .iter()
            .map(|element| {
                let mut call = jreq.clone();
                let result = call.parse(element, log_ips).and_then(|()| self.server.execute(&call));
                match result {
                    Ok(result) => json_rpc_reply_obj(result, UniValue::Null, call.id),
                    Err(e) => json_rpc_reply_obj(UniValue::Null, e.to_object(), call.id),
                }
            })
            .collect();
        UniValue::Arr(replies).write() + "\n"
    }
}

#[cfg(test)]
mod tests;
