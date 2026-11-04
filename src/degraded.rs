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
    /// The program whose Full Disk Access macOS checks (macOS only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions_checked_for: Option<String>,
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
        let checked_for = crate::responsibility::checked_for();
        let mut how_to_fix = vec![
            "Claude can open the right settings pane and show the file: call open_permission_settings.".into(),
            "Open System Settings › Privacy & Security › Full Disk Access.".into(),
            format!("Click +, press ⌘⇧G, paste this path and enable it: {binary}"),
            "Then turn the Timbre extension off and on again in Claude's settings.".into(),
        ];
        if let Some(other) = checked_for.as_deref().filter(|w| !w.starts_with("timbre ")) {
            // The relaunch did not happen: macOS checks the launching app.
            how_to_fix.push(format!(
                "macOS checks the access of {other}, which started Timbre, instead of Timbre's own: \
                 give Full Disk Access to that program, or update Timbre."
            ));
        }
        Ok(Json(Problem { problem: self.error.clone(), how_to_fix, permissions_checked_for: checked_for }))
    }

    #[tool(
        name = "open_permission_settings",
        description = "Opens System Settings at Full Disk Access and selects Timbre's program in Finder, so the user only drags it into the list and switches it on.",
        annotations(
            title = "Open Full Disk Access settings",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn open_permission_settings(&self) -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if !cfg!(target_os = "macos") {
            return Err("Only available on macOS.".into());
        }
        let open = |args: &[&std::ffi::OsStr]| {
            std::process::Command::new("/usr/bin/open").args(args).status().is_ok_and(|s| s.success())
        };
        let pane = open(&["x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles".as_ref()]);
        let shown = open(&["-R".as_ref(), exe.as_os_str()]);
        Ok(format!(
            "{}{} Ask the user to drag the selected file \"{}\" from Finder into the Full Disk Access list (or click + and pick it), \
             switch it on, then turn the Timbre extension off and on in Claude's settings.",
            if pane { "System Settings is open at Full Disk Access. " } else { "Could not open System Settings. " },
            if shown { "Finder shows Timbre's program, selected." } else { "Could not show the program in Finder." },
            exe.display()
        ))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Degraded {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("timbre", env!("CARGO_PKG_VERSION")).with_title("Timbre"))
            .with_instructions(
                "Timbre cannot read Mail's data yet. Call mail_status; open_permission_settings opens the right \
                 System Settings pane and shows the file to add.",
            )
    }
}
