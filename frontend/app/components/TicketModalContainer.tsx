// Open a Linear issue from the board, network half.
//
// Four things live here and nowhere below: the `issueCreate` mutation, the lookup of who can
// be assigned in the chosen team, the team the operator last opened a ticket in, and the
// markdown editor the body is typed into, which carries its own image upload. Everything else,
// including the title, the priority, and whether the button may fire, is TicketModalView.
//
// The team is held here rather than in the View because three impure things hang off it: the
// people lookup, the key that stores a pasted image, and what gets remembered for next time.
//
// The created ticket goes back up rather than being handled here: the column writes it into
// its own list and opens its panel, which is the page's call and not the dialog's.
import { lazy, Suspense, useEffect, useMemo, useState } from "react";

import { TicketModalView } from "~/components/TicketModalView";
import { teamKeysOf } from "~/lib/cloneDraft";
import { toLinearMarkdown } from "~/lib/linear/assets";
import { issueCreate, keysForTeam } from "~/lib/linear/mutations";
import { defaultAssignee, fetchTeamPeople, type TicketPerson } from "~/lib/linear/people";
import { lastTicketTeam, preferredTicketTeam, rememberTicketTeam } from "~/lib/lastTicketTeam";
import type { LinearTicket } from "~/lib/tickets";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

// BlockNote is browser-only and heavy; the description field pulls it in on demand. The
// container is the import target, so the upload call it owns rides the same lazy chunk.
const MarkdownEditorContainer = lazy(() => import("~/components/MarkdownEditorContainer"));

export function TicketModalContainer({
  presets,
  onClose,
  onCreated,
}: {
  /** Configured presets (`config.presets`). Their labels are the dialog's team keys, and a
   *  non-empty `linearKey` says which of them can actually open a ticket. */
  presets: PresetRedacted[];
  onClose: () => void;
  /** The issue Linear answered with, once it exists. The dialog closes either way. */
  onCreated: (ticket: LinearTicket) => void;
}) {
  const teams = useMemo(() => teamKeysOf(presets), [presets]);
  // The team the last ticket was opened in, when a preset still claims it. Read once on mount:
  // storage is a session fact, and re-reading it would fight the dropdown.
  const [team, setTeam] = useState(() => preferredTicketTeam(teams, lastTicketTeam()));
  // The body lives here because the editor does: the slot reports markdown up on every
  // keystroke, and the View sends whatever it last said.
  const [description, setDescription] = useState("");
  const [people, setPeople] = useState<TicketPerson[]>([]);
  const [peopleLoading, setPeopleLoading] = useState(false);
  const [assigneeId, setAssigneeId] = useState("");

  // Whichever key claims the chosen team. It opens the issue and it stores the images pasted
  // into the body, so both follow the dropdown rather than being fixed when the dialog opened.
  const key = keysForTeam(presets, team)[0] ?? "";

  // Who can hold a ticket in this team, and which of them is you. Refetched per team, because
  // membership is a property of the team and not of the workspace.
  //
  // A lookup that fails leaves the list empty rather than blocking the dialog: the create then
  // falls back to the key's own owner, which is the assignee the operator wanted anyway. Whose
  // answer this is matters when the operator changes team mid-flight, so a late reply for a
  // team no longer chosen is dropped.
  useEffect(() => {
    if (key === "" || team === "") {
      setPeople([]);
      return;
    }
    let live = true;
    setPeopleLoading(true);
    fetchTeamPeople(key, team)
      .then((found) => {
        if (!live) return;
        setPeople(found);
        setAssigneeId(defaultAssignee(found));
      })
      .catch(() => {
        if (!live) return;
        setPeople([]);
        setAssigneeId("");
      })
      .finally(() => {
        if (live) setPeopleLoading(false);
      });
    return () => {
      live = false;
    };
  }, [key, team]);

  return (
    <TicketModalView
      teams={teams}
      team={team}
      onTeamChange={setTeam}
      people={people}
      assigneeId={assigneeId}
      onAssigneeChange={setAssigneeId}
      peopleLoading={peopleLoading}
      description={description}
      descriptionEditor={
        <Suspense
          fallback={
            <p className="px-3 text-xs text-slate-400 dark:text-slate-500">Loading editor…</p>
          }
        >
          {/* Deliberately not keyed on the team: BlockNote captures its upload function once
              at mount, so remounting to move the image target would take the typed body with
              it. Images therefore go to whichever workspace the dialog opened on, and only a
              fleet whose presets point at different Linear workspaces can notice. */}
          <MarkdownEditorContainer
            onChange={setDescription}
            linearKey={key}
            placeholder="What needs doing — paste images, format freely"
          />
        </Suspense>
      }
      onClose={onClose}
      // The editor holds every pasted image behind `/api/linear/asset`, because that is the
      // only source an `<img>` on this page can load. Linear gets the `uploads.linear.app`
      // URL back, so the issue reads correctly for everyone who is not on this LAN. That
      // swap happens here and nowhere else: this is the last place the body is ours.
      onCreate={(ticket) =>
        issueCreate(keysForTeam(presets, ticket.team)[0] ?? "", {
          ...ticket,
          description: toLinearMarkdown(ticket.description),
        }).then((created) => {
          // Only a create that landed. A team someone scrolled past should not become the
          // default for the next ticket.
          rememberTicketTeam(team);
          onCreated(created);
        })
      }
    />
  );
}
