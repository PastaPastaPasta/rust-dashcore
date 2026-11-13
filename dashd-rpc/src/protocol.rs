//! Status and error codes, and the JSON-RPC 1.0 envelope (`rpc/protocol.h`, `rpc/request.cpp`).
//!
//! The server speaks JSON-RPC 1.0 only (FR-RPC-10): a `"jsonrpc"` member is ignored, every reply
//! carries `result`, `error` and `id`.

use crate::UniValue;

// Core: rpc/protocol.h:10-20
pub const HTTP_OK: u16 = 200;
pub const HTTP_BAD_REQUEST: u16 = 400;
pub const HTTP_UNAUTHORIZED: u16 = 401;
pub const HTTP_FORBIDDEN: u16 = 403;
pub const HTTP_NOT_FOUND: u16 = 404;
pub const HTTP_BAD_METHOD: u16 = 405;
pub const HTTP_INTERNAL_SERVER_ERROR: u16 = 500;
pub const HTTP_SERVICE_UNAVAILABLE: u16 = 503;

// Core: rpc/protocol.h:23-92
pub const RPC_INVALID_REQUEST: i32 = -32600;
pub const RPC_METHOD_NOT_FOUND: i32 = -32601;
pub const RPC_INVALID_PARAMS: i32 = -32602;
pub const RPC_INTERNAL_ERROR: i32 = -32603;
pub const RPC_PARSE_ERROR: i32 = -32700;
pub const RPC_MISC_ERROR: i32 = -1;
pub const RPC_FORBIDDEN_BY_SAFE_MODE: i32 = -2;
pub const RPC_TYPE_ERROR: i32 = -3;
pub const RPC_INVALID_ADDRESS_OR_KEY: i32 = -5;
pub const RPC_OUT_OF_MEMORY: i32 = -7;
pub const RPC_INVALID_PARAMETER: i32 = -8;
pub const RPC_DATABASE_ERROR: i32 = -20;
pub const RPC_DESERIALIZATION_ERROR: i32 = -22;
pub const RPC_VERIFY_ERROR: i32 = -25;
pub const RPC_VERIFY_REJECTED: i32 = -26;
pub const RPC_VERIFY_ALREADY_IN_CHAIN: i32 = -27;
pub const RPC_IN_WARMUP: i32 = -28;
pub const RPC_METHOD_DEPRECATED: i32 = -32;
pub const RPC_TRANSACTION_ERROR: i32 = RPC_VERIFY_ERROR;
pub const RPC_TRANSACTION_REJECTED: i32 = RPC_VERIFY_REJECTED;
pub const RPC_TRANSACTION_ALREADY_IN_CHAIN: i32 = RPC_VERIFY_ALREADY_IN_CHAIN;
pub const RPC_CLIENT_NOT_CONNECTED: i32 = -9;
pub const RPC_CLIENT_IN_INITIAL_DOWNLOAD: i32 = -10;
pub const RPC_CLIENT_NODE_ALREADY_ADDED: i32 = -23;
pub const RPC_CLIENT_NODE_NOT_ADDED: i32 = -24;
pub const RPC_CLIENT_NODE_NOT_CONNECTED: i32 = -29;
pub const RPC_CLIENT_INVALID_IP_OR_SUBNET: i32 = -30;
pub const RPC_CLIENT_P2P_DISABLED: i32 = -31;
pub const RPC_CLIENT_NODE_CAPACITY_REACHED: i32 = -34;
pub const RPC_CLIENT_MEMPOOL_DISABLED: i32 = -33;
pub const RPC_WALLET_ERROR: i32 = -4;
pub const RPC_WALLET_INSUFFICIENT_FUNDS: i32 = -6;
pub const RPC_WALLET_INVALID_LABEL_NAME: i32 = -11;
pub const RPC_WALLET_KEYPOOL_RAN_OUT: i32 = -12;
pub const RPC_WALLET_UNLOCK_NEEDED: i32 = -13;
pub const RPC_WALLET_PASSPHRASE_INCORRECT: i32 = -14;
pub const RPC_WALLET_WRONG_ENC_STATE: i32 = -15;
pub const RPC_WALLET_ENCRYPTION_FAILED: i32 = -16;
pub const RPC_WALLET_ALREADY_UNLOCKED: i32 = -17;
pub const RPC_WALLET_NOT_FOUND: i32 = -18;
pub const RPC_WALLET_NOT_SPECIFIED: i32 = -19;
pub const RPC_WALLET_ALREADY_LOADED: i32 = -35;
pub const RPC_WALLET_ALREADY_EXISTS: i32 = -36;

/// What an RPC handler throws. Core distinguishes them by the C++ exception type, and
/// `ExecuteCommand` turns the last two into error objects.
// Core: rpc/server.cpp:520-536
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcError {
    /// A thrown `JSONRPCError(code, message)` (a `UniValue`): passed on as it is.
    Json {
        code: i32,
        message: String,
    },
    /// A `UniValue::type_error`, such as `get_str` on a number: `RPC_TYPE_ERROR`.
    Type(String),
    /// Any other `std::exception`, including the help text a wrong argument count throws:
    /// `RPC_MISC_ERROR`.
    Runtime(String),
}

impl RpcError {
    /// Core's `JSONRPCError(code, message)`.
    pub fn new(code: i32, message: impl Into<String>) -> RpcError {
        RpcError::Json {
            code,
            message: message.into(),
        }
    }

    /// The error object: `{"code": .., "message": ..}`.
    pub fn to_object(&self) -> UniValue {
        let (code, message) = match self {
            RpcError::Json {
                code,
                message,
            } => (*code, message.as_str()),
            RpcError::Type(m) => (RPC_TYPE_ERROR, m.as_str()),
            RpcError::Runtime(m) => (RPC_MISC_ERROR, m.as_str()),
        };
        json_rpc_error(code, message)
    }

    /// The JSON-RPC code the error object carries.
    pub fn code(&self) -> i32 {
        match self {
            RpcError::Json {
                code,
                ..
            } => *code,
            RpcError::Type(_) => RPC_TYPE_ERROR,
            RpcError::Runtime(_) => RPC_MISC_ERROR,
        }
    }
}

/// `UniValue`'s getters throw `type_error` (a `std::runtime_error`), which `ExecuteCommand`
/// reports as `RPC_TYPE_ERROR`.
impl From<dash_args::RuntimeError> for RpcError {
    fn from(e: dash_args::RuntimeError) -> RpcError {
        RpcError::Type(e.0)
    }
}

/// Core's `JSONRPCError`: the error object.
// Core: rpc/request.cpp:49-55
pub fn json_rpc_error(code: i32, message: &str) -> UniValue {
    let mut error = UniValue::new_object();
    error.push_kv_end("code", i64::from(code).into());
    error.push_kv_end("message", message.into());
    error
}

/// Core's `JSONRPCReplyObj`: `result` is null when `error` is not.
// Core: rpc/request.cpp:32-42
pub fn json_rpc_reply_obj(result: UniValue, error: UniValue, id: UniValue) -> UniValue {
    let mut reply = UniValue::new_object();
    reply.push_kv_end(
        "result",
        if error.is_null() {
            result
        } else {
            UniValue::Null
        },
    );
    reply.push_kv_end("error", error);
    reply.push_kv_end("id", id);
    reply
}

/// Core's `JSONRPCReply`: the reply object, written, and a newline.
pub fn json_rpc_reply(result: UniValue, error: UniValue, id: UniValue) -> String {
    json_rpc_reply_obj(result, error, id).write() + "\n"
}
