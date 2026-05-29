//! The MCP tool-router for vmette.
//!
//! Exposes seven tools:
//!
//! * `execute`           — one-shot `python` / `node` / `shell` code
//! * `fetch_url`         — HTTPS GET with byte cap, returns body
//! * `workspace_create`  — allocate a scratch dir + per-call image
//! * `workspace_write`   — write a file inside a workspace
//! * `workspace_read`    — read a file from a workspace
//! * `workspace_run`     — shell command inside a microVM, workspace mounted at /mnt/work
//! * `workspace_destroy` — tear down a workspace
//!
//! All tools return their result as a single plain-text MCP content
//! block. Structured JSON-result returns are also possible via the
//! rmcp `Json` wrapper but plain text is what most agent UIs render
//! sensibly today.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::sandbox::{RunReply, RunRequest, Sandbox, Share};
use crate::workspace::{open_for_read, open_for_write, WorkspaceState};

const DEFAULT_TIMEOUT_S: u32 = 30;
const DEFAULT_WORKSPACE_TIMEOUT_S: u32 = 60;
const DEFAULT_FETCH_MAX_BYTES: usize = 20_000;

/// Tool inputs ----------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteArgs {
    /// Language: "python", "node", or "shell". Each maps to a canonical
    /// OCI image (python:3.12-alpine, node:20-alpine, alpine:3.20).
    pub language: String,
    /// Source code to execute. Quoting is handled for you.
    pub code: String,
    /// Allow outbound network from the guest (default: false). Subject
    /// to the server's --allow-network policy — denied calls return
    /// an error rather than silently running offline.
    #[serde(default)]
    pub network: bool,
    /// Wall-clock timeout in seconds (default: 30). Cap is enforced
    /// inside vmette via --timeout; a timed-out guest returns exit 124.
    pub timeout: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchUrlArgs {
    /// The URL to fetch. Only http/https are supported; redirects are
    /// followed by urllib.
    pub url: String,
    /// Cap the returned body length in bytes (default: 20000). Useful
    /// for context-window management.
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkspaceCreateArgs {
    /// OCI image used by `workspace_run` against this workspace.
    /// Default: alpine:3.20.
    pub image: Option<String>,
    /// Enable network for `workspace_run` calls. Subject to the
    /// server's --allow-network policy.
    #[serde(default)]
    pub network: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkspaceWriteArgs {
    pub workspace_id: String,
    /// Relative path inside the workspace. May not be absolute or
    /// contain `..`.
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkspaceReadArgs {
    pub workspace_id: String,
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkspaceRunArgs {
    pub workspace_id: String,
    /// Shell command — runs as `sh -c "$command"` inside the guest
    /// with the workspace mounted read-write at /mnt/work and cwd
    /// initially set to /mnt/work.
    pub command: String,
    /// Wall-clock timeout in seconds (default: 60).
    pub timeout: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkspaceDestroyArgs {
    pub workspace_id: String,
}

// --- structured returns for the workspace_create tool --------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceCreateResult {
    pub workspace_id: String,
    pub image: String,
    pub host_path: String,
}

// --- the server ----------------------------------------------------------

#[derive(Clone)]
pub struct VmetteServer {
    sandbox: Arc<Sandbox>,
    workspaces: Arc<WorkspaceState>,
    default_image: String,
    allow_network: bool,
    /// Populated by `Self::tool_router()` from the `#[tool_router]`
    /// macro; the `#[tool_handler]` macro reads it via the macro's
    /// generated code, which the dead-code lint can't see.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl VmetteServer {
    pub fn new(
        sandbox: Sandbox,
        workspaces: WorkspaceState,
        default_image: String,
        allow_network: bool,
    ) -> Self {
        Self {
            sandbox: Arc::new(sandbox),
            workspaces: Arc::new(workspaces),
            default_image,
            allow_network,
            tool_router: Self::tool_router(),
        }
    }

    /// Reject a network=true call when the server was started without
    /// --allow-network. Returns the gated `net` value (`false` when
    /// the server forbids network entirely; otherwise the request).
    fn gate_network(&self, requested: bool) -> Result<bool, ErrorData> {
        if requested && !self.allow_network {
            return Err(ErrorData::invalid_params(
                "this MCP server was started without --allow-network; \
                 set network=false or restart the server with --allow-network"
                    .to_string(),
                None,
            ));
        }
        Ok(requested)
    }
}

#[tool_router]
impl VmetteServer {
    #[tool(
        description = "Execute a code snippet (python/node/shell) in a fresh microVM. Returns stdout, stderr, and exit code. Each call boots a clean kernel; nothing persists across calls. Use workspace_* tools when you need state between turns."
    )]
    async fn execute(
        &self,
        Parameters(args): Parameters<ExecuteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let net = self.gate_network(args.network)?;
        let (rootfs, exec) = build_execute_command(&args.language, &args.code)?;
        let req = RunRequest {
            rootfs,
            exec,
            shares: Vec::new(),
            net,
            timeout_seconds: Some(args.timeout.unwrap_or(DEFAULT_TIMEOUT_S)),
            offline: false,
        };
        let reply = self
            .sandbox
            .run(&req)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(format_reply(
            &reply,
        ))]))
    }

    #[tool(
        description = "Fetch a URL via HTTP(S) GET from inside a microVM, returning status and body. Use this instead of giving the model raw web access to your host. The tool is always registered, but calls fail with an explicit error if the server was started without --allow-network. Only http:// and https:// schemes are accepted; file:// / ftp:// / data:// are rejected."
    )]
    async fn fetch_url(
        &self,
        Parameters(args): Parameters<FetchUrlArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let _ = self.gate_network(true)?;

        // Scheme validation: parse with `url` (RFC 3986-compliant) and
        // reject anything other than http/https. Without this, an agent
        // could pass `file:///etc/shadow` and urllib would happily open
        // the path inside the guest's rootfs.
        let parsed = url::Url::parse(&args.url).map_err(|e| {
            ErrorData::invalid_params(format!("invalid url {:?}: {}", args.url, e), None)
        })?;
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(ErrorData::invalid_params(
                format!("fetch_url only supports http/https, got scheme {scheme:?}"),
                None,
            ));
        }

        // Build the Python source. The URL is serialised with
        // `serde_json::to_string` because JSON string syntax is a
        // valid Python string literal — `\uXXXX` (4 hex) is what
        // Python expects, while Rust's `{:?}` would emit `\u{XXXX}`
        // (braced) which Python's tokenizer rejects.
        let max_bytes = args.max_bytes.unwrap_or(DEFAULT_FETCH_MAX_BYTES);
        let url_lit = serde_json::to_string(&args.url)
            .map_err(|e| ErrorData::internal_error(format!("url to json: {e}"), None))?;
        let py = format!(
            "import urllib.request, json; r = urllib.request.urlopen({url_lit}, timeout=10); print(json.dumps({{'status': r.status, 'body': r.read({max_bytes}).decode('utf-8','replace')}}))"
        );
        let req = RunRequest {
            rootfs: "python:3.12-alpine".into(),
            exec: shell_quoted_python(&py),
            shares: Vec::new(),
            net: true,
            timeout_seconds: Some(DEFAULT_TIMEOUT_S),
            offline: false,
        };
        let reply = self
            .sandbox
            .run(&req)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(format_reply(
            &reply,
        ))]))
    }

    #[tool(
        description = "Create a per-task workspace: a host directory owned by this MCP server that the agent can read/write via workspace_write/read and operate on with workspace_run. Returns a workspace_id token to pass to subsequent calls."
    )]
    async fn workspace_create(
        &self,
        Parameters(args): Parameters<WorkspaceCreateArgs>,
    ) -> Result<Json<WorkspaceCreateResult>, ErrorData> {
        let net = self.gate_network(args.network)?;
        let image = args.image.unwrap_or_else(|| self.default_image.clone());
        let ws = self
            .workspaces
            .create(image.clone(), net)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        Ok(Json(WorkspaceCreateResult {
            workspace_id: ws.id,
            image,
            host_path: ws.dir.display().to_string(),
        }))
    }

    #[tool(
        description = "Write a file inside a workspace. Relative path only; absolute paths and '..' rejected. Path safety: every component is opened via openat with O_DIRECTORY|O_NOFOLLOW, so an agent that creates symlinks via workspace_run (e.g. ws/escape -> /etc) cannot trick subsequent writes into following them. Overwrites existing files; creates missing parent dirs."
    )]
    async fn workspace_write(
        &self,
        Parameters(args): Parameters<WorkspaceWriteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ws = self
            .workspaces
            .get(&args.workspace_id)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let fd = open_for_write(&ws.dir, &args.path).map_err(|e| {
            // Path safety / not-found errors are caller-side: surface as invalid_params.
            ErrorData::invalid_params(e.to_string(), None)
        })?;
        let mut file = std::fs::File::from(fd);
        use std::io::Write;
        file.write_all(args.content.as_bytes())
            .map_err(|e| ErrorData::internal_error(format!("write {}: {}", args.path, e), None))?;
        let written = args.content.len();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "wrote {} ({} bytes)",
            args.path, written
        ))]))
    }

    #[tool(
        description = "Read a file from a workspace. Relative path only. Same openat+NOFOLLOW path-safety as workspace_write."
    )]
    async fn workspace_read(
        &self,
        Parameters(args): Parameters<WorkspaceReadArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ws = self
            .workspaces
            .get(&args.workspace_id)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let fd = open_for_read(&ws.dir, &args.path)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let mut file = std::fs::File::from(fd);
        use std::io::Read;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| ErrorData::internal_error(format!("read {}: {}", args.path, e), None))?;
        Ok(CallToolResult::success(vec![Content::text(
            String::from_utf8_lossy(&bytes).into_owned(),
        )]))
    }

    #[tool(
        description = "Run a shell command inside the workspace's microVM. The workspace directory is mounted at /mnt/work (read-write) and is the initial cwd. The exact exec is `sh -c \"cd /mnt/work && <your-command>\"`, so don't begin `command` with a shell operator like `&&` or `||`. Image and network policy were set at workspace_create time."
    )]
    async fn workspace_run(
        &self,
        Parameters(args): Parameters<WorkspaceRunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ws = self
            .workspaces
            .get(&args.workspace_id)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        // Run from /mnt/work so relative paths in the command DTRT.
        let exec = format!("cd /mnt/work && {}", args.command);
        let req = RunRequest {
            rootfs: ws.image.clone(),
            exec,
            shares: vec![Share {
                tag: "work".into(),
                path: ws.dir.clone(),
            }],
            net: ws.net,
            timeout_seconds: Some(args.timeout.unwrap_or(DEFAULT_WORKSPACE_TIMEOUT_S)),
            offline: false,
        };
        let reply = self
            .sandbox
            .run(&req)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(format_reply(
            &reply,
        ))]))
    }

    #[tool(
        description = "Destroy a workspace: remove its host directory and forget the id. Idempotent."
    )]
    async fn workspace_destroy(
        &self,
        Parameters(args): Parameters<WorkspaceDestroyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(e) = self.workspaces.destroy(&args.workspace_id) {
            warn!(error = %e, id = %args.workspace_id, "destroy failed");
        }
        Ok(CallToolResult::success(vec![Content::text(format!(
            "workspace {} destroyed",
            args.workspace_id
        ))]))
    }
}

#[tool_handler]
impl ServerHandler for VmetteServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "vmette MCP server: sandboxed code execution and per-task workspaces \
                 on macOS via Apple's Virtualization.framework. Each tool call boots \
                 a fresh Linux microVM (~1s) and tears it down when the call returns. \
                 Use `execute` for one-shot code; use `workspace_*` for state across turns."
                .to_string(),
        )
    }
}

// --- helpers -------------------------------------------------------------

/// Translate (language, code) → (rootfs OCI ref, shell exec). The
/// shell exec uses single-quote escaping (via the embedded helper)
/// so embedded quotes, $, backticks, and newlines all round-trip
/// safely as one shell argument.
fn build_execute_command(language: &str, code: &str) -> Result<(String, String), ErrorData> {
    let (rootfs, runner) = match language {
        "python" => ("python:3.12-alpine", "python3 -c"),
        "node" => ("node:20-alpine", "node -e"),
        "shell" => ("alpine:3.20", "sh -c"),
        other => {
            return Err(ErrorData::invalid_params(
                format!("unknown language {other:?}; supported: python, node, shell"),
                None,
            ));
        }
    };
    let exec = format!("{runner} {}", single_quote(code));
    Ok((rootfs.into(), exec))
}

/// Shell single-quote a string for safe use as one argument to /bin/sh.
/// Single quotes preserve everything literally except embedded single
/// quotes, which we close, escape, and reopen.
fn single_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Specialised wrapper that builds `python3 -c '<code>'` shell args.
fn shell_quoted_python(code: &str) -> String {
    format!("python3 -c {}", single_quote(code))
}

fn format_reply(r: &RunReply) -> String {
    // A compact, model-readable summary. Stderr is included only when
    // non-empty since vmette's own banner is verbose and rarely useful
    // to the agent.
    let mut out = format!("exit: {}\n\nstdout:\n{}", r.exit, r.stdout);
    if !r.stderr.trim().is_empty() {
        out.push_str("\n\nstderr:\n");
        out.push_str(&r.stderr);
    }
    out
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_quote_handles_quotes_and_meta() {
        assert_eq!(single_quote("hello"), "'hello'");
        assert_eq!(single_quote("it's"), "'it'\\''s'");
        assert_eq!(single_quote("$(foo)"), "'$(foo)'");
        assert_eq!(single_quote("`bar`"), "'`bar`'");
        assert_eq!(single_quote("a\nb"), "'a\nb'");
    }

    #[test]
    fn build_execute_command_known_languages() {
        let (rootfs, exec) = build_execute_command("python", "print(1)").unwrap();
        assert_eq!(rootfs, "python:3.12-alpine");
        assert!(exec.starts_with("python3 -c "));
        assert!(exec.contains("print(1)"));

        let (rootfs, exec) = build_execute_command("node", "console.log(1)").unwrap();
        assert_eq!(rootfs, "node:20-alpine");
        assert!(exec.starts_with("node -e "));

        let (rootfs, exec) = build_execute_command("shell", "echo hi").unwrap();
        assert_eq!(rootfs, "alpine:3.20");
        assert!(exec.starts_with("sh -c "));
    }

    #[test]
    fn build_execute_command_rejects_unknown_language() {
        assert!(build_execute_command("ruby", "puts 1").is_err());
    }

    #[test]
    fn format_reply_omits_empty_stderr() {
        let r = RunReply {
            stdout: "hi".into(),
            stderr: "".into(),
            exit: 0,
        };
        let f = format_reply(&r);
        assert!(f.contains("exit: 0"));
        assert!(f.contains("hi"));
        assert!(!f.contains("stderr"));
    }
}
