//! Turning an RPC failure into the status line the bridge answers with.

use ferry_core::ops::OpError;
use ferry_core::rpc::RpcError;

/// What to answer for one failed operation on the peer, and whether the
/// connection that produced it should be dropped rather than reused.
pub(crate) fn map_rpc_error(error: &RpcError) -> (&'static str, bool) {
    match error {
        RpcError::Remote(OpError::NotFound) => ("404 Not Found", false),
        // I1 only ever calls `stat`, `list`, and `read`, none of which the
        // peer's `Roots` refuses for a real reason (`roots.rs`): a
        // `PermissionDenied` answer here can only mean the peer's own
        // `stop` or `forget` switched this connection off. Reported as the
        // device going away, which is what it is from Finder's side.
        RpcError::Remote(OpError::PermissionDenied) => ("503 Service Unavailable", true),
        RpcError::Remote(_) => ("500 Internal Server Error", false),
        // Anything that is not an answer from the peer is the connection
        // itself failing: the peer stopped and closed the socket
        // (`docs/engine-contract.md` item 16c), the cable went, or the
        // stream broke. From Finder's side the device went away.
        _ => ("503 Service Unavailable", true),
    }
}
