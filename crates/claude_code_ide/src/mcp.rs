use std::sync::Arc;

use editor_capabilities::EditorCapabilities;

/// Routes MCP method calls from the WebSocket layer into [`EditorCapabilities`].
/// Each MCP tool maps to one method here; each method:
///   - parses tool arguments into typed structs (serde),
///   - acquires `&App` / `&mut App` via the foreground executor,
///   - calls the corresponding `EditorCapabilities` method,
///   - serializes the result back to a JSON-RPC response.
///
/// Wire-level method dispatch is added in a follow-up commit; this struct is
/// the holder of the capabilities reference that the dispatch layer will use.
pub struct McpDispatcher {
    capabilities: Arc<dyn EditorCapabilities>,
}

impl McpDispatcher {
    pub fn new(capabilities: Arc<dyn EditorCapabilities>) -> Self {
        Self { capabilities }
    }

    #[allow(dead_code)]
    pub fn capabilities(&self) -> &Arc<dyn EditorCapabilities> {
        &self.capabilities
    }
}
