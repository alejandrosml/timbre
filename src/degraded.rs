//! Minimal server used when Mail's data cannot be opened (usually missing
//! Full Disk Access). It answers with clear instructions instead of failing
//! to start, so the user learns what to fix.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Clone)]
pub struct Degraded {
    error: String,
    tool_router: ToolRouter<Degraded>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Problem {
    pub problem: String,
    pub how_to_fix: Vec<String>,
}

#[tool_router]
impl Degraded {
    pub fn new(error: String) -> Self {
        Degraded { error, tool_router: Self::tool_router() }
    }

    #[tool(
        name = "mail_status",
        description = "Explains why Timbre cannot read Mail yet and how the user can fix it.",
        annotations(title = "Timbre status", read_only_hint = true, open_world_hint = false)
    )]
    async fn mail_status(&self) -> Result<Json<Problem>, String> {
        let binary = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
        Ok(Json(Problem {
            problem: self.error.clone(),
            how_to_fix: vec![
                "Open System Settings › Privacy & Security › Full Disk Access.".into(),
                format!("Click +, press ⌘⇧G, paste this path and enable it: {binary}"),
                "Then turn the Timbre extension off and on again in Claude's settings.".into(),
            ],
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Degraded {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("timbre", env!("CARGO_PKG_VERSION")).with_title("Timbre"))
            .with_instructions("Timbre cannot read Mail's data yet. Call mail_status and show the user the steps to fix it.")
    }
}
