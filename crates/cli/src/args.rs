//! The clap command tree. Fleet management only — driving the agents *inside*
//! clones is the desktop MCP's job (computer use), and code moves via git.
//!
//! Structure is uniform **noun → verb**: `rmng <noun> <verb> [<clone>] [flags]`. The nouns are
//! `clone` (the fleet unit), `image`, `account`, `op`, and `desktop`. One list verb (`ls`), one
//! destroy verb (`rm`); the target is always a positional `<clone>`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Default seconds to wait on an operation before giving up (shared by every `--wait`/`op wait`).
const DEFAULT_TIMEOUT: u64 = 600;

#[derive(Parser, Debug)]
#[command(
    name = "rmng",
    version,
    about = "Fleet management for the RMNG control-server",
    long_about = "Fleet management for the RMNG control-server.\n\n\
                  Inside a clone the server is auto-resolved from $RMNG_CONTROL_URL, so bare \
                  `rmng …` just works. Otherwise: --server > $RMNG_CONTROL_URL > \
                  http://localhost:9000."
)]
pub struct Cli {
    /// Control-server web-API origin (e.g. http://rmng-control:9000)
    #[arg(long, global = true, value_name = "URL")]
    pub server: Option<String>,
    /// Emit machine-readable JSON instead of a table (honored by every command)
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Manage clones (the fleet unit): ls / create / rm / archive / restore / ssh / exec / …
    #[command(subcommand)]
    Clone(CloneCmd),
    /// Clone-source image operations
    #[command(subcommand)]
    Image(ImageCmd),
    /// Imported-account operations
    #[command(subcommand)]
    Account(AccountCmd),
    /// Operation (clone / delete / archive / pull / commit / update) inspection
    #[command(subcommand)]
    Op(OpCmd),
    /// Drive a clone's desktop via its daemon MCP (screenshot-on-every-action)
    Desktop {
        /// Clone id
        clone: String,
        #[command(subcommand)]
        cmd: DesktopCmd,
    },
}

/// `rmng clone <verb>` — everything that acts on the fleet unit.
#[derive(Subcommand, Debug)]
pub enum CloneCmd {
    /// List clones with live CPU, RAM, token totals, activity, and account-group assignment
    Ls,
    /// Create a clone under an exact hostname
    Create {
        /// Exact hostname for the new clone (DNS label)
        hostname: String,
        /// Clone-source image reference to create from (see `rmng image ls`)
        #[arg(long)]
        from: String,
        /// Account group to route this clone's agents through. Omitted inside a clone ⇒ inherit
        /// the parent's group; use --no-group to bind none.
        #[arg(long)]
        group: Option<String>,
        /// Bind no account group (opt out of inheriting the parent's)
        #[arg(long, conflicts_with = "group")]
        no_group: bool,
        /// Env preset name. Omitted inside a clone ⇒ inherit the parent's preset; use
        /// --no-preset for none.
        #[arg(long)]
        preset: Option<String>,
        /// Use no env preset (opt out of inheriting the parent's)
        #[arg(long, conflicts_with = "preset")]
        no_preset: bool,
        /// Headless clone: no desktop; the viewer shows a tmux tab view instead of a stream
        #[arg(long)]
        headless: bool,
        /// Create as a sub clone under this parent clone id (must be top-level). Overrides the
        /// default caller auto-detection. Conflicts with --top-level.
        #[arg(long, conflicts_with = "top_level")]
        parent: Option<String>,
        /// Force a top-level clone even when run from inside a clone (skip auto-nesting)
        #[arg(long)]
        top_level: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Destroy a clone (container + volumes). Non-interactive callers must pass -y.
    Rm {
        /// Clone id
        clone: String,
        /// Skip the confirmation prompt (required when not attached to a terminal)
        #[arg(short = 'y', long)]
        yes: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Stop a clone but retain its container, volumes, notes, and chat
    Archive {
        /// Clone id
        clone: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Restart a retained archived clone
    Restore {
        /// Clone id
        clone: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Print the ready-to-paste `ssh` command for a clone
    Ssh {
        /// Clone id
        clone: String,
    },
    /// Run a single non-interactive command inside a clone (docker-exec-style)
    Exec {
        /// Clone id
        clone: String,
        /// Run-as user (uid or name); defaults to the clone's agent user server-side
        #[arg(short = 'u', long)]
        user: Option<String>,
        /// Working directory inside the container
        #[arg(short = 'w', long)]
        workdir: Option<String>,
        /// Extra environment `KEY=VAL` (repeatable)
        #[arg(short = 'e', long)]
        env: Vec<String>,
        /// The command argv, after `--` (e.g. `rmng clone exec c -- ls -la`)
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Bind a clone to an account group
    Bind {
        /// Clone id
        clone: String,
        /// Account group name to bind (omit and pass --none to clear)
        group: Option<String>,
        /// Clear the clone's account-group binding
        #[arg(long, conflicts_with = "group")]
        none: bool,
    },
    /// Point the operator's viewer at a clone (operator-only; no effect on command targeting)
    Select {
        /// Clone id (omit and pass --none to clear the selection)
        clone: Option<String>,
        /// Clear the viewer selection
        #[arg(long, conflicts_with = "clone")]
        none: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ImageCmd {
    /// List clone-source images
    Ls,
    /// Pull the clone template from a registry (default: the configured reference)
    Pull {
        /// Registry reference (e.g. pegasis0/rmng-template:latest)
        reference: Option<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Commit a running clone to a new clone-source image `<name>:latest`
    Commit {
        /// Clone id to commit
        clone: String,
        /// Image name (DNS label; becomes the repo of `<name>:latest`)
        #[arg(long = "as", value_name = "NAME")]
        as_name: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Remove a clone-source image (fails while clones use it)
    Rm {
        /// Image reference or id
        reference: String,
    },
}

/// Account provider filter for `rmng account ls --provider <p>`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum Provider {
    Claude,
    Codex,
    Gemini,
}

#[derive(Subcommand, Debug)]
pub enum AccountCmd {
    /// List imported accounts with usage windows (all providers by default)
    Ls {
        /// Only show accounts for this provider
        #[arg(long)]
        provider: Option<Provider>,
    },
}

#[derive(Subcommand, Debug)]
pub enum OpCmd {
    /// List operations (clone / delete / archive / restore / pull / commit / update)
    Ls,
    /// Wait for an operation to reach a terminal state
    Wait {
        /// Operation id (as printed by clone/image commands)
        op_id: String,
        /// Give up after this many seconds
        #[arg(long, default_value_t = DEFAULT_TIMEOUT)]
        timeout: u64,
    },
}

/// `--wait [--timeout N]` shared by the operation-starting commands.
#[derive(Args, Debug)]
pub struct WaitArgs {
    /// Block until the operation finishes (rides the /events SSE stream)
    #[arg(long)]
    pub wait: bool,
    /// Seconds to wait before giving up (with --wait)
    #[arg(long, default_value_t = DEFAULT_TIMEOUT)]
    pub timeout: u64,
}

/// Optional `--rescale-cursor <range>` (and optional `--rescale-screen <W>x<H>`)
/// for desktop action verbs. Two independent knobs:
///
///   * `--rescale-cursor <range>` rescales the verb's **input** X Y from a
///     normalized coord space (e.g. MiniMax M3's 0–1000) into pixel coords
///     before calling the daemon. Two modes:
///       - alone: the CLI issues one extra `list_monitors` call per action to
///         discover the target monitor's W×H.
///       - with `--rescale-screen`: uses the caller's W×H directly, skipping
///         the auto-detect RPC. Useful for repeated calls where the screen
///         size is known and stable.
///
///   * `--rescale-screen <W>x<H>` rescales the **output screenshot** the
///     daemon returns (both explicit `screenshot` and the auto-snap after
///     action verbs) to the requested W×H. Saves tokens when feeding the
///     image back to a model. Independent of `--rescale-cursor` — you can use
///     either, both, or neither.
#[derive(Args, Debug, Clone, Default)]
pub struct RescaleArgs {
    /// Rescale input X Y from this coord space into the target monitor's pixel
    /// space. Accepts `<max>` (assumes 0-based) or `<min>-<max>` (e.g. `0-1000`
    /// for MiniMax M3).
    #[arg(long, value_name = "RANGE")]
    pub rescale_cursor: Option<String>,
    /// Rescale the **screenshot** the daemon returns to `<W>x<H>` pixels.
    /// Independent of `--rescale-cursor`: useful for shrinking the JPEG before
    /// feeding it to a vision model (saves tokens), or upscaling for a clearer
    /// view. Works on every desktop verb (the action verbs auto-snap a settle
    /// screenshot after the call).
    #[arg(long, value_name = "WxH")]
    pub rescale_screen: Option<String>,
}

impl RescaleArgs {
    /// Parse `--rescale-cursor` into `(min, max)` source-space bounds, or
    /// `None` if unset. Returns an error for malformed input so the CLI can
    /// fail fast rather than silently sending nonsense coords to the daemon.
    pub fn parsed_cursor(&self) -> Result<Option<(i32, i32)>, String> {
        let Some(s) = self.rescale_cursor.as_deref() else {
            return Ok(None);
        };
        if let Some((lo, hi)) = s.split_once('-') {
            let lo: i32 = lo
                .parse()
                .map_err(|e| format!("--rescale-cursor: bad min: {e}"))?;
            let hi: i32 = hi
                .parse()
                .map_err(|e| format!("--rescale-cursor: bad max: {e}"))?;
            if hi <= lo {
                return Err("--rescale-cursor: max must be > min".into());
            }
            Ok(Some((lo, hi)))
        } else {
            let hi: i32 = s
                .parse()
                .map_err(|e| format!("--rescale-cursor: bad max: {e}"))?;
            if hi <= 0 {
                return Err("--rescale-cursor: max must be > 0".into());
            }
            Ok(Some((0, hi)))
        }
    }

    /// Parse `--rescale-screen` into `(W, H)` pixels, or `None` if unset. Only
    /// meaningful when `--rescale-from` is also set (enforced by clap's
    /// `requires`); we still tolerate the call without `rescale_from` for
    /// unit-testability.
    pub fn parsed_screen(&self) -> Result<Option<(i32, i32)>, String> {
        let Some(s) = self.rescale_screen.as_deref() else {
            return Ok(None);
        };
        let (w, h) = s
            .split_once('x')
            .or_else(|| s.split_once('X'))
            .ok_or_else(|| format!("--rescale-screen: expected WxH, got '{s}'"))?;
        let w: i32 = w
            .parse()
            .map_err(|e| format!("--rescale-screen: bad W: {e}"))?;
        let h: i32 = h
            .parse()
            .map_err(|e| format!("--rescale-screen: bad H: {e}"))?;
        if w <= 0 || h <= 0 {
            return Err("--rescale-screen: W and H must be > 0".into());
        }
        Ok(Some((w, h)))
    }
}

/// The `rmng desktop <clone> …` verbs. Each maps 1:1 to a daemon-MCP tool; action
/// verbs guarantee a post-action screenshot (see `commands::desktop`).
#[derive(Subcommand, Debug)]
pub enum DesktopCmd {
    /// Capture a screenshot (→ `screenshot`)
    Screenshot {
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rescale: RescaleArgs,
    },
    /// List monitors (→ `list_monitors`)
    Monitors,
    /// List windows (→ `list_windows`)
    Windows,
    /// List launchable apps (→ `list_apps`)
    Apps,
    /// Move the mouse to X Y (→ `mouse_move`)
    Move {
        x: i32,
        y: i32,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rescale: RescaleArgs,
    },
    /// Left click, optionally at X Y (→ `left_click`)
    Click {
        x: Option<i32>,
        y: Option<i32>,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rescale: RescaleArgs,
    },
    /// Right click, optionally at X Y (→ `right_click`)
    RightClick {
        x: Option<i32>,
        y: Option<i32>,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rescale: RescaleArgs,
    },
    /// Middle click, optionally at X Y (→ `middle_click`)
    MiddleClick {
        x: Option<i32>,
        y: Option<i32>,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rescale: RescaleArgs,
    },
    /// Left double click, optionally at X Y (→ `left_double_click`)
    DoubleClick {
        x: Option<i32>,
        y: Option<i32>,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rescale: RescaleArgs,
    },
    /// Scroll by AMOUNT, optionally at X Y (→ `scroll`)
    Scroll {
        amount: i32,
        x: Option<i32>,
        y: Option<i32>,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rescale: RescaleArgs,
    },
    /// Press a key chord, e.g. `ctrl+c` (→ `key`)
    Key {
        /// Key chord (e.g. `ctrl+c`, `Return`)
        keys: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Type literal text (→ `type`)
    Type {
        /// The text to type
        text: String,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Launch an app by id, e.g. `firefox.desktop` (→ `launch_app`)
    Launch {
        /// App/desktop-entry id
        id: String,
    },
    /// Move/arrange a window by id (→ `move_window`)
    MoveWindow {
        /// Window id
        id: String,
        #[arg(long)]
        monitor: Option<u32>,
        /// Placement mode, e.g. `maximize` / `center-half`
        #[arg(long)]
        mode: Option<String>,
    },
}

/// `--server` > `$RMNG_CONTROL_URL` > localhost default.
pub fn resolve_server(flag: Option<String>, env: Option<String>) -> String {
    flag.filter(|s| !s.trim().is_empty())
        .or(env.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "http://localhost:9000".to_string())
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_clone_ls() {
        let cli = Cli::parse_from(["rmng", "clone", "ls"]);
        assert!(matches!(cli.cmd, Cmd::Clone(CloneCmd::Ls)));
        assert!(!cli.json);
    }

    #[test]
    fn global_flags_work_after_subcommand() {
        let cli = Cli::parse_from(["rmng", "clone", "ls", "--json", "--server", "http://x:9000"]);
        assert!(cli.json);
        assert_eq!(cli.server.as_deref(), Some("http://x:9000"));
    }

    #[test]
    fn clone_create_positional_hostname_and_from() {
        let cli = Cli::parse_from([
            "rmng", "clone", "create", "w-cp", "--from", "tmpl:latest", "--group", "pooled",
            "--wait", "--timeout", "120",
        ]);
        match cli.cmd {
            Cmd::Clone(CloneCmd::Create {
                hostname, from, group, no_group, preset, no_preset, headless, parent, top_level,
                wait,
            }) => {
                assert_eq!(hostname, "w-cp");
                assert_eq!(from, "tmpl:latest");
                assert_eq!(group.as_deref(), Some("pooled"));
                assert!(!no_group && !no_preset && !headless && !top_level);
                assert_eq!(preset, None);
                assert_eq!(parent, None);
                assert!(wait.wait);
                assert_eq!(wait.timeout, 120);
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    #[test]
    fn clone_create_mutually_exclusive_flags() {
        // --parent ⊕ --top-level, --group ⊕ --no-group, --preset ⊕ --no-preset.
        assert!(Cli::try_parse_from([
            "rmng", "clone", "create", "w-x", "--from", "i", "--parent", "p", "--top-level",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "rmng", "clone", "create", "w-x", "--from", "i", "--group", "g", "--no-group",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "rmng", "clone", "create", "w-x", "--from", "i", "--preset", "p", "--no-preset",
        ])
        .is_err());
        // --from is required.
        assert!(Cli::try_parse_from(["rmng", "clone", "create", "w-x"]).is_err());
    }

    #[test]
    fn clone_rm_requires_clone() {
        assert!(Cli::try_parse_from(["rmng", "clone", "rm"]).is_err());
        let cli = Cli::parse_from(["rmng", "clone", "rm", "w-cp", "-y"]);
        assert!(matches!(
            cli.cmd,
            Cmd::Clone(CloneCmd::Rm { ref clone, yes: true, .. }) if clone == "w-cp"
        ));
    }

    #[test]
    fn clone_archive_and_restore_parse_with_wait() {
        let archive = Cli::parse_from(["rmng", "clone", "archive", "w-cp", "--wait"]);
        assert!(matches!(
            archive.cmd,
            Cmd::Clone(CloneCmd::Archive { ref clone, ref wait }) if clone == "w-cp" && wait.wait
        ));
        let restore = Cli::parse_from(["rmng", "clone", "restore", "w-cp"]);
        assert!(matches!(
            restore.cmd,
            Cmd::Clone(CloneCmd::Restore { ref clone, .. }) if clone == "w-cp"
        ));
    }

    #[test]
    fn clone_bind_and_select_none_flags() {
        let bind = Cli::parse_from(["rmng", "clone", "bind", "w-cp", "pooled"]);
        assert!(matches!(
            bind.cmd,
            Cmd::Clone(CloneCmd::Bind { ref clone, group: Some(ref g), none: false })
                if clone == "w-cp" && g == "pooled"
        ));
        let unbind = Cli::parse_from(["rmng", "clone", "bind", "w-cp", "--none"]);
        assert!(matches!(
            unbind.cmd,
            Cmd::Clone(CloneCmd::Bind { none: true, group: None, .. })
        ));
        // group + --none conflict.
        assert!(Cli::try_parse_from(["rmng", "clone", "bind", "w-cp", "pooled", "--none"]).is_err());
        let sel = Cli::parse_from(["rmng", "clone", "select", "--none"]);
        assert!(matches!(sel.cmd, Cmd::Clone(CloneCmd::Select { none: true, clone: None })));
    }

    #[test]
    fn op_ls_and_wait() {
        assert!(matches!(
            Cli::parse_from(["rmng", "op", "ls"]).cmd,
            Cmd::Op(OpCmd::Ls)
        ));
        let w = Cli::parse_from(["rmng", "op", "wait", "op_123", "--timeout", "30"]);
        assert!(matches!(
            w.cmd,
            Cmd::Op(OpCmd::Wait { ref op_id, timeout: 30 }) if op_id == "op_123"
        ));
    }

    #[test]
    fn image_commit_takes_name_as_flag() {
        let cli = Cli::parse_from(["rmng", "image", "commit", "w-cp", "--as", "myimg"]);
        assert!(matches!(
            cli.cmd,
            Cmd::Image(ImageCmd::Commit { ref clone, ref as_name, .. })
                if clone == "w-cp" && as_name == "myimg"
        ));
        // --as is required.
        assert!(Cli::try_parse_from(["rmng", "image", "commit", "w-cp"]).is_err());
    }

    #[test]
    fn account_ls_provider_enum() {
        let cli = Cli::parse_from(["rmng", "account", "ls", "--provider", "codex"]);
        assert!(matches!(
            cli.cmd,
            Cmd::Account(AccountCmd::Ls { provider: Some(Provider::Codex) })
        ));
        // Bad provider rejected.
        assert!(Cli::try_parse_from(["rmng", "account", "ls", "--provider", "bogus"]).is_err());
    }

    #[test]
    fn clone_exec_separates_command_after_dashes() {
        let cli = Cli::parse_from([
            "rmng", "clone", "exec", "c", "-u", "root", "-w", "/srv", "-e", "A=1", "-e", "B=2",
            "--", "env",
        ]);
        match cli.cmd {
            Cmd::Clone(CloneCmd::Exec { clone, user, workdir, env, cmd }) => {
                assert_eq!(clone, "c");
                assert_eq!(user.as_deref(), Some("root"));
                assert_eq!(workdir.as_deref(), Some("/srv"));
                assert_eq!(env, vec!["A=1".to_string(), "B=2".to_string()]);
                assert_eq!(cmd, vec!["env".to_string()]);
            }
            other => panic!("wrong cmd: {other:?}"),
        }
        assert!(Cli::try_parse_from(["rmng", "clone", "exec", "c"]).is_err());
    }

    #[test]
    fn desktop_click_parses_verb_and_coords() {
        let cli = Cli::parse_from(["rmng", "desktop", "w-cp", "click", "10", "20"]);
        match cli.cmd {
            Cmd::Desktop { clone, cmd: DesktopCmd::Click { x, y, monitor, out, .. } } => {
                assert_eq!(clone, "w-cp");
                assert_eq!((x, y), (Some(10), Some(20)));
                assert_eq!(monitor, None);
                assert_eq!(out, None);
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    #[test]
    fn desktop_renamed_verbs_use_kebab_case() {
        // Old cryptic names no longer parse.
        for old in ["rclick", "mclick", "dclick", "movewin"] {
            assert!(
                Cli::try_parse_from(["rmng", "desktop", "w-cp", old]).is_err(),
                "old verb `{old}` should no longer parse"
            );
        }
        // New spelled-out names resolve to their variants.
        let cli = Cli::parse_from(["rmng", "desktop", "w-cp", "right-click", "5", "6"]);
        assert!(matches!(
            cli.cmd,
            Cmd::Desktop { cmd: DesktopCmd::RightClick { x: Some(5), y: Some(6), .. }, .. }
        ));
        assert!(matches!(
            Cli::parse_from(["rmng", "desktop", "w-cp", "middle-click"]).cmd,
            Cmd::Desktop { cmd: DesktopCmd::MiddleClick { .. }, .. }
        ));
        assert!(matches!(
            Cli::parse_from(["rmng", "desktop", "w-cp", "double-click"]).cmd,
            Cmd::Desktop { cmd: DesktopCmd::DoubleClick { .. }, .. }
        ));
        let cli = Cli::parse_from(["rmng", "desktop", "w-cp", "move-window", "win1", "--mode", "maximize"]);
        assert!(matches!(
            cli.cmd,
            Cmd::Desktop { cmd: DesktopCmd::MoveWindow { ref id, .. }, .. } if id == "win1"
        ));
    }

    #[test]
    fn desktop_click_accepts_both_rescale_flags() {
        let cli = Cli::parse_from([
            "rmng", "desktop", "w-cp", "click", "500", "500", "--rescale-cursor", "0-1000",
            "--rescale-screen", "1920x1080",
        ]);
        match cli.cmd {
            Cmd::Desktop { cmd: DesktopCmd::Click { rescale, .. }, .. } => {
                assert_eq!(rescale.rescale_cursor.as_deref(), Some("0-1000"));
                assert_eq!(rescale.rescale_screen.as_deref(), Some("1920x1080"));
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    #[test]
    fn desktop_screenshot_accepts_rescale_screen_alone() {
        let cli = Cli::parse_from([
            "rmng", "desktop", "w-cp", "screenshot", "--rescale-screen", "1280x720",
        ]);
        match cli.cmd {
            Cmd::Desktop { cmd: DesktopCmd::Screenshot { rescale, .. }, .. } => {
                assert_eq!(rescale.rescale_screen.as_deref(), Some("1280x720"));
                assert_eq!(rescale.rescale_cursor, None);
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    #[test]
    fn server_resolution_precedence() {
        assert_eq!(
            resolve_server(Some("http://flag:1/".into()), Some("http://env:2".into())),
            "http://flag:1"
        );
        assert_eq!(resolve_server(None, Some("http://env:2".into())), "http://env:2");
        assert_eq!(resolve_server(None, None), "http://localhost:9000");
        assert_eq!(resolve_server(Some("  ".into()), None), "http://localhost:9000");
    }
}
