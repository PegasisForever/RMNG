//! `rmng exec` DTOs — a docker-exec-style single non-interactive command run inside a
//! clone. The `rmng` CLI POSTs [`ExecRequest`] to `/api/hosts/:id/exec`; the
//! control-server runs it via the bollard Docker socket and returns [`ExecResult`].
//! No TTY, no streaming — stdout/stderr are captured separately and buffered
//! (UTF-8-lossy; binary output is out of scope).

use serde::{Deserialize, Serialize};

/// A command to run inside a clone (no TTY). `cmd` is the argv (empty → 400). `user`
/// defaults to uid `1000` (the clone's agent user) server-side when `None`. `env`
/// entries are `KEY=VAL`; `stdin_b64` is base64-encoded stdin fed to the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecRequest {
    /// The argv to execute (`["ls", "-la"]`); empty is rejected with 400.
    pub cmd: Vec<String>,
    /// Run-as user (uid or name). `None` ⇒ the server defaults to uid `1000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Working directory inside the container. `None` ⇒ the image default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// Extra environment, one `KEY=VAL` entry each.
    #[serde(default)]
    pub env: Vec<String>,
    /// Base64-encoded stdin fed to the command, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_b64: Option<String>,
    /// Fire-and-forget: launch the command detached (in the background) and return immediately,
    /// with no captured stdout/stderr and no exit code. Intended for GUI apps on the clone desktop
    /// (`rmng exec -d`). `stdin_b64` is ignored in this mode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detach: bool,
}

/// The result of an [`ExecRequest`]: the command's real exit code plus its captured
/// stdout/stderr (UTF-8-lossy, streams kept separate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}
