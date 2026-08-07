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
    /// Manage clones (the fleet unit): ls / create-* / rm / archive / restore / ssh / exec / …
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
    /// Search the distilled transcripts of every clone, retired clones included
    #[command(subcommand)]
    Ledger(LedgerCmd),
    /// Drive a clone's desktop via its daemon MCP (screenshot-on-every-action)
    Desktop {
        /// Clone id
        clone: String,
        #[command(subcommand)]
        cmd: DesktopCmd,
    },
}

/// Flags shared by every clone-creating verb (`create`, `create-from-ticket`,
/// `create-with-new-ticket`, `create-plain`), matching the controls the web dialog shows
/// below its three tabs.
#[derive(Args, Debug)]
pub struct CreateArgs {
    /// Clone-source image reference to create from (see `rmng image ls`)
    #[arg(long)]
    pub from: String,
    /// Claude account for the new clone: an email, `auto`, `none`, or `group:<pool>`.
    /// Omitted inherits the parent's selection (inside a clone), else `auto`.
    #[arg(long)]
    pub claude_account: Option<String>,
    /// Codex account, same forms. Independent of --claude-account.
    #[arg(long)]
    pub codex_account: Option<String>,
    /// Headless clone: no desktop; the viewer shows a tmux tab view instead of a stream
    #[arg(long)]
    pub headless: bool,
    /// Create as a sub clone under this parent clone id (must be top-level). Overrides the
    /// default caller auto-detection. Conflicts with --top-level.
    #[arg(long, conflicts_with = "top_level")]
    pub parent: Option<String>,
    /// Force a top-level clone even when run from inside a clone (skip auto-nesting)
    #[arg(long)]
    pub top_level: bool,
    #[command(flatten)]
    pub wait: WaitArgs,
}

/// Read `--description` / `--description-file` into one markdown string. The file form
/// exists because a markdown body with newlines and images is painful to pass as an argv
/// word; `-` reads stdin, so a heredoc or a pipe works.
pub fn read_text(inline: Option<&String>, file: Option<&PathBuf>) -> std::io::Result<String> {
    match (inline, file) {
        (Some(s), _) => Ok(s.clone()),
        (None, Some(p)) if p.as_os_str() == "-" => std::io::read_to_string(std::io::stdin()),
        (None, Some(p)) => std::fs::read_to_string(p),
        (None, None) => Ok(String::new()),
    }
}

/// `rmng clone <verb>` — everything that acts on the fleet unit.
#[derive(Subcommand, Debug)]
pub enum CloneCmd {
    /// List clones with live CPU, RAM, activity, and each provider's bound account
    Ls,
    /// Create a clone under an exact hostname (no ticket, no derived name)
    Create {
        /// Exact hostname for the new clone (DNS label)
        hostname: String,
        /// Env preset name. Omitted inside a clone ⇒ inherit the parent's preset; use
        /// --no-preset for none.
        #[arg(long)]
        preset: Option<String>,
        /// Use no env preset (opt out of inheriting the parent's)
        #[arg(long, conflicts_with = "preset")]
        no_preset: bool,
        #[command(flatten)]
        common: CreateArgs,
    },
    /// Create a clone for an EXISTING Linear ticket (the web dialog's "Existing ticket" tab).
    /// The hostname derives from the ticket id and the preset is auto-selected from its team
    /// prefix — there is no --preset here, exactly as in the dialog.
    CreateFromTicket {
        /// Linear ticket link or bare id (e.g. `WE-142`)
        ticket: String,
        /// Extra clone-agent instructions, appended to the default (takes precedence)
        #[arg(long)]
        agent_instructions: Option<String>,
        /// Extra Claude Code instructions, appended to the default (takes precedence)
        #[arg(long)]
        claude_instructions: Option<String>,
        #[command(flatten)]
        common: CreateArgs,
    },
    /// Create a Linear ticket AND a clone for it (the dialog's "New ticket" tab) — the only
    /// verb that opens a new ticket. The team key selects the preset, whose Linear API key
    /// creates the issue.
    CreateWithNewTicket {
        /// Linear team key the ticket is created in, e.g. `we` (must be a label on some preset)
        #[arg(long)]
        team: String,
        /// Ticket title
        #[arg(long)]
        title: String,
        /// Ticket description as **markdown**
        #[arg(long, conflicts_with = "description_file")]
        description: Option<String>,
        /// Read the markdown description from a file (`-` for stdin)
        #[arg(long, value_name = "PATH")]
        description_file: Option<PathBuf>,
        /// Extra clone-agent instructions, appended to the default (takes precedence)
        #[arg(long)]
        agent_instructions: Option<String>,
        /// Extra Claude Code instructions, appended to the default (takes precedence)
        #[arg(long)]
        claude_instructions: Option<String>,
        #[command(flatten)]
        common: CreateArgs,
    },
    /// Create a no-ticket clone with a title-derived hostname (the dialog's "No ticket" tab).
    /// Use `clone create` instead when you want to name the host yourself.
    CreatePlain {
        /// Container title — the display name, and the stem of the derived hostname
        #[arg(long)]
        title: String,
        /// First message auto-sent to the agent (omitted ⇒ nothing is sent)
        #[arg(long)]
        message: Option<String>,
        /// Read the first message from a file (`-` for stdin)
        #[arg(long, value_name = "PATH", conflicts_with = "message")]
        message_file: Option<PathBuf>,
        /// Env preset name (required when any presets are configured)
        #[arg(long)]
        preset: Option<String>,
        #[command(flatten)]
        common: CreateArgs,
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
        /// Launch detached (fire-and-forget): return immediately with no captured output —
        /// for GUI apps on the clone desktop (`rmng clone exec -d c -- gnome-text-editor`)
        #[arg(short = 'd', long)]
        detach: bool,
        /// The command argv, after `--` (e.g. `rmng clone exec c -- ls -la`)
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
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
}

#[derive(Subcommand, Debug)]
pub enum AccountCmd {
    /// List imported accounts with usage windows (all providers by default)
    Ls {
        /// Only show accounts for this provider
        #[arg(long)]
        provider: Option<Provider>,
    },
    /// Hot-swap a clone's account for one provider (no restart — the credential file is
    /// rewritten and the agent re-reads it on its next request)
    Swap {
        /// Clone id
        clone: String,
        /// An email, `auto`, `none`, or `group:<pool>`
        account: String,
        /// Swap the Codex account instead of the Claude one
        #[arg(long)]
        codex: bool,
    },
    /// Delete an imported account by email, moving any clones running it
    Rm {
        /// Account email
        account: String,
        /// Remove a Codex account instead of a Claude one
        #[arg(long)]
        codex: bool,
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

/// The transcript ledger: what every clone's agent actually did, kept after the clone is gone.
///
/// The server holds the corpus and runs the search, so both verbs return matches rather than
/// transcripts. `search` finds the lines; `read` gives the conversation around one of them.
#[derive(Subcommand, Debug)]
pub enum LedgerCmd {
    /// Find ledger lines matching a pattern
    Search {
        /// Case-insensitive substring, matched against the whole ledger line, so it reaches
        /// the text, the tool name and the kind alike
        pattern: String,
        /// Only this clone (default: every clone the ledger knows, retired ones included)
        #[arg(long)]
        clone: Option<String>,
        /// Oldest record to return: a duration ago (`90m`, `6h`, `2d`, `3w`) or epoch millis
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,
        /// Newest record to return, same forms as `--since`
        #[arg(long, value_name = "WHEN")]
        until: Option<String>,
        /// Only subagent turns, which is the delegated work rather than the conversation
        #[arg(long, conflicts_with = "no_sidechain")]
        sidechain: bool,
        /// Only the conversation, hiding every subagent turn
        #[arg(long)]
        no_sidechain: bool,
        /// Only this subagent, by the id a hit reports as `agentId`
        #[arg(long, value_name = "ID")]
        agent: Option<String>,
        /// Most hits to return (server caps this at 500)
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Print a byte range of one session's ledger, as NDJSON
    Read {
        /// Clone id, as `search` reports it
        clone: String,
        /// Session id, as `search` reports it
        session: String,
        /// Where to start. Pass a hit's offset, or less to read what led up to it
        #[arg(long, default_value_t = 0)]
        offset: u64,
        /// How many bytes to read (server caps this at 1 MiB)
        #[arg(long, default_value_t = 64 * 1024)]
        len: u64,
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

/// Optional `--resolution <W>x<H>` / `--native` for the desktop verbs that deal in
/// coordinates or images.
///
/// Both the screenshot the daemon returns **and** the space its `x`/`y` are read in
/// come from this one value, so they can never disagree — the daemon does the scaling
/// (see `clone-daemon/src/mcp.rs`), and the CLI just forwards the choice. Omitting both
/// flags gets the daemon's default (1080p-height, i.e. 1920×1080 on a 16:9 monitor).
#[derive(Args, Debug, Clone, Default)]
pub struct ResolutionArgs {
    /// Coordinate + screenshot space for this call, `<W>x<H>` (e.g. `1280x720`).
    /// Omit for the daemon default (1080p-height — 1920x1080 on a 16:9 monitor).
    #[arg(long, value_name = "WxH", conflicts_with = "native")]
    pub resolution: Option<String>,
    /// Use the monitor's native resolution instead of the 1080p default.
    #[arg(long)]
    pub native: bool,
}

impl ResolutionArgs {
    /// The value to forward to the daemon as its `resolution` argument, or `None` to let
    /// the daemon apply its default. Validates `<W>x<H>` here so a typo fails the command
    /// outright rather than being silently ignored on the far side.
    pub fn resolution_arg(&self) -> Result<Option<String>, String> {
        if self.native {
            return Ok(Some("native".into()));
        }
        let Some(s) = self.resolution.as_deref() else {
            return Ok(None);
        };
        let (w, h) = s
            .split_once(['x', 'X'])
            .ok_or_else(|| format!("--resolution: expected WxH, got '{s}'"))?;
        let w: i32 = w.trim().parse().map_err(|e| format!("--resolution: bad W: {e}"))?;
        let h: i32 = h.trim().parse().map_err(|e| format!("--resolution: bad H: {e}"))?;
        if w <= 0 || h <= 0 {
            return Err("--resolution: W and H must be > 0".into());
        }
        Ok(Some(format!("{w}x{h}")))
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
        resolution: ResolutionArgs,
    },
    /// List monitors (→ `list_monitors`)
    Monitors,
    /// List windows (→ `list_windows`)
    Windows,
    /// Move the mouse to X Y (→ `mouse_move`)
    Move {
        x: i32,
        y: i32,
        #[arg(long)]
        monitor: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        resolution: ResolutionArgs,
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
        resolution: ResolutionArgs,
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
        resolution: ResolutionArgs,
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
        resolution: ResolutionArgs,
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
        resolution: ResolutionArgs,
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
        resolution: ResolutionArgs,
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
            "rmng", "clone", "create", "w-cp", "--from", "tmpl:latest", "--claude-account", "pooled",
            "--wait", "--timeout", "120",
        ]);
        match cli.cmd {
            Cmd::Clone(CloneCmd::Create { hostname, preset, no_preset, common }) => {
                assert_eq!(hostname, "w-cp");
                assert_eq!(common.from, "tmpl:latest");
                assert_eq!(common.claude_account.as_deref(), Some("pooled"));
                assert!(!no_preset && !common.headless && !common.top_level);
                assert_eq!(preset, None);
                assert_eq!(common.parent, None);
                assert!(common.wait.wait);
                assert_eq!(common.wait.timeout, 120);
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    /// The three `create-*` verbs mirror the web dialog's three tabs. Every clone-creating
    /// verb is named `create…` so the action is unmistakable — `clone ticket WE-142` read
    /// like it did something TO the ticket. The ticket verbs deliberately expose NO
    /// `--preset`: the server auto-selects it (from the ticket prefix, or from the team key),
    /// exactly as the dialog does.
    #[test]
    fn clone_create_verbs_mirror_the_dialog_tabs() {
        let cli = Cli::parse_from([
            "rmng", "clone", "create-from-ticket", "WE-142", "--from", "t:1", "--wait",
            "--agent-instructions", "be brief",
        ]);
        match cli.cmd {
            Cmd::Clone(CloneCmd::CreateFromTicket { ticket, agent_instructions, common, .. }) => {
                assert_eq!(ticket, "WE-142");
                assert_eq!(agent_instructions.as_deref(), Some("be brief"));
                assert!(common.wait.wait);
            }
            other => panic!("wrong cmd: {other:?}"),
        }
        assert!(
            Cli::try_parse_from([
                "rmng", "clone", "create-from-ticket", "WE-1", "--from", "t:1", "--preset", "p",
            ])
            .is_err(),
            "the ticket verb must not accept --preset (the server auto-selects it)"
        );

        let cli = Cli::parse_from([
            "rmng", "clone", "create-with-new-ticket", "--from", "t:1", "--team", "we", "--title", "Fix it",
            "--description", "# heading",
        ]);
        match cli.cmd {
            Cmd::Clone(CloneCmd::CreateWithNewTicket { team, title, description, common, .. }) => {
                assert_eq!((team.as_str(), title.as_str()), ("we", "Fix it"));
                assert_eq!(description.as_deref(), Some("# heading"));
                assert_eq!(common.from, "t:1");
            }
            other => panic!("wrong cmd: {other:?}"),
        }
        // --team and --title are required; --description ⊕ --description-file.
        assert!(Cli::try_parse_from(["rmng", "clone", "create-with-new-ticket", "--from", "t:1"]).is_err());
        assert!(
            Cli::try_parse_from([
                "rmng", "clone", "create-with-new-ticket", "--from", "t:1", "--team", "we", "--title", "x",
                "--description", "a", "--description-file", "b",
            ])
            .is_err()
        );

        let cli = Cli::parse_from([
            "rmng", "clone", "create-plain", "--from", "t:1", "--title", "scratch", "--preset", "p1",
        ]);
        match cli.cmd {
            Cmd::Clone(CloneCmd::CreatePlain { title, preset, message, .. }) => {
                assert_eq!(title, "scratch");
                assert_eq!(preset.as_deref(), Some("p1"));
                assert_eq!(message, None);
            }
            other => panic!("wrong cmd: {other:?}"),
        }

        // The pre-rename spellings are gone, not aliased — `clone ticket` / `clone new-ticket`
        // / `clone plain` didn't say they created anything. These verbs were only ever in
        // unreleased commits, so there's nothing to keep working.
        for old in [
            vec!["rmng", "clone", "ticket", "WE-1", "--from", "t:1"],
            vec!["rmng", "clone", "new-ticket", "--from", "t:1", "--team", "we", "--title", "t"],
            vec!["rmng", "clone", "plain", "--from", "t:1", "--title", "t"],
        ] {
            assert!(
                Cli::try_parse_from(&old).is_err(),
                "old verb `{}` should no longer parse",
                old[2]
            );
        }
    }

    #[test]
    fn read_text_prefers_inline_over_file() {
        let inline = "inline body".to_string();
        let missing = PathBuf::from("/nonexistent/rmng-test");
        assert_eq!(read_text(Some(&inline), Some(&missing)).unwrap(), "inline body");
        assert_eq!(read_text(None, None).unwrap(), "");
        // A real file is read verbatim, newlines and all — the whole point of the flag.
        let path = std::env::temp_dir().join("rmng-read-text-test.md");
        std::fs::write(&path, "# title\n\nbody\n").unwrap();
        assert_eq!(read_text(None, Some(&path)).unwrap(), "# title\n\nbody\n");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn clone_create_mutually_exclusive_flags() {
        // --parent ⊕ --top-level, --preset ⊕ --no-preset. Opting out of an account needs no
        // flag of its own: `--claude-account none` / `--codex-account none` says it, per
        // provider, in the same vocabulary every other account value uses.
        assert!(Cli::try_parse_from([
            "rmng", "clone", "create", "w-x", "--from", "i", "--parent", "p", "--top-level",
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
    fn ledger_search_needs_a_pattern_and_takes_its_filters() {
        assert!(Cli::try_parse_from(["rmng", "ledger", "search"]).is_err());
        let cli = Cli::parse_from([
            "rmng", "ledger", "search", "va-api", "--clone", "pega-we-142", "--since", "2d",
            "--limit", "5",
        ]);
        assert!(matches!(
            cli.cmd,
            Cmd::Ledger(LedgerCmd::Search {
                ref pattern, ref clone, ref since, until: None, limit: 5, ..
            }) if pattern == "va-api"
                && clone.as_deref() == Some("pega-we-142")
                && since.as_deref() == Some("2d")
        ));
        // Bare search: every clone, both halves of every session, 50 hits.
        let bare = Cli::parse_from(["rmng", "ledger", "search", "encoder"]);
        assert!(matches!(
            bare.cmd,
            Cmd::Ledger(LedgerCmd::Search {
                clone: None, since: None, limit: 50, sidechain: false, no_sidechain: false, ..
            })
        ));

        // One subagent's run, and the conversation without any of them.
        let one = Cli::parse_from(["rmng", "ledger", "search", "x", "--agent", "a7", "--sidechain"]);
        assert!(matches!(
            one.cmd,
            Cmd::Ledger(LedgerCmd::Search { ref agent, sidechain: true, .. })
                if agent.as_deref() == Some("a7")
        ));
        let main_only = Cli::parse_from(["rmng", "ledger", "search", "x", "--no-sidechain"]);
        assert!(matches!(
            main_only.cmd,
            Cmd::Ledger(LedgerCmd::Search { no_sidechain: true, sidechain: false, .. })
        ));
        // The two are opposites, so asking for both is an error rather than a silent winner.
        assert!(
            Cli::try_parse_from(["rmng", "ledger", "search", "x", "--sidechain", "--no-sidechain"])
                .is_err()
        );
    }

    #[test]
    fn ledger_read_takes_a_clone_a_session_and_a_range() {
        assert!(Cli::try_parse_from(["rmng", "ledger", "read", "pega-we-142"]).is_err());
        let cli = Cli::parse_from([
            "rmng", "ledger", "read", "pega-we-142", "sess-1", "--offset", "4096",
        ]);
        assert!(matches!(
            cli.cmd,
            Cmd::Ledger(LedgerCmd::Read { ref clone, ref session, offset: 4096, len: 65536 })
                if clone == "pega-we-142" && session == "sess-1"
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
            "-d", "--", "env",
        ]);
        match cli.cmd {
            Cmd::Clone(CloneCmd::Exec { clone, user, workdir, env, detach, cmd }) => {
                assert_eq!(clone, "c");
                assert_eq!(user.as_deref(), Some("root"));
                assert_eq!(workdir.as_deref(), Some("/srv"));
                assert_eq!(env, vec!["A=1".to_string(), "B=2".to_string()]);
                assert!(detach, "-d should set detach");
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
    fn desktop_click_accepts_resolution() {
        let cli =
            Cli::parse_from(["rmng", "desktop", "w-cp", "click", "500", "500", "--resolution", "1280x720"]);
        match cli.cmd {
            Cmd::Desktop { cmd: DesktopCmd::Click { x, y, resolution, .. }, .. } => {
                // Coordinates are forwarded verbatim — the daemon owns the scaling now.
                assert_eq!((x, y), (Some(500), Some(500)));
                assert_eq!(resolution.resolution_arg(), Ok(Some("1280x720".into())));
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    #[test]
    fn desktop_screenshot_accepts_native() {
        let cli = Cli::parse_from(["rmng", "desktop", "w-cp", "screenshot", "--native"]);
        match cli.cmd {
            Cmd::Desktop { cmd: DesktopCmd::Screenshot { resolution, .. }, .. } => {
                assert_eq!(resolution.resolution_arg(), Ok(Some("native".into())));
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    /// The two flags name the same knob, so clap must reject them together rather than
    /// silently letting one win.
    #[test]
    fn desktop_rejects_resolution_and_native_together() {
        assert!(
            Cli::try_parse_from([
                "rmng", "desktop", "w-cp", "screenshot", "--resolution", "1280x720", "--native",
            ])
            .is_err()
        );
    }

    /// Neither flag ⇒ nothing sent ⇒ the daemon applies its 1080p default.
    #[test]
    fn resolution_arg_is_none_when_unset() {
        assert_eq!(ResolutionArgs::default().resolution_arg(), Ok(None));
    }

    #[test]
    fn resolution_arg_accepts_either_x_case() {
        for s in ["1280x720", "1280X720", " 1280 x 720 "] {
            let r = ResolutionArgs { resolution: Some(s.into()), native: false };
            assert_eq!(r.resolution_arg(), Ok(Some("1280x720".into())), "input {s:?}");
        }
    }

    /// A typo must fail the command here rather than reach the daemon, which would fall back
    /// to native and silently put the caller's clicks in the wrong space.
    #[test]
    fn resolution_arg_rejects_malformed_values() {
        for bad in ["1920", "1920x", "x1080", "0x1080", "1920x0", "-1x-1", "axb", ""] {
            let r = ResolutionArgs { resolution: Some(bad.into()), native: false };
            assert!(r.resolution_arg().is_err(), "should reject {bad:?}");
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
