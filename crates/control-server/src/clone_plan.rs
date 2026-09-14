//! What a new clone will be, settled before anything is built.
//!
//! Both `POST /api/clone` and `POST /api/fork` hand their [`wire::CloneRequest`] here with a
//! state snapshot and get back a [`ClonePlan`] or the 400 message. Whatever the request
//! leaves open — preset, pool, both accounts, the name, a fork's source — is decided here
//! and nowhere else. No Docker, no ZFS, no network, so every rule is a unit test.

use std::collections::HashSet;

use wire::{AppConfig, CloneRequest, ControlState, LinearMeta, OperationStatus, Preset, RmngClone};

use crate::naming;
use crate::provision::is_dns_label;

/// What one provider's account does on the new clone: always a fresh pick inside the
/// plan's pool. `None` reads as `auto`. A fork never keeps its source's account: it draws
/// from the group like every other clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Side {
    /// Resolve this selection inside the plan's pool (`None` reads as `auto`).
    Assign(Option<String>),
}

/// A clone that does not exist yet, fully decided.
#[derive(Debug, Clone)]
pub(crate) struct ClonePlan {
    /// The new clone's id: the ticket or a slug of the title, plus the first free letter.
    pub id: String,
    /// The clone a fork copies. `None` builds from the preset's image onto a fresh home.
    pub source: Option<RmngClone>,
    /// The id recorded as the new clone's parent (`None` = top-level).
    pub parent: Option<String>,
    /// Drives the image, the env, the playbook and the startup script.
    pub preset_name: Option<String>,
    /// The ticket context the row stores and the agent is started on.
    pub linear: Option<LinearMeta>,
    /// The pool both sides draw from. `None` is every pool.
    pub group: Option<String>,
    pub claude: Side,
    pub codex: Side,
    pub first_message: Option<String>,
    pub agent_instructions: Option<String>,
    pub claude_instructions: Option<String>,
    pub headless: bool,
    pub run_startup_script: bool,
    pub rebuild: bool,
}

/// Plan the clone `req` asks for: a fork of a live clone when `fork`, else one built from a
/// preset image. `retired` is the names a deleted clone's transcript ledger still holds
/// ([`crate::ledger::reserved_names`]); they are never handed out again.
pub(crate) fn plan(
    cfg: &AppConfig,
    st: &ControlState,
    retired: &HashSet<String>,
    fork: bool,
    req: CloneRequest,
) -> Result<ClonePlan, String> {
    let named = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let asked_name = named(&req.preset);
    let asked_preset = match &asked_name {
        Some(name) => {
            Some(preset_named(cfg, name).ok_or_else(|| format!("unknown preset '{name}'"))?)
        }
        None => None,
    };
    let source = match (fork, named(&req.source)) {
        (true, asked) => Some(fork_source(st, asked, asked_preset)?),
        (false, None) => None,
        (false, Some(_)) => {
            return Err("a source is only for a fork (POST /api/fork)".into());
        }
    };
    // Cosmetic subclone link, one level deep. A template build is always top-level.
    let parent = match (fork, named(&req.parent)) {
        (false, Some(_)) => {
            return Err("a parent is only for a fork (POST /api/fork)".into());
        }
        (true, Some(id)) => {
            let p = st
                .hosts
                .iter()
                .find(|h| h.id == id)
                .ok_or_else(|| format!("unknown clone '{id}'"))?;
            if p.parent.is_some() {
                return Err(format!("'{id}' is already a subclone"));
            }
            Some(id)
        }
        (_, None) => None,
    };
    // A fork keeps its source's preset unless the request names another one.
    let preset_name = asked_name.or_else(|| source.as_ref().and_then(|s| s.preset_name.clone()));
    if source.is_none() && preset_name.is_none() && !cfg.presets.is_empty() {
        let names: Vec<&str> = cfg.presets.iter().map(|p| p.name.as_str()).collect();
        return Err(format!(
            "a preset is required (configured: {})",
            names.join(", ")
        ));
    }

    // The pool: an explicit `group` wins, then a legacy `group:<pool>` account selection,
    // then the pool of the preset the request named, else (on a fork) the source's own.
    let default_group = match asked_preset {
        Some(p) => preset_pool(p),
        None => source.as_ref().and_then(|s| s.group.clone()),
    };
    let explicit = req
        .group
        .as_deref()
        .map(|g| Some(g.trim().to_string()).filter(|g| !g.eq_ignore_ascii_case("none")));
    let (group, claude_sel, codex_sel) = crate::clone_ops::split_group_binding(
        named(&req.claude_account),
        named(&req.codex_account),
        default_group,
        explicit,
    );
    crate::clone_ops::validate_group(cfg, group.as_deref()).map_err(|e| e.to_string())?;
    let claude = Side::Assign(claude_sel);
    let codex = Side::Assign(codex_sel);

    // The name: the ticket identifier, else a slug of the title. A clone built from an image
    // has no ticket to fall back on, so there a title is required.
    //
    // A fork that carries neither takes its SOURCE's id as the base, so forking
    // `pega-dev-123` reads as `pega-dev-123a`. It used to fall through to the empty title,
    // whose base is a literal `<prefix>host`: every titleless fork in the fleet pooled onto
    // that one name whatever it was forked from, and because a retired name is never handed
    // out again, the pool ran dry for good after `<prefix>hostz`.
    let ticket = req.linear.as_ref().and_then(|l| named(&l.ticket));
    let title = req.linear.as_ref().and_then(|l| named(&l.display_name));
    if source.is_none() && ticket.is_none() && title.is_none() {
        return Err("a title is required (linear.displayName)".into());
    }
    let prefix = &cfg.docker.hostname_prefix;
    let base = match (&ticket, &title) {
        (Some(t), _) => naming::ticket_hostname_base(prefix, t),
        (None, Some(t)) => naming::plain_hostname_base(prefix, t),
        // Without a source the guard above already refused this, so the fallback only keeps
        // the match total.
        (None, None) => source
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_else(|| naming::plain_hostname_base(prefix, "")),
    };
    let taken: HashSet<&str> = st
        .hosts
        .iter()
        .map(|h| h.id.as_str())
        .chain(
            st.operations
                .iter()
                .filter(|o| o.status == OperationStatus::Running)
                .map(|o| o.target.as_str()),
        )
        .chain(retired.iter().map(String::as_str))
        .collect();
    let suffix = std::iter::once(String::new())
        .chain((b'a'..=b'z').map(|c| (c as char).to_string()))
        .find(|s| !taken.contains(format!("{base}{s}").as_str()))
        .ok_or_else(|| format!("every name from '{base}' to '{base}z' is taken"))?;
    let id = format!("{base}{suffix}");
    if !is_dns_label(&id) {
        return Err(format!(
            "'{id}' is not a DNS label (lowercase letters, digits, hyphens)"
        ));
    }
    // A second clone for the same ticket or title takes the next letter, and says so: the
    // letter rides the display name too, so the board shows which one it is.
    let linear = match req.linear {
        Some(mut l) => {
            if !suffix.is_empty() {
                l.display_name = l.display_name.map(|t| format!("{} ({suffix})", t.trim()));
            }
            Some(l)
        }
        // A fork bringing no ticket of its own keeps the source's, field for field.
        None => source.as_ref().map(source_linear),
    };

    Ok(ClonePlan {
        id,
        source,
        parent,
        preset_name,
        linear,
        group,
        claude,
        codex,
        first_message: named(&req.first_message),
        agent_instructions: named(&req.agent_instructions),
        claude_instructions: named(&req.claude_instructions),
        headless: req.headless,
        run_startup_script: req.run_startup_script,
        rebuild: req.rebuild,
    })
}

fn preset_named<'a>(cfg: &'a AppConfig, name: &str) -> Option<&'a Preset> {
    cfg.presets.iter().find(|p| p.name == name)
}

/// The pool a preset names, where it names one (blank and `none` are "every pool").
fn preset_pool(p: &Preset) -> Option<String> {
    Some(p.group.trim().to_string()).filter(|g| !g.is_empty() && !g.eq_ignore_ascii_case("none"))
}

/// The clone a fork copies: the one asked for, else the preset's default fork clone where it
/// is still forkable, else the oldest forkable clone (clones are prepended as they are made,
/// so the last row is the oldest survivor).
fn fork_source(
    st: &ControlState,
    asked: Option<String>,
    preset: Option<&Preset>,
) -> Result<RmngClone, String> {
    let forkable = |h: &&RmngClone| h.managed && !h.archived;
    let src = match &asked {
        Some(id) => st
            .hosts
            .iter()
            .find(|h| &h.id == id)
            .ok_or_else(|| format!("unknown clone '{id}'"))?,
        None => {
            let default = preset
                .map(|p| p.default_fork_clone.trim())
                .filter(|d| !d.is_empty());
            st.hosts
                .iter()
                .filter(forkable)
                .find(|h| Some(h.id.as_str()) == default)
                .or_else(|| st.hosts.iter().find(forkable))
                .ok_or_else(|| {
                    "no forkable clones: build one from a preset image first".to_string()
                })?
        }
    };
    if !src.managed {
        return Err(format!("'{}' is not a managed clone", src.id));
    }
    if src.base_tag.is_none() {
        return Err(format!("'{}' is not a gen-2 clone (no base tag)", src.id));
    }
    if st
        .operations
        .iter()
        .any(|o| o.status == OperationStatus::Running && o.target == src.id)
    {
        return Err(format!("'{}' already has an operation in flight", src.id));
    }
    Ok(src.clone())
}

/// The ticket context a fork inherits when it brings none of its own.
fn source_linear(src: &RmngClone) -> LinearMeta {
    LinearMeta {
        workspace: src.linear_workspace.clone(),
        ticket: src.linear_ticket.clone(),
        ticket_url: src.linear_ticket_url.clone(),
        branch: src.linear_branch.clone(),
        display_name: src.display_name.clone(),
        label: src.linear_label.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AppConfig {
        AppConfig {
            docker: wire::DockerConfig {
                hostname_prefix: "pega-".into(),
                ..Default::default()
            },
            groups: vec![wire::CloneGroup {
                name: "pooled".into(),
                accounts: vec!["a@x".into()],
            }],
            presets: vec![
                wire::Preset {
                    name: "work".into(),
                    group: "pooled".into(),
                    ..Default::default()
                },
                wire::Preset {
                    name: "bare".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// A live clone to fork: gen-2, in a pool, with an account on each side.
    fn source(id: &str) -> RmngClone {
        RmngClone {
            id: id.into(),
            host: id.into(),
            managed: true,
            base_tag: Some("rmng-p-0".into()),
            group: Some("pooled".into()),
            preset_name: Some("work".into()),
            claude_selection: Some("auto".into()),
            claude_account_email: Some("a@x".into()),
            codex_selection: Some("auto".into()),
            codex_account_email: Some("c@x".into()),
            ..Default::default()
        }
    }

    fn running(target: &str) -> wire::Operation {
        wire::Operation {
            id: "op".into(),
            kind: wire::OperationKind::Clone,
            target: target.into(),
            source: None,
            status: OperationStatus::Running,
            step: "start".into(),
            pct: 0.0,
            message: String::new(),
            log: Vec::new(),
            started_at: 0,
            finished_at: None,
        }
    }

    fn state(hosts: Vec<RmngClone>) -> ControlState {
        ControlState {
            hosts,
            ..Default::default()
        }
    }

    /// `{ "linear": { "displayName": title } }`, the shape both routes name a clone with.
    fn titled(title: &str) -> CloneRequest {
        CloneRequest {
            linear: Some(LinearMeta {
                display_name: Some(title.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn plan_of(st: &ControlState, fork: bool, req: CloneRequest) -> Result<ClonePlan, String> {
        plan(&cfg(), st, &HashSet::new(), fork, req)
    }

    // --- the pool and the two accounts ---------------------------------------------------

    /// The dialog sends the pool it shows plus `auto` on both sides. The pool used to be
    /// dropped on the way in, which left the clone drawing from every account there is.
    #[test]
    fn an_asked_for_pool_binds_even_beside_auto_accounts() {
        let got = plan_of(
            &state(vec![]),
            false,
            CloneRequest {
                preset: Some("work".into()),
                group: Some("pooled".into()),
                claude_account: Some("auto".into()),
                codex_account: Some("auto".into()),
                ..titled("encoder scratch")
            },
        )
        .unwrap();
        assert_eq!(got.group.as_deref(), Some("pooled"));
        assert_eq!(got.claude, Side::Assign(Some("auto".into())));
    }

    /// Same shape, pool that does not exist: the typo is caught rather than leaving the
    /// clone tokenless with nothing in any log.
    #[test]
    fn a_pool_that_does_not_exist_is_refused() {
        let err = plan_of(
            &state(vec![]),
            false,
            CloneRequest {
                preset: Some("work".into()),
                group: Some("typo".into()),
                claude_account: Some("auto".into()),
                ..titled("x")
            },
        )
        .unwrap_err();
        assert!(err.contains("unknown account pool"), "{err}");
    }

    /// "Any group (all pools)" on a fork. `none` is the whole point of the control, so it
    /// has to beat the pool the source sits in.
    #[test]
    fn none_unbinds_a_fork_from_its_sources_pool() {
        let got = plan_of(
            &state(vec![source("pega-we-1")]),
            true,
            CloneRequest {
                group: Some("none".into()),
                ..titled("spike")
            },
        )
        .unwrap();
        assert_eq!(got.group, None);
        // The pool moved, so neither side may keep the account it drew from the old one.
        assert_eq!(got.claude, Side::Assign(None));
        assert_eq!(got.codex, Side::Assign(None));
    }

    /// A fork naming a preset takes that preset's pool, exactly as a clone built from the
    /// same preset's image does.
    #[test]
    fn a_forks_preset_decides_its_pool() {
        let mut src = source("pega-we-1");
        src.group = None;
        let got = plan_of(
            &state(vec![src]),
            true,
            CloneRequest {
                preset: Some("work".into()),
                ..titled("spike")
            },
        )
        .unwrap();
        assert_eq!(got.group.as_deref(), Some("pooled"));
        // A preset with no pool of its own unbinds, rather than silently keeping the old one.
        let got = plan_of(
            &state(vec![source("pega-we-1")]),
            true,
            CloneRequest {
                preset: Some("bare".into()),
                ..titled("spike")
            },
        )
        .unwrap();
        assert_eq!(got.group, None);
    }

    /// A fork that asks for nothing draws both sides fresh from the pool, like every
    /// other clone. It never keeps the source's account.
    #[test]
    fn a_plain_fork_follows_the_group_on_both_sides() {
        let got = plan_of(&state(vec![source("pega-we-1")]), true, titled("spike")).unwrap();
        assert_eq!(got.group.as_deref(), Some("pooled"));
        assert_eq!(got.claude, Side::Assign(None));
        assert_eq!(got.codex, Side::Assign(None));
        assert_eq!(got.preset_name.as_deref(), Some("work"));
    }

    /// One side named, the other silent: the named side pins, the other follows the group.
    #[test]
    fn naming_one_side_leaves_the_other_on_the_group() {
        let got = plan_of(
            &state(vec![source("pega-we-1")]),
            true,
            CloneRequest {
                claude_account: Some("me@x".into()),
                ..titled("spike")
            },
        )
        .unwrap();
        assert_eq!(got.claude, Side::Assign(Some("me@x".into())));
        assert_eq!(got.codex, Side::Assign(None));
    }

    // --- the subclone parent -------------------------------------------------------------

    /// `--parent` is recorded on the row; omitted means top-level.
    #[test]
    fn a_fork_parent_is_recorded_and_defaults_to_none() {
        let mut req = titled("spike");
        req.parent = Some("pega-we-1".into());
        let got = plan_of(&state(vec![source("pega-we-1")]), true, req).unwrap();
        assert_eq!(got.parent.as_deref(), Some("pega-we-1"));

        let got = plan_of(&state(vec![source("pega-we-1")]), true, titled("spike")).unwrap();
        assert_eq!(got.parent, None);
    }

    /// A parent that names nothing is refused rather than recorded dangling.
    #[test]
    fn an_unknown_fork_parent_is_refused() {
        let mut req = titled("spike");
        req.parent = Some("pega-nope".into());
        let err = plan_of(&state(vec![source("pega-we-1")]), true, req).unwrap_err();
        assert!(err.contains("unknown clone"), "{err}");
    }

    /// One level deep: a subclone is never itself a parent.
    #[test]
    fn a_subclone_is_never_itself_a_parent() {
        let mut child = source("pega-we-1a");
        child.parent = Some("pega-we-1".into());
        let mut req = titled("spike");
        req.parent = Some("pega-we-1a".into());
        let err = plan_of(&state(vec![source("pega-we-1"), child]), true, req).unwrap_err();
        assert!(err.contains("already a subclone"), "{err}");
    }

    /// A template build is always top-level.
    #[test]
    fn a_parent_on_the_template_route_is_refused() {
        let mut req = titled("spike");
        req.parent = Some("pega-we-1".into());
        let err = plan_of(&state(vec![source("pega-we-1")]), false, req).unwrap_err();
        assert!(err.contains("only for a fork"), "{err}");
    }

    // --- which clone a fork copies --------------------------------------------------------
    #[test]
    fn a_fork_without_a_source_takes_the_presets_default_then_the_oldest() {
        let mut preset_default = cfg();
        preset_default.presets[0].default_fork_clone = "pega-we-2".into();
        let st = state(vec![source("pega-we-1"), source("pega-we-2")]);
        let req = CloneRequest {
            preset: Some("work".into()),
            ..titled("spike")
        };
        let got = plan(&preset_default, &st, &HashSet::new(), true, req.clone()).unwrap();
        assert_eq!(got.source.unwrap().id, "pega-we-2");
        // A default naming no forkable clone falls back to the oldest.
        let got = plan_of(&st, true, req).unwrap();
        assert_eq!(got.source.unwrap().id, "pega-we-1");
    }

    #[test]
    fn a_fork_needs_something_to_copy() {
        let err = plan_of(&state(vec![]), true, titled("spike")).unwrap_err();
        assert!(err.contains("no forkable clones"), "{err}");
        // An archived clone is stopped, so it is not one either.
        let archived = RmngClone {
            archived: true,
            ..source("pega-we-1")
        };
        let err = plan_of(&state(vec![archived]), true, titled("spike")).unwrap_err();
        assert!(err.contains("no forkable clones"), "{err}");
    }

    #[test]
    fn a_source_that_cannot_be_forked_is_named_in_the_error() {
        let unknown = plan_of(
            &state(vec![]),
            true,
            CloneRequest {
                source: Some("ghost".into()),
                ..titled("spike")
            },
        )
        .unwrap_err();
        assert!(unknown.contains("unknown clone 'ghost'"), "{unknown}");

        let gen1 = RmngClone {
            base_tag: None,
            ..source("pega-we-1")
        };
        let err = plan_of(
            &state(vec![gen1]),
            true,
            CloneRequest {
                source: Some("pega-we-1".into()),
                ..titled("spike")
            },
        )
        .unwrap_err();
        assert!(err.contains("not a gen-2 clone"), "{err}");
    }

    #[test]
    fn a_source_already_in_an_operation_is_refused() {
        let mut st = state(vec![source("pega-we-1")]);
        st.operations.push(running("pega-we-1"));
        let err = plan_of(&st, true, titled("spike")).unwrap_err();
        assert!(err.contains("already has an operation"), "{err}");
    }

    // --- the name -------------------------------------------------------------------------

    #[test]
    fn the_ticket_names_the_clone_and_the_title_names_the_rest() {
        let ticketed = plan_of(
            &state(vec![]),
            false,
            CloneRequest {
                preset: Some("work".into()),
                linear: Some(LinearMeta {
                    ticket: Some("WE-142".into()),
                    display_name: Some("Encoder drops frames".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ticketed.id, "pega-we-142");
        let titled = plan_of(
            &state(vec![]),
            false,
            CloneRequest {
                preset: Some("work".into()),
                ..titled("My cool task!")
            },
        )
        .unwrap();
        assert_eq!(titled.id, "pega-my-cool-task");
    }

    /// The next clone for a name in use takes the next letter, and carries it in the title
    /// so the two are told apart on the board.
    #[test]
    fn a_name_in_use_takes_the_next_letter() {
        let mut st = state(vec![RmngClone {
            id: "pega-we-142".into(),
            ..Default::default()
        }]);
        let req = || CloneRequest {
            preset: Some("work".into()),
            linear: Some(LinearMeta {
                ticket: Some("WE-142".into()),
                display_name: Some("Encoder drops frames".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let got = plan_of(&st, false, req()).unwrap();
        assert_eq!(got.id, "pega-we-142a");
        assert_eq!(
            got.linear.unwrap().display_name.as_deref(),
            Some("Encoder drops frames (a)")
        );
        // A clone being created holds its name too, before it exists.
        st.operations.push(running("pega-we-142a"));
        assert_eq!(plan_of(&st, false, req()).unwrap().id, "pega-we-142b");
        // And so does a deleted clone whose transcript ledger is still filed under it.
        let retired = HashSet::from(["pega-we-142b".to_string()]);
        let got = plan(&cfg(), &st, &retired, false, req()).unwrap();
        assert_eq!(got.id, "pega-we-142c");
    }

    /// A fork that names nothing is named after what it copies, so the letter says which
    /// copy of that clone it is. It used to be named `pega-host`, which said neither.
    #[test]
    fn a_fork_that_names_nothing_is_named_after_its_source() {
        let st = state(vec![RmngClone {
            linear_ticket: Some("DEV-123".into()),
            ..source("pega-dev-123")
        }]);
        let got = plan_of(&st, true, CloneRequest::default()).unwrap();
        assert_eq!(got.id, "pega-dev-123a");
        // Its ticket still comes from the source, field for field.
        assert_eq!(got.linear.unwrap().ticket.as_deref(), Some("DEV-123"));
    }

    /// Every titleless fork used to share one `pega-host` name whatever it copied, so 27 of
    /// them anywhere in the fleet exhausted the letters for good. Naming each after its own
    /// source keeps the families apart.
    #[test]
    fn titleless_forks_of_different_sources_do_not_share_a_name() {
        let st = state(vec![source("pega-dev-123"), source("pega-dev-456")]);
        let a = plan_of(
            &st,
            true,
            CloneRequest {
                source: Some("pega-dev-123".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let b = plan_of(
            &st,
            true,
            CloneRequest {
                source: Some("pega-dev-456".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (a.id.as_str(), b.id.as_str()),
            ("pega-dev-123a", "pega-dev-456a")
        );
    }

    /// A titled fork is named from the title, and bringing a `linear` of its own is what
    /// keeps the source's ticket off it — so nothing kicks an agent off on that ticket.
    #[test]
    fn a_titled_fork_is_named_from_the_title_and_inherits_no_ticket() {
        let st = state(vec![source("pega-dev-123")]);
        let got = plan_of(&st, true, titled("ng 0c3e2998")).unwrap();
        assert_eq!(got.id, "pega-ng-0c3e2998");
        let linear = got.linear.unwrap();
        assert_eq!(linear.ticket, None);
        assert_eq!(linear.display_name.as_deref(), Some("ng 0c3e2998"));
    }

    // --- what a request must say ----------------------------------------------------------

    #[test]
    fn a_clone_built_from_an_image_needs_a_title_and_a_preset() {
        let no_title = plan_of(
            &state(vec![]),
            false,
            CloneRequest {
                preset: Some("work".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(no_title.contains("title is required"), "{no_title}");

        let no_preset = plan_of(&state(vec![]), false, titled("x")).unwrap_err();
        assert!(no_preset.contains("preset is required"), "{no_preset}");

        // A fork needs neither: both come from the clone it copies.
        assert!(
            plan_of(
                &state(vec![source("pega-we-1")]),
                true,
                CloneRequest::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn a_preset_that_does_not_exist_is_refused() {
        let err = plan_of(
            &state(vec![]),
            false,
            CloneRequest {
                preset: Some("ghost".into()),
                ..titled("x")
            },
        )
        .unwrap_err();
        assert!(err.contains("unknown preset 'ghost'"), "{err}");
    }

    #[test]
    fn a_source_on_the_template_route_is_refused() {
        let err = plan_of(
            &state(vec![source("pega-we-1")]),
            false,
            CloneRequest {
                source: Some("pega-we-1".into()),
                preset: Some("work".into()),
                ..titled("x")
            },
        )
        .unwrap_err();
        assert!(err.contains("only for a fork"), "{err}");
    }
}
