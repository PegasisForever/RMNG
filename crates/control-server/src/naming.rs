//! Hostname derivation for new clones, plus the preset lookup that goes with it.
//!
//! A clone's hostname is built here and nowhere else, because uniqueness needs the live clone
//! list ([`crate::jobs::next_free_hostname`]) and no client can see it. Two bases exist: one
//! slugged from a plain title, one from a ticket identifier. Both run through
//! [`clean_prefix`], which is what keeps `config.docker.hostnamePrefix` a legal DNS label.
//!
//! [`pick_preset_by_prefix`] lives here for the same reason the ticket base does: a preset's
//! labels are ticket-id prefixes (a preset labelled `DEV` claims `DEV-196`), so picking one is
//! string matching over the same identifier the hostname is built from.

/// The first preset (config order) with a label matching the ticket-id `prefix`
/// (case-insensitive), e.g. a preset labelled `DEV` matches `DEV-196` (prefix `dev`).
/// Presets with no labels never auto-match.
pub fn pick_preset_by_prefix<'a>(
    presets: &'a [wire::Preset],
    prefix: &str,
) -> Option<&'a wire::Preset> {
    presets.iter().find(|p| p.labels.iter().any(|pl| pl.eq_ignore_ascii_case(prefix)))
}

/// Sanitize the configurable hostname prefix to DNS-label-safe chars: lowercase,
/// keep `[a-z0-9-]`, drop a leading `-` (a trailing one like `pega-` is intended).
pub fn clean_prefix(prefix: &str) -> String {
    let s: String = prefix
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    s.trim_start_matches('-').to_string()
}

/// `(pega-, My cool task!)` → `pega-my-cool-task` (a DNS label; start_clone re-validates).
pub fn plain_hostname_base(prefix: &str, title: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in title.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-').chars().take(40).collect::<String>();
    let slug = slug.trim_matches('-').to_string();
    let prefix = clean_prefix(prefix);
    if slug.is_empty() { format!("{prefix}host") } else { format!("{prefix}{slug}") }
}

/// `(pega-, DEV-123)` → `pega-dev-123`.
pub fn ticket_hostname_base(prefix: &str, identifier: &str) -> String {
    format!("{}{}", clean_prefix(prefix), identifier.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_preset_by_ticket_prefix() {
        let p = |name: &str, labels: &[&str]| wire::Preset {
            name: name.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let presets = [p("front", &["WE", "UI"]), p("back", &["DEV"]), p("nolabel", &[])];
        // Case-insensitive match against the (lowercase) ticket-id prefix.
        assert_eq!(pick_preset_by_prefix(&presets, "dev").unwrap().name, "back");
        // Multiple labels on a preset → any of them can match.
        assert_eq!(pick_preset_by_prefix(&presets, "we").unwrap().name, "front");
        // No matching prefix / labelless presets never auto-match.
        assert!(pick_preset_by_prefix(&presets, "docs").is_none());
        assert!(pick_preset_by_prefix(&presets, "").is_none());
    }

    #[test]
    fn plain_slug() {
        assert_eq!(plain_hostname_base("pega-", "My cool task!"), "pega-my-cool-task");
        assert_eq!(plain_hostname_base("pega-", "!!!"), "pega-host");
        // custom + sanitized prefixes
        assert_eq!(plain_hostname_base("clone-", "My task"), "clone-my-task");
        assert_eq!(plain_hostname_base("", "My task"), "my-task");
        assert_eq!(plain_hostname_base("-Bad_Pre-", "X"), "badpre-x"); // leading '-' dropped, '_' stripped, lowercased
    }

    #[test]
    fn ticket_base() {
        assert_eq!(ticket_hostname_base("pega-", "DEV-123"), "pega-dev-123");
        assert_eq!(ticket_hostname_base("", "WE-7"), "we-7");
    }
}
