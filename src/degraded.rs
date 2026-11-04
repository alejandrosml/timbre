//! The server Timbre runs when Mail's data cannot be opened yet (usually
//! missing Full Disk Access).
//!
//! It offers exactly the tools of the full server, under the same
//! instructions: each one explains what is missing, and
//! `open_permission_settings` opens the right System Settings pane. Under
//! the launcher, Timbre then restarts by itself once access is granted, and
//! Claude keeps the same session and tools.

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams,
    ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};

use crate::config::Config;

pub struct Degraded {
    error: String,
    tools: Vec<Tool>,
    /// Running under the launcher: access is picked up without a restart.
    restarts_by_itself: bool,
}

impl Degraded {
    pub fn new(error: String, cfg: &Config) -> Self {
        Degraded { error, tools: crate::server::tool_list(cfg), restarts_by_itself: crate::launcher::launched() }
    }

    fn problem(&self) -> String {
        let app = crate::util::app_path();
        let mut text = format!(
            "Timbre cannot read Mail's data yet: {}\n\n\
             How to fix it: call open_permission_settings. System Settings opens at Privacy & Security › \
             Full Disk Access and Finder selects Timbre; the user drags Timbre into the list and switches it on \
             (or clicks +, presses ⌘⇧G and pastes {}).",
            self.error,
            app.display()
        );
        text.push_str(if self.restarts_by_itself {
            " Timbre notices within seconds and starts working by itself: nothing to restart."
        } else {
            " Then the user turns the Timbre extension off and on in Claude's settings."
        });
        if let Some(other) = crate::responsibility::checked_for().filter(|w| !w.starts_with("timbre ")) {
            text.push_str(&format!(
                "\n\nmacOS checks the access of {other}, which started Timbre, instead of Timbre's own: \
                 give Full Disk Access to that program, or update Timbre."
            ));
        }
        text
    }
}

/// Opens System Settings at Full Disk Access and selects Timbre in Finder.
pub fn open_permission_settings() -> String {
    if !cfg!(target_os = "macos") {
        return "Only available on macOS.".into();
    }
    let app = crate::util::app_path();
    let open = |args: &[&std::ffi::OsStr]| {
        std::process::Command::new("/usr/bin/open").args(args).status().is_ok_and(|s| s.success())
    };
    let pane = open(&["x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles".as_ref()]);
    let shown = open(&["-R".as_ref(), app.as_os_str()]);
    format!(
        "{}{} Ask the user to drag the selected \"{}\" from Finder into the Full Disk Access list (or click + and \
         pick it) and switch it on. {}",
        if pane { "System Settings is open at Full Disk Access. " } else { "Could not open System Settings. " },
        if shown { "Finder shows Timbre, selected." } else { "Could not show Timbre in Finder." },
        app.display(),
        if crate::launcher::launched() {
            "Timbre starts working by itself within seconds."
        } else {
            "Then turn the Timbre extension off and on in Claude's settings."
        }
    )
}

impl ServerHandler for Degraded {
    fn get_info(&self) -> ServerConfig {
        crate::server::server_info()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name == "open_permission_settings" {
            let text = tokio::task::spawn_blocking(open_permission_settings).await.unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into());
        }
        Ok(CallToolResult::error(vec![ContentBlock::text(self.problem())]).into())
    }

    /// No schema check on these answers: they are explanations, not results.
    fn get_tool(&self, _name: &str) -> Option<Tool> {
        None
    }
}
