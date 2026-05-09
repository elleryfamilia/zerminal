//! Wire-layer for the Copilot CLI `/ide` integration: HTTP/1.1 + SSE over a
//! Unix domain socket. The MCP message layer sits on top of this.

mod reader;

pub use reader::{ReadError, RequestParts, RequestReader};
