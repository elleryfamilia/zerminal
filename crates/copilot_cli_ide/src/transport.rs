//! Wire-layer for the Copilot CLI `/ide` integration: HTTP/1.1 + SSE over a
//! Unix domain socket. The MCP message layer sits on top of this.

mod content_negotiation;
mod reader;
mod server;
mod session;
mod writer;

pub(crate) use reader::RequestParts;
pub(crate) use server::{PostHandler, PostResponse, Server};
pub(crate) use session::{CreateError, SessionStore};
