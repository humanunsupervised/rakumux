//! The MCP protocol adapter. This is the only module that uses the MCP SDK
//! (`rmcp`, exact-pinned), so swapping or upgrading it touches one file.
//!
//! Transport: stdio, newline-delimited JSON-RPC. Stdout carries protocol
//! messages only; diagnostics go to stderr.

use crate::client::{ApiClient, ToolError};
use crate::instructions::INSTRUCTIONS;
use crate::tools::{self, ToolOutcome, ToolSpec};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use std::sync::Arc;

/// OpenCode 2.x tags every tool call with the calling conversation. Its MCP
/// servers run in a background service shared by every conversation, so
/// the environment's session identity (if any) may belong to another
/// session. The Rakumux plugin in OpenCode's
/// TUI binds the conversations it shows to its Rakumux session, and calls
/// act as that session.
const OPENCODE_SESSION_META: &str = "ai.opencode/sessionID";

/// The Rakumux MCP server.
pub struct RakumuxServer {
    api: Arc<ApiClient>,
    specs: Vec<ToolSpec>,
    tools: Vec<Tool>,
    version: String,
}

impl RakumuxServer {
    pub fn new(api: ApiClient, version: &str) -> Self {
        let specs = tools::catalogue();
        let tools = specs.iter().map(to_tool).collect();
        Self {
            api: Arc::new(api),
            specs,
            tools,
            version: version.to_owned(),
        }
    }

    /// Serve MCP over `reader`/`writer` until the client disconnects.
    pub async fn serve<R, W>(self, reader: R, writer: W) -> Result<(), String>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
        W: tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        let running = ServiceExt::serve(self, (reader, writer))
            .await
            .map_err(|e| format!("MCP handshake failed: {e}"))?;
        running
            .waiting()
            .await
            .map(|_| ())
            .map_err(|e| format!("MCP server stopped: {e}"))
    }

    /// Serve MCP on stdin/stdout.
    pub async fn serve_stdio(self) -> Result<(), String> {
        let (stdin, stdout) = rmcp::transport::stdio();
        self.serve(stdin, stdout).await
    }

    async fn run_tool(
        &self,
        request: CallToolRequestParams,
        opencode_conversation: Option<String>,
    ) -> Option<ToolOutcome> {
        let spec = self.specs.iter().find(|spec| spec.name == request.name)?;
        let arguments = request.arguments.unwrap_or_default();
        let Some(conversation) = opencode_conversation else {
            return tools::call(&self.api.caller(true), spec.name, arguments).await;
        };
        // Act as the Rakumux session that bound this conversation.
        let bound = self.api.for_conversation("opencode", &conversation);
        let outcome = tools::call(&bound, spec.name, arguments.clone()).await?;
        if !outcome.conversation_unbound() {
            return Some(outcome);
        }
        // No session bound it: OpenCode runs outside Rakumux, or without
        // the plugin. An environment identity then may be any session's:
        // act as the user for reads and refuse changes.
        let ambiguous = self.api.identity().is_some();
        if ambiguous && spec.mutating {
            return Some(ToolOutcome::error(&ToolError::new(
                "permission_denied",
                "this OpenCode conversation isn't linked to a Rakumux session, and this MCP \
                 server is shared by every OpenCode conversation, so it can't tell which \
                 session is calling; tools that change sessions are disabled. Install the \
                 Rakumux plugin for OpenCode (`rakumux-mcp opencode-plugin --install`, then \
                 restart OpenCode), or run `rakumux new --detach` from the shell tool.",
            )));
        }
        tools::call(&self.api.caller(false), spec.name, arguments).await
    }
}

impl ServerHandler for RakumuxServer {
    fn get_info(&self) -> ServerConfig {
        let mut implementation = Implementation::new("rakumux", self.version.clone());
        implementation.title = Some("Rakumux".into());
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(implementation)
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let conversation = |meta: &rmcp::model::RequestMetaObject| {
            meta.0
                 .0
                .get(OPENCODE_SESSION_META)
                .and_then(|value| value.as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        };
        let opencode =
            conversation(&context.meta).or_else(|| request.meta.as_ref().and_then(conversation));
        match self.run_tool(request, opencode).await {
            Some(outcome) => Ok(CallToolResponse::Complete(to_result(outcome))),
            None => Err(ErrorData::invalid_params(
                format!("unknown tool `{name}`"),
                None,
            )),
        }
    }
}

fn to_tool(spec: &ToolSpec) -> Tool {
    let input = as_object(&spec.input_schema);
    let output = as_object(&spec.output_schema);
    Tool::new(spec.name, spec.description, Arc::new(input))
        .with_title(spec.title)
        .with_raw_output_schema(Arc::new(output))
        .with_annotations(
            ToolAnnotations::with_title(spec.title)
                .read_only(spec.read_only)
                .destructive(spec.destructive)
                .idempotent(spec.idempotent)
                .open_world(false),
        )
}

fn as_object(schema: &serde_json::Value) -> JsonObject {
    schema.as_object().cloned().unwrap_or_default()
}

fn to_result(outcome: ToolOutcome) -> CallToolResult {
    let mut result = if outcome.is_error {
        CallToolResult::structured_error(outcome.structured)
    } else {
        CallToolResult::structured(outcome.structured)
    };
    result.content = vec![ContentBlock::text(outcome.text)];
    result
}
