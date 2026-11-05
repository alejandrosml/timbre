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

use std::sync::{Arc, Mutex};

use crate::config::Config;

pub struct Degraded {
    /// Why Mail's data cannot be read, kept current while Timbre waits.
    why: Arc<Mutex<String>>,
    info: ServerConfig,
    tools: Vec<Tool>,
    /// Running under the launcher: access is picked up without a restart.
    restarts_by_itself: bool,
}

/// What keeps Timbre from working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// macOS does not let Timbre read Mail's data (Full Disk Access).
    Access,
    /// Apple Mail has no data on this Mac yet.
    NoMail,
    /// Timbre's own index could not be opened.
    Index,
    Other,
}

/// Sort an error message into a [`Problem`].
pub fn problem_of(error: &str) -> Problem {
    let e = error.to_lowercase();
    if e.contains("full disk access") || e.contains("operation not permitted") || e.contains("authorization denied") {
        Problem::Access
    } else if e.contains("no v*/maildata") || (e.contains("cannot read") && e.contains("no such file")) {
        Problem::NoMail
    } else if e.starts_with("timbre's own index") {
        Problem::Index
    } else {
        Problem::Other
    }
}

impl Degraded {
    pub fn new(why: Arc<Mutex<String>>, cfg: &Config) -> Self {
        Degraded {
            why,
            info: crate::server::server_info(cfg),
            tools: crate::server::tool_list(cfg),
            restarts_by_itself: crate::launcher::launched(),
        }
    }

    fn problem(&self) -> String {
        let error = self.why.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let by_itself = self.restarts_by_itself;
        match problem_of(&error) {
            Problem::Access => {
                let app = crate::util::app_path();
                let mut text = format!(
                    "Timbre cannot read Mail's data yet: {error}\n\n\
                     How to fix it: call open_permission_settings. System Settings opens at Privacy & Security › \
                     Full Disk Access and Finder selects Timbre; the user drags Timbre into the list and switches it on \
                     (or clicks +, presses ⌘⇧G and pastes {}).",
                    app.display()
                );
                text.push_str(if by_itself {
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
            Problem::NoMail => format!(
                "Apple Mail has no data on this Mac yet ({error}). Ask the user to open Mail, add their email \
                 accounts and let Mail download the messages. {}",
                if by_itself {
                    "Timbre starts working by itself once Mail's data is there."
                } else {
                    "Then turn the Timbre extension off and on in Claude's settings."
                }
            ),
            Problem::Index => format!(
                "{error}\n\nAsk the user to turn the Timbre extension off and on in Claude's settings (or to quit \
                 and reopen Claude, if another Claude window may be running an older Timbre)."
            ),
            Problem::Other => format!(
                "Timbre cannot open Mail's data: {error}\n\nAsk the user to check that Mail opens and shows their \
                 messages, and to update Timbre ({}). {}",
                env!("CARGO_PKG_REPOSITORY"),
                if by_itself { "Timbre retries by itself every few seconds." } else { "" }
            ),
        }
    }
}

/// Opens System Settings at Full Disk Access and selects Timbre in Finder.
pub fn open_permission_settings() -> String {
    if !cfg!(target_os = "macos") {
        return "Only available on macOS.".into();
    }
    let app = crate::util::app_path();
    let open = |args: &[&std::ffi::OsStr]| {
        std::process::Command::new("/usr/bin/open")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
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

/// Open System Settings where the user lets Timbre act in Mail.
pub fn open_automation_settings() -> String {
    if !cfg!(target_os = "macos") {
        return "Only available on macOS.".into();
    }
    let opened = std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    format!(
        "{} Ask the user to find Timbre in the list, switch on Mail under it, then try again. If Timbre is not \
         listed, macOS asks the next time Timbre acts in Mail: the user answers OK.",
        if opened { "System Settings is open at Automation." } else { "Could not open System Settings." }
    )
}

impl ServerHandler for Degraded {
    fn get_info(&self) -> ServerConfig {
        self.info.clone()
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
        if !self.tools.iter().any(|t| t.name == request.name) {
            return Err(ErrorData::invalid_params("tool not found", None));
        }
        if request.name == "open_permission_settings" {
            let automation = request
                .arguments
                .as_ref()
                .and_then(|a| a.get("pane"))
                .and_then(|v| v.as_str())
                .is_some_and(|p| p.trim().eq_ignore_ascii_case("automation"));
            let text = tokio::task::spawn_blocking(move || {
                if automation { open_automation_settings() } else { open_permission_settings() }
            })
            .await
            .unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into());
        }
        Ok(CallToolResult::error(vec![ContentBlock::text(self.problem())]).into())
    }

    /// No schema check on these answers: they are explanations, not results.
    fn get_tool(&self, _name: &str) -> Option<Tool> {
        None
    }
}
