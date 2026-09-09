//! Where a clone sits on the dashboard board, as a Rust mirror of the browser's
//! `frontend/app/lib/board.ts`.
//!
//! The server stores [`crate::BoardColumn`]s and applies no rules to them: `PUT /api/board`
//! replaces the list wholesale because the client owns the arrangement. That made the browser
//! the only thing that knew the rules, which was fine while it was the only client. The CLI is
//! the second one, so the rules live here, in the crate both of them already share.
//!
//! Three of those rules are load-bearing and none is obvious from the stored shape:
//!
//! - A clone no column claims is not stored anywhere. It is drawn in its home column, which is
//!   the first archive column when it is archived and the first column otherwise, so a clone
//!   created a second ago appears without anyone having written it down.
//! - A sub clone is never filed. It is drawn under its parent's card, so giving it a column of
//!   its own would draw it twice.
//! - An id for a clone that no longer exists is kept rather than pruned. It costs one string,
//!   and a clone recreated under the same name is the one the operator filed.

use crate::control::{BoardColumn, RmngClone};

/// What the board shows before anyone has made a column: somewhere to work, and somewhere to
/// retire clones to. The browser draws these without storing them, so a first write has to
/// send them too or the operator's board would silently lose its Archived column.
pub fn default_columns() -> Vec<BoardColumn> {
    vec![
        BoardColumn {
            id: "clones".into(),
            title: "Clones".into(),
            clone_ids: Vec::new(),
            archive: false,
        },
        BoardColumn {
            id: "archived".into(),
            title: "Archived".into(),
            clone_ids: Vec::new(),
            archive: true,
        },
    ]
}

/// The stored columns, or the defaults when nothing is stored yet.
pub fn with_defaults(columns: &[BoardColumn]) -> Vec<BoardColumn> {
    if columns.is_empty() {
        default_columns()
    } else {
        columns.to_vec()
    }
}

/// Whether this clone is drawn as a card of its own, rather than under a parent's.
///
/// A sub clone whose parent is gone is promoted to its own card instead of disappearing.
fn is_filed(clone: &RmngClone, live: &[RmngClone]) -> bool {
    match clone.parent.as_deref() {
        Some(parent) => !live.iter().any(|c| c.id == parent),
        None => true,
    }
}

/// The columns as drawn: live, non-sub clones only, with anything unfiled appended to its home
/// column. Shape is preserved, so a caller can render this directly.
pub fn resolve_columns(columns: &[BoardColumn], clones: &[RmngClone]) -> Vec<BoardColumn> {
    let filed_clones: Vec<&RmngClone> = clones.iter().filter(|c| is_filed(c, clones)).collect();
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<BoardColumn> = columns
        .iter()
        .map(|column| {
            let clone_ids = column
                .clone_ids
                .iter()
                .filter(|id| {
                    let live = filed_clones.iter().any(|c| &&c.id == id);
                    // First occurrence wins, which is what the board draws when a client bug
                    // files one clone in two columns.
                    let fresh = !seen.iter().any(|s| &s == id);
                    if live && fresh {
                        seen.push((*id).clone());
                    }
                    live && fresh
                })
                .cloned()
                .collect();
            BoardColumn {
                clone_ids,
                ..column.clone()
            }
        })
        .collect();
    if out.is_empty() {
        return out;
    }
    for clone in &filed_clones {
        if seen.contains(&clone.id) {
            continue;
        }
        let home = home_for(&out, clone);
        if let Some(column) = out.iter_mut().find(|c| c.id == home) {
            column.clone_ids.push(clone.id.clone());
        }
    }
    out
}

/// Where an unfiled clone is drawn: the first archive column when it is already archived, else
/// the first column. An archived clone drawn in an ordinary column would be unarchived by the
/// next drag out of it, which is not what "never filed" should mean.
fn home_for(columns: &[BoardColumn], clone: &RmngClone) -> String {
    if clone.archived {
        if let Some(archive) = columns.iter().find(|c| c.archive) {
            return archive.id.clone();
        }
    }
    columns.first().map(|c| c.id.clone()).unwrap_or_default()
}

/// The column holding `clone_id`, or `None` when nothing claims it. Pass resolved columns to
/// include the ones drawn in their home column.
pub fn column_of<'a>(columns: &'a [BoardColumn], clone_id: &str) -> Option<&'a BoardColumn> {
    columns
        .iter()
        .find(|c| c.clone_ids.iter().any(|id| id == clone_id))
}

/// Put `clone_id` at `to_index` of `to_column`, taking it out of wherever it was.
///
/// Out of every column first, including the destination, so a move within one column reorders
/// rather than duplicating.
pub fn move_card(
    columns: &[BoardColumn],
    clone_id: &str,
    to_column: &str,
    to_index: usize,
) -> Vec<BoardColumn> {
    columns
        .iter()
        .map(|column| {
            let mut clone_ids: Vec<String> = column
                .clone_ids
                .iter()
                .filter(|id| *id != clone_id)
                .cloned()
                .collect();
            if column.id == to_column {
                clone_ids.insert(to_index.min(clone_ids.len()), clone_id.to_string());
            }
            BoardColumn {
                clone_ids,
                ..column.clone()
            }
        })
        .collect()
}

/// Find the column an operator named, by title or by id, ignoring case and surrounding space.
///
/// Titles are what a person reads off the board, so they are tried first and are the reason
/// this exists: `In Progress` should work without anyone knowing it is stored as `in-progress`.
pub fn find_column<'a>(columns: &'a [BoardColumn], name: &str) -> Option<&'a BoardColumn> {
    let wanted = name.trim().to_lowercase();
    columns
        .iter()
        .find(|c| c.title.trim().to_lowercase() == wanted)
        .or_else(|| {
            columns
                .iter()
                .find(|c| c.id.trim().to_lowercase() == wanted)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clone(id: &str, archived: bool) -> RmngClone {
        RmngClone {
            id: id.into(),
            archived,
            managed: true,
            ..Default::default()
        }
    }

    fn sub(id: &str, parent: &str) -> RmngClone {
        RmngClone {
            parent: Some(parent.into()),
            ..clone(id, false)
        }
    }

    fn columns() -> Vec<BoardColumn> {
        vec![
            BoardColumn {
                id: "todo".into(),
                title: "Todo".into(),
                clone_ids: vec!["a".into(), "b".into()],
                archive: false,
            },
            BoardColumn {
                id: "doing".into(),
                title: "In Progress".into(),
                clone_ids: vec!["c".into()],
                archive: false,
            },
            BoardColumn {
                id: "done".into(),
                title: "Done".into(),
                clone_ids: Vec::new(),
                archive: true,
            },
        ]
    }

    #[test]
    fn a_clone_no_column_claims_lands_in_the_first_column() {
        let out = resolve_columns(
            &columns(),
            &[
                clone("a", false),
                clone("b", false),
                clone("c", false),
                clone("d", false),
            ],
        );
        assert_eq!(out[0].clone_ids, ["a", "b", "d"]);
        assert_eq!(out[1].clone_ids, ["c"]);
    }

    #[test]
    fn an_unfiled_archived_clone_lands_in_the_first_archive_column() {
        let out = resolve_columns(&columns(), &[clone("a", false), clone("z", true)]);
        assert_eq!(out[0].clone_ids, ["a"]);
        assert_eq!(out[2].clone_ids, ["z"]);
    }

    #[test]
    fn a_clone_that_is_gone_is_not_drawn_but_a_sub_clone_is_never_filed() {
        // "b" no longer exists, and "s" belongs under its parent's card rather than in a column.
        let out = resolve_columns(&columns(), &[clone("a", false), sub("s", "a")]);
        assert_eq!(out[0].clone_ids, ["a"]);
        assert!(out.iter().all(|c| !c.clone_ids.iter().any(|id| id == "s")));
    }

    #[test]
    fn a_sub_clone_whose_parent_is_gone_gets_a_card_of_its_own() {
        let out = resolve_columns(&columns(), &[sub("s", "vanished")]);
        assert_eq!(out[0].clone_ids, ["s"]);
    }

    #[test]
    fn move_card_puts_it_first_and_takes_it_out_of_where_it_was() {
        let out = move_card(&columns(), "a", "doing", 0);
        assert_eq!(out[0].clone_ids, ["b"]);
        assert_eq!(out[1].clone_ids, ["a", "c"]);
    }

    #[test]
    fn moving_within_one_column_reorders_rather_than_duplicating() {
        let out = move_card(&columns(), "b", "todo", 0);
        assert_eq!(out[0].clone_ids, ["b", "a"]);
    }

    #[test]
    fn an_index_past_the_end_lands_at_the_end() {
        let out = move_card(&columns(), "a", "doing", 99);
        assert_eq!(out[1].clone_ids, ["c", "a"]);
    }

    #[test]
    fn a_column_is_found_by_the_title_a_person_reads_off_the_board() {
        let cols = columns();
        assert_eq!(find_column(&cols, "In Progress").unwrap().id, "doing");
        assert_eq!(find_column(&cols, "  in progress ").unwrap().id, "doing");
        // The stored id works too, for scripts that already know it.
        assert_eq!(find_column(&cols, "doing").unwrap().id, "doing");
        assert!(find_column(&cols, "nowhere").is_none());
    }

    #[test]
    fn an_empty_board_still_names_the_two_columns_the_browser_draws() {
        let cols = with_defaults(&[]);
        assert_eq!(find_column(&cols, "Archived").unwrap().id, "archived");
        assert!(find_column(&cols, "Archived").unwrap().archive);
    }
}
