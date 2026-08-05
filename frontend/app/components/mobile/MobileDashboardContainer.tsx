// The phone app's network half: it owns which clone is open, talks to the server, and
// hands the two pure pages (MobileHome, MobileClone) everything they draw.
//
// It is the counterpart of `DashboardContainer`, not a wrapper around it. The board, its
// drag handlers, and every modal except account import stay on the desktop side and are
// never imported here, so a phone downloads none of them.
//
// Navigation is one id, and it lives in the page's address rather than in state: no `?clone=`
// is the clone list, an id is that clone's screen. So the phone's own Back gesture is what
// returns to the list, and to the clone before that. Selecting a clone activates it
// server-side the same way a board card does, which is what clears its unread flag and stops
// the monitor from notifying about output the operator is looking at.
//
// Opening the import dialog is navigation too, even though the phone has one route: the home
// screen reports the tap through `onImportAccount` and the dialog is mounted here, beside the
// screen rather than inside it. That is what lets a story render the home screen without a
// modal on top of it, and the dialog's own stories cover every state it has.
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";

import { ImportAccountModalContainer } from "~/components/ImportAccountModalContainer";
import { TicketPanel } from "~/components/TicketPanel";
import { MobileClone, type CloneTab } from "~/components/mobile/MobileClone";
import { MobileHome } from "~/components/mobile/MobileHome";
import { useAccountOrder } from "~/lib/accountOrder";
import { activate, refreshClaudeUsage, refreshCodexUsage } from "~/lib/api";
import { withDefaults } from "~/lib/board";
import { copyText } from "~/lib/clipboard";
import { browserLocale } from "~/lib/format";
import {
  issueSetLabel,
  issueSetStateId,
  issueUpdate,
  keysForTeam,
} from "~/lib/linear/mutations";
import type { TicketLabel, TicketWorkflowState } from "~/lib/linear/types";
import { useTeamMeta } from "~/lib/linear/useTeamMeta";
import { useTickets } from "~/lib/linear/useTickets";
import { readSelection, sameSelection, withSelection } from "~/lib/selection";
import { cloneForTicket, cloneTickets, findTicket, type LinearTicket } from "~/lib/tickets";
import type { ControlState } from "~/lib/types";
import { useCloneNotifications } from "~/lib/useCloneNotifications";
import { useNow } from "~/lib/useNow";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

// BlockNote and the chat panel are browser-only, and both are the desktop's modules
// unchanged: the phone changes their surroundings, not what they do.
const NotesEditorContainer = lazy(() => import("~/components/NotesEditorContainer"));
const ChatContainer = lazy(() => import("~/components/ChatContainer"));
// The ticket description is markdown, and rendering it means BlockNote, which is as
// browser-only as the notes editor beside it.
const TicketDescription = lazy(() => import("~/components/TicketDescription"));

function ClientOnly({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  return mounted ? <>{children}</> : null;
}

function PaneFallback({ label }: { label: string }) {
  return <p className="p-4 text-sm text-slate-400 dark:text-slate-500">{label}</p>;
}

export function MobileDashboardContainer({
  state,
  cloneGroups,
  codexGroups,
  presets,
}: {
  state: ControlState;
  /** Configured Claude pools (`config.cloneGroups`) — the usage list groups by these. */
  cloneGroups: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups: CloneGroup[];
  /** Configured presets (`config.presets`). Their Linear keys are what read a clone's own
   *  ticket, which is the one thing on this screen the control server does not send. */
  presets: PresetRedacted[];
}) {
  // Which clone's screen is up, read off the address. The desktop writes the same parameter,
  // so the two shells agree on what a link to this page means.
  const [params, setParams] = useSearchParams();
  const openId = readSelection(params).clone;
  const [tab, setTab] = useState<CloneTab>("chat");
  const [usageOpen, setUsageOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The two session reads the usage bars need. Both belong here rather than in the panel:
  // the store is shared with desktop Settings, and the clock has to tick somewhere the
  // pure page cannot see.
  const { acctOrder } = useAccountOrder();
  const now = useNow();

  const run = (p: Promise<unknown>) =>
    p.then(() => setError(null)).catch((e: Error) => setError(e.message));

  /** Show a clone's screen, or the clone list when `id` is null. Leaves a history entry, so
   *  the phone's Back gesture goes back a screen. */
  const show = (id: string | null) => {
    const next = { clone: id, ticket: null };
    if (sameSelection(readSelection(params), next)) return;
    setParams(withSelection(params, next), { preventScrollReset: true });
  };

  // Tell the server which clone is on view, once per address. Back and Forward have to reach
  // it the way the tap that first opened the screen did, and null is a real value here: the
  // clone list is nothing on view, so the viewer stops following.
  //
  // Always lands on the chat, because the notes are where you were last time and the chat is
  // where the news is.
  const applied = useRef<string | null>(null);
  useEffect(() => {
    if (applied.current === openId) return;
    applied.current = openId;
    setTab("chat");
    run(activate(openId));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [openId]);

  // A clone that stops working while its screen is closed still notifies, and tapping the
  // notification opens it.
  useCloneNotifications(state.hosts, state.mutedClones ?? [], show);

  // Every clone's own Linear ticket, read by this browser with the presets' keys. The phone
  // has no ticket column, so the open queue is nobody's business here and only the clones'
  // own identifiers are asked for.
  const {
    tickets: linearTickets,
    refetch: refetchTickets,
    upsert: upsertTicket,
  } = useTickets(
    presets,
    state.hosts.flatMap((h) => (h.linearTicket ? [h.linearTicket] : [])),
    { openIssues: false },
  );

  // Write a title or a body back to Linear and show it now, with Linear's own answer a round
  // trip later. A refused write puts the old value back. Same shape as the desktop's, which
  // is the shape every write in this app takes.
  const editTicket = (ticket: LinearTicket, patch: { title?: string; description?: string }) => {
    upsertTicket({ ...ticket, ...patch });
    run(
      issueUpdate(keysForTeam(presets, ticket.team ?? ""), ticket, patch)
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };

  // Move a ticket into a state picked by name off its team's workflow, and put one label on or
  // take it off. Both are the desktop's own writes, unchanged, because a ticket is the same
  // ticket on a phone: show it now, ask Linear, put the old value back if Linear refuses.
  const setTicketState = (ticket: LinearTicket, state: TicketWorkflowState) => {
    upsertTicket({ ...ticket, state: state.type, stateId: state.id, stateName: state.name });
    run(
      issueSetStateId(keysForTeam(presets, ticket.team ?? ""), ticket, state.id)
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };

  const setTicketLabel = (ticket: LinearTicket, label: TicketLabel, on: boolean) => {
    const labels = on
      ? [...ticket.labels, label]
      : ticket.labels.filter((l) => l.id !== label.id);
    upsertTicket({ ...ticket, labels });
    run(
      issueSetLabel(keysForTeam(presets, ticket.team ?? ""), ticket, label.id, on)
        .catch((e: Error) => {
          upsertTicket(ticket);
          throw e;
        })
        .then(refetchTickets),
    );
  };

  // Each clone's ticket as Linear has it now, which is where every row and every header on
  // this screen takes its title from.
  const liveCloneTickets = cloneTickets(state.hosts, linearTickets);

  // Derived, not stored: a clone deleted from the desktop while its screen is open simply
  // stops resolving, and the list comes back on the next frame with no cleanup to run.
  const openClone = openId ? (state.hosts.find((c) => c.id === openId) ?? null) : null;
  const openTicket = openClone?.linearTicket
    ? findTicket(openClone.linearTicket, linearTickets)
    : null;
  // What the open ticket's two menus offer. Above the early return below, because a hook that
  // only runs on some renders is not a hook.
  const ticketMeta = useTeamMeta(presets, openTicket);

  if (openClone) {
    return (
      <MobileClone
        clone={openClone}
        ticket={liveCloneTickets[openClone.id]}
        tab={tab}
        onTabChange={setTab}
        onBack={() => show(null)}
        error={error}
        // A clone made from a ticket shows the ticket where its notes would be, and the tab
        // says so. The notes file stays on disk; nothing on this screen opens it.
        notesLabel={openClone.linearTicket ? "Ticket" : "Notes"}
        notes={
          !openClone.linearTicket ? (
            <ClientOnly>
              <Suspense fallback={<PaneFallback label="Loading notes…" />}>
                <NotesEditorContainer key={openClone.id} cloneId={openClone.id} />
              </Suspense>
            </ClientOnly>
          ) : !openTicket ? (
            <PaneFallback label={`Loading ${openClone.linearTicket}…`} />
          ) : (
            <TicketPanel
              ticket={openTicket}
              onCopyBranchName={copyText}
              description={
                <ClientOnly>
                  <Suspense fallback={<PaneFallback label="Loading description…" />}>
                    <TicketDescription
                      key={openTicket.id}
                      markdown={openTicket.description ?? ""}
                      onSave={(markdown) => editTicket(openTicket, { description: markdown })}
                    />
                  </Suspense>
                </ClientOnly>
              }
              onTitleChange={(title) => editTicket(openTicket, { title })}
              onStateChange={(next) => setTicketState(openTicket, next)}
              stateOptions={ticketMeta.states}
              statesLoading={ticketMeta.loading}
              labelOptions={ticketMeta.labels}
              labelsLoading={ticketMeta.loading}
              onAddLabel={(label) => setTicketLabel(openTicket, label, true)}
              onRemoveLabel={(label) => setTicketLabel(openTicket, label, false)}
              // A sub-issue somebody already cloned is one tap away, so the row opens that
              // clone instead of leaving for Linear. Everything else leaves.
              resolveLink={(id) => {
                const clone = cloneForTicket(id, state.hosts);
                if (!clone) return null;
                return {
                  title: `Show the clone for ${id}: ${clone.id}`,
                  open: () => show(clone.id),
                };
              }}
            />
          )
        }
        chat={
          <ClientOnly>
            <Suspense fallback={<PaneFallback label="Loading chat…" />}>
              <ChatContainer
                key={openClone.id}
                cloneId={openClone.id}
                archived={openClone.archived === true}
              />
            </Suspense>
          </ClientOnly>
        }
      />
    );
  }

  return (
    <>
      <MobileHome
        accounts={state.claudeAccounts ?? []}
        accountOrder={acctOrder}
        cloneGroups={cloneGroups}
        codexGroups={codexGroups}
        // Read here rather than in the usage bars, so the reset-time tooltips are a function
        // of the page's props like everything else it draws.
        locale={browserLocale()}
        now={now}
        usageOpen={usageOpen}
        onUsageOpenChange={setUsageOpen}
        onRefresh={() => run(Promise.all([refreshClaudeUsage(), refreshCodexUsage()]))}
        onImportAccount={() => setImportOpen(true)}
        columns={withDefaults(state.boardColumns ?? [])}
        clones={state.hosts}
        cloneTickets={liveCloneTickets}
        onSelectClone={(clone) => show(clone.id)}
        error={error}
      />
      {importOpen ? (
        <ImportAccountModalContainer
          claudeGroups={cloneGroups.map((g) => g.name)}
          codexGroups={codexGroups.map((g) => g.name)}
          onClose={() => setImportOpen(false)}
          onImported={() => {
            setImportOpen(false);
            // The account is in the server's store immediately; its usage windows fill in
            // on the next poll, which these kick off rather than waiting the interval out.
            setError(null);
            run(refreshClaudeUsage());
            run(refreshCodexUsage());
          }}
        />
      ) : null}
    </>
  );
}
