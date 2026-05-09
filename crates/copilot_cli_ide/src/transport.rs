//! Wire-layer for the Copilot CLI `/ide` integration: HTTP/1.1 + SSE over a
//! Unix domain socket. The MCP message layer sits on top of this.

mod content_negotiation;
mod reader;
mod writer;

pub use content_negotiation::{accepts, content_type_is};
pub use reader::{ReadError, RequestParts, RequestReader};
pub use writer::{empty_response, json_response, plain_response, serialize_response};
