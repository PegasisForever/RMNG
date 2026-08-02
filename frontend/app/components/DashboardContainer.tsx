// The desktop dashboard's network half: it owns every mutation the board can start, the
// dialogs those mutations open, and the browser reads the shell must not make.
//
// It is the counterpart of `MobileDashboardContainer`, not a wrapper around it. The two share
// the live state and the config gate the route holds above both, and nothing else: the board,
// its drag handlers, and every dialog except account import stay on this side.
//
// The shell it renders is pure, so three things it cannot do are resolved here and handed
// down: the operator's locale, the page's own address (the SSH command's jump target when no
// public host is configured), and the remembered side-panel width.
//
// Dialogs are mounted beside the shell rather than inside it. Each one positions itself, so
// the shell would only be passing them through, and a page story then shows the page with
// nothing on top of it while each dialog's own stories cover every state it has.
import { lazy, Suspense, useEffect, useState } from "react";

import { AppShellV2, type SideFocus } from "~/components/AppShellV2";
import { ChangeAccountModalContainer } from "~/components/ChangeAccountModalContainer";
import { CloneModalContainer } from "~/components/CloneModalContainer";
import { CommitImageModal } from "~/components/CommitImageModal";
import { ImportAccountModalContainer } from "~/components/ImportAccountModalContainer";
import { PortForwardModal } from "~/components/PortForwardModal";
import { SettingsPanelContainer } from "~/components/SettingsPanelContainer";
import { TicketModalContainer } from "~/components/TicketModalContainer";
import { TicketPanel } from "~/components/TicketPanel";
import { issueUpdate, keysForTeam } from "~/lib/linear/mutations";
import { useTickets } from "~/lib/linear/useTickets";
import {
  cloneForTicket,
  findTicket,
  openTickets,
  orderTickets,
  type LinearTicket,
} from "~/lib/tickets";
import {
  activate,
  activateLayout,
  archiveClone,
  duplicateClone,
  commitImage,
  deleteClaudeAccount,
  deleteCodexAccount,
  deleteClone,
  deleteImage,
  getConfig,
  getUpdateStatus,
  listImages,
  pullTemplate,
  putBoardColumns,
  putConfig,
  putForwards,
  putTicketOrder,
  refreshClaudeUsage,
  refreshCodexUsage,
  restartServer,
  swapClaudeAccount,
  swapCodexAccount,
  testConfig,
  unarchiveClone,
  updateServer,
} from "~/lib/api";
import {
  columnIdOf,
  moveCard,
  newColumnId,
  removeColumn,
  resolveColumns,
  withDefaults,
  type BoardColumn,
} from "~/lib/board";
import { useAccountOrder } from "~/lib/accountOrder";
import { copyText } from "~/lib/clipboard";
import { browserLocale } from "~/lib/format";
import { rememberSideWidth, SIDE_DEFAULT, storedSideWidth } from "~/lib/sidePanelWidth";
import { type ControlState, type Clone } from "~/lib/types";
import { useCloneNotifications } from "~/lib/useCloneNotifications";
import { useNow } from "~/lib/useNow";
import type { ContainerStats } from "~/lib/wire/ContainerStats";
import type { ForwardRuntime } from "~/lib/wire/ForwardRuntime";
import type { CloneGroup } from "~/lib/wire/CloneGroup";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import type { LxcStats } from "~/lib/wire/LxcStats";
import type { ImageInfo } from "~/lib/wire/ImageInfo";

// BlockNote + the chat panel are browser-only; load them lazily and render only
// after mount so they never participate in SSR.
const NotesEditorContainer = lazy(() => import("~/components/NotesEditorContainer"));
const ChatContainer = lazy(() => import("~/components/ChatContainer"));
// The ticket description is markdown, and rendering it means BlockNote, which is as
// browser-only as the notes editor that already uses it.
const TicketDescription = lazy(() => import("~/components/TicketDescription"));

function ClientOnly({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  return mounted ? <>{children}</> : null;
}

export function DashboardContainer({
  state,
  stats,
  lxcStats,
  forwards,
  sshPublicHost,
  bastionPort,
  cloneGroups,
  codexGroups,
  presets,
}: {
  state: ControlState;
  stats: Record<string, ContainerStats>;
  lxcStats: LxcStats | null;
  forwards: Record<string, ForwardRuntime[]>;
  /** `ssh.publicHost` (config) — threaded down to each card's copied SSH command;
   *  empty ⇒ falls back to `window.location.hostname`. */
  sshPublicHost: string;
  /** `listen.bastion` — the bastion `sshd` port the copied SSH commands jump through. */
  bastionPort: number;
  /** Configured Claude pools (`config.cloneGroups`) — the rail's usage list groups by these. */
  cloneGroups: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups: CloneGroup[];
  /** Configured presets (`config.presets`). Their labels are the ticket dialog's team keys. */
  presets: PresetRedacted[];
}) {
  // The ticket whose panel has the side column, by id. Held as an id rather than the object
  // so a poll that rewrites the list keeps the panel on the current copy, not a stale one.
  const [openTicketId, setOpenTicketId] = useState<string | null>(null);

  // OS notification whenever a clone transitions out of `working` (idle/offline) while
  // it isn't the selected one — driven by the server's `unread` edge. Clicking it selects
  // that clone, which closes an open ticket the same way a card click does.
  useCloneNotifications(state.hosts, (id) => {
    setOpenTicketId(null);
    run(activate(id));
  });

  const [error, setError] = useState<string | null>(null);
  const [cloneOpen, setCloneOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // The shared cosmetic account order and the live clock. Both are session reads, so they
  // are resolved here and handed down: the rail's usage bars and the settings account lists
  // then draw from props alone, and both stay in step because they read one subscription.
  const { acctOrder } = useAccountOrder();
  const now = useNow();
  const [commitClone, setCommitClone] = useState<Clone | null>(null);
  const [committing, setCommitting] = useState(false);
  const [changeClone, setChangeClone] = useState<Clone | null>(null);
  const [changing, setChanging] = useState(false);
  // The group an "add account" OAuth login is in flight for (null = modal closed).
  const [importOpen, setImportOpen] = useState(false);
  const [forwardClone, setForwardClone] = useState<Clone | null>(null);
  const [forwarding, setForwarding] = useState(false);
  const [forwardError, setForwardError] = useState<string | null>(null);

  // Clone-source images (from /api/images) — fetched on mount and refetched
  // whenever a pull/commit/delete op leaves `running` (the image set changed).
  const [images, setImages] = useState<ImageInfo[]>([]);
  const [imagesLoading, setImagesLoading] = useState(true);
  const refreshImages = () => {
    setImagesLoading(true);
    listImages()
      .then(setImages)
      .catch(() => {
        /* keep the last-known list on a transient error */
      })
      .finally(() => setImagesLoading(false));
  };
  useEffect(() => {
    refreshImages();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Which half of the side panel gets the height.
  const [sideFocus, setSideFocus] = useState<SideFocus>("notes");
  // The width the operator last left the side panel at. Read after mount rather than in the
  // initial state, so the server and the first client render agree on the default.
  const [sideWidth, setSideWidth] = useState(SIDE_DEFAULT);
  useEffect(() => {
    const stored = storedSideWidth();
    if (stored !== null) setSideWidth(stored);
  }, []);

  // Board columns, held locally so a drag lands instantly, and re-adopted whenever the
  // server's own list changes. Keyed on the serialized list rather than the array identity:
  // every SSE frame brings a fresh array, and resetting on each one would undo a drag that
  // is still in flight.
  const serverColumns = JSON.stringify(state.boardColumns ?? []);
  const [columns, setColumns] = useState<BoardColumn[]>(() =>
    withDefaults(state.boardColumns ?? []),
  );
  useEffect(() => {
    setColumns(withDefaults(JSON.parse(serverColumns) as BoardColumn[]));
  }, [serverColumns]);

  const clonesById = new Map(state.hosts.map((h) => [h.id, h]));
  const selectedClone = state.selected ? clonesById.get(state.selected) ?? null : null;

  // Refetch images when an image-mutating op (pull/commit/delete) leaves the
  // running set — that's when the image list changed. Keyed on the set of running
  // op ids so it fires on each transition, not on every SSE frame.
  const imgOpsRunning = state.operations
    .filter(
      (o) =>
        o.status === "running" &&
        (o.kind === "pull" || o.kind === "commit" || o.kind === "delete"),
    )
    .map((o) => o.id)
    .join(",");
  useEffect(() => {
    refreshImages();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [imgOpsRunning]);

  const run = (p: Promise<unknown>) =>
    p.then(() => setError(null)).catch((e: Error) => setError(e.message));

  // The two things a card asks the browser for rather than the server. Both leave the cards
  // themselves pure: a menu item says what it wants done and this decides how.
  //
  // `noopener` is what stops the Linear tab from reaching back through `window.opener`, and
  // `noreferrer` keeps this page's address off the request. Both menus that leave for Linear
  // go through this one call, so neither can drift from the other.
  const openInLinear = (url: string) => {
    window.open(url, "_blank", "noopener,noreferrer");
  };

  // Adopt a new column layout locally, then persist it. The board previews a drag on its
  // own, so by the time this runs the operator has already seen the result; the PUT is
  // what makes it survive a refresh.
  const applyColumns = (next: BoardColumn[]) => {
    setColumns(next);
    run(putBoardColumns(next));
  };

  // A clone being provisioned into a specific column. The board can only file a clone that
  // exists, and the clone does not exist until its op finishes, so the request is parked
  // here and applied when the clone shows up in `hosts`.
  const [pendingColumn, setPendingColumn] = useState<{ columnId: string; target: string } | null>(
    null,
  );
  const [newCloneColumn, setNewCloneColumn] = useState<string | null>(null);
  /** The clone the open create-dialog is making, selected when the dialog closes. */
  const [newClone, setNewClone] = useState<string | null>(null);
  // The new-ticket dialog. Nothing else in the app opens it, so it needs no argument.
  const [newTicketOpen, setNewTicketOpen] = useState(false);
  // Seeds the clone dialog's ticket field when a ticket opened it (dragged onto a column, or
  // its menu). Empty for the column's own "New clone" button.
  const [ticketPrefill, setTicketPrefill] = useState("");
  // The operator's own arrangement of the ticket column, top to bottom. Stored server-side,
  // and held locally on top of that so a drag lands instantly. Same shape as the columns
  // above, for the same reason: keyed on the serialized list rather than the array identity,
  // because every SSE frame brings a fresh array and resetting on each one would undo a drag
  // that is still in flight.
  const serverTicketOrder = JSON.stringify(state.ticketOrder ?? []);
  const [ticketOrder, setTicketOrder] = useState<string[]>(() => state.ticketOrder ?? []);
  useEffect(() => {
    setTicketOrder(JSON.parse(serverTicketOrder) as string[]);
  }, [serverTicketOrder]);

  // Adopt a new order locally, then persist it. The column has already previewed the drag by
  // the time this runs; the PUT is what makes it survive a refresh. The server lowercases
  // what it stores, so the frame that comes back overwrites this with the same order in a
  // different case, which `orderTickets` cannot tell apart.
  const applyTicketOrder = (next: string[]) => {
    setTicketOrder(next);
    run(putTicketOrder(next));
  };

  // Linear's own answer, asked for by this browser with the presets' keys. It is the one
  // piece of the board that does not come from the control server.
  const {
    tickets: linearTickets,
    error: ticketsError,
    loading: ticketsLoading,
    refetch: refetchTickets,
    upsert: upsertTicket,
  } = useTickets(presets);

  // Write a title or a body back to Linear, and put the new value on screen twice: once
  // straight away so the panel does not snap back to what it said before the edit, and again
  // from Linear's own answer a round trip later. Without the refetch the poll interval owns
  // the panel, and reopening it inside that window shows the old text.
  const editTicket = (ticket: LinearTicket, patch: { title?: string; description?: string }) => {
    upsertTicket({ ...ticket, ...patch });
    run(issueUpdate(keysForTeam(presets, ticket.team ?? ""), ticket, patch).then(refetchTickets));
  };

  const visibleTickets = orderTickets(
    openTickets(linearTickets, state.hosts),
    ticketOrder,
  );
  // Derived: a ticket that leaves the list (cloned, closed, moved in Linear) closes its
  // panel on its own, with no cleanup to run.
  const openTicket = visibleTickets.find((t) => t.id === openTicketId) ?? null;
  useEffect(() => {
    if (!pendingColumn) return;
    const { columnId, target } = pendingColumn;
    if (!state.hosts.some((h) => h.id === target)) return;
    setPendingColumn(null);
    if (columnIdOf(columns, target) === columnId) return;
    applyColumns(moveCard(columns, target, columnId, 0));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.hosts, pendingColumn]);

  // Imported accounts, split per provider. `claudeAccounts` carries BOTH providers' rows
  // tagged by `provider`; a row written before that field existed has none, which means
  // Claude — so the Claude side matches on "not codex" rather than on "is claude".
  const accounts = state.claudeAccounts ?? [];
  const claudeAccounts = accounts.filter((a) => a.provider !== "codex");
  const codexAccounts = accounts.filter((a) => a.provider === "codex");

  const onDeleteAccount = (email: string) =>
    run(deleteClaudeAccount(email).then(() => refreshClaudeUsage()));
  const onDeleteCodexAccount = (email: string) =>
    run(deleteCodexAccount(email).then(() => refreshCodexUsage()));

  // Archiving rides a drag into the Archived column, and that column's contents come from
  // the server's `archived` flag rather than from the column list. So when the call fails
  // the card has to go back where it was: `before` is the layout from before the drop, the
  // board having fired this callback ahead of the move.
  const archiveDrop = (clone: Clone, archived: boolean) => {
    const before = columns;
    const call = archived ? archiveClone(clone.id) : unarchiveClone(clone.id);
    call
      .then(() => setError(null))
      .catch((e: Error) => {
        setError(e.message);
        applyColumns(before);
      });
  };

  // The selected clone, when it can actually parent a sub clone: managed, and top-level
  // (sub clones are one level deep — the server rejects a sub clone as a parent).
  const subCloneParent =
    selectedClone?.managed && !selectedClone.parent ? selectedClone : null;

  // Clones per column, for the settings editor's counts (unfiled ones ride the first).
  const columnCounts = Object.fromEntries(
    resolveColumns(columns, state.hosts).map((c) => [c.id, c.cloneIds.length]),
  );

  return (
    <>
      <AppShellV2
        selectedClone={selectedClone}
        ticket={
          openTicket ? (
            <TicketPanel
              ticket={openTicket}
              onCopyBranchName={copyText}
              description={
                <ClientOnly>
                  <Suspense fallback={<p className="px-1 text-xs text-slate-400">Loading…</p>}>
                    <TicketDescription
                      key={openTicket.id}
                      markdown={openTicket.description ?? ""}
                      onSave={(markdown) => editTicket(openTicket, { description: markdown })}
                    />
                  </Suspense>
                </ClientOnly>
              }
              onTitleChange={(title) => editTicket(openTicket, { title })}
              onCreateClone={() => {
                setTicketPrefill(openTicket.url);
                setCloneOpen(true);
              }}
              // A parent or sub-issue the board already holds is one click away, so the row
              // goes there instead of to Linear. The ticket column first, then the clones:
              // once work has a clone, the clone is the thing you wanted, and the ticket is
              // no longer in the column to select anyway.
              resolveLink={(id) => {
                const ticket = findTicket(id, visibleTickets);
                if (ticket) {
                  return {
                    title: `Show ${ticket.id} in this panel`,
                    open: () => setOpenTicketId(ticket.id),
                  };
                }
                const clone = cloneForTicket(id, state.hosts);
                if (clone) {
                  return {
                    title: `Show the clone for ${id}: ${clone.id}`,
                    open: () => {
                      setOpenTicketId(null);
                      run(activate(clone.id));
                    },
                  };
                }
                return null;
              }}
            />
          ) : undefined
        }
        error={error}
        sideFocus={sideFocus}
        onSideFocusChange={setSideFocus}
        initialSideWidth={sideWidth}
        onSideWidthCommit={rememberSideWidth}
        // The operator's locale, read here rather than in the usage bars, so the reset-time
        // tooltips are a function of the shell's props like everything else it draws.
        locale={browserLocale()}
        rail={{
          accounts,
          accountOrder: acctOrder,
          cloneGroups,
          codexGroups,
          now,
          lxcStats,
          operations: state.operations,
          presetNames: state.layoutPresetNames ?? [],
          activeLayout: state.activeLayout ?? "",
          onActivateLayout: (name) => run(activateLayout(name)),
          onOpenSettings: () => setSettingsOpen(true),
          onImportAccount: () => setImportOpen(true),
          onRefresh: () => {
            void Promise.all([refreshClaudeUsage(), refreshCodexUsage()]);
          },
        }}
        board={{
          columns,
          clones: state.hosts,
          stats,
          cloneTokens: state.cloneTokens,
          forwards,
          operations: state.operations,
          // A ticket takes the highlight while it is open; the clone stays activated
          // underneath, so the viewer keeps its stream.
          selectedId: openTicket ? null : state.selected,
          // The card copies an `ssh -J` one-liner, and the jump target is this page's own
          // address whenever no public host is configured. That read is the browser's, so it
          // happens here and the card is handed the answer.
          sshPublicHost: sshPublicHost || window.location.hostname,
          bastionPort,
          // Picking a clone always closes the ticket, the currently selected clone
          // included: that click means "back to this clone", and there is nothing else it
          // could mean once its own card is already the one activated.
          onSelectClone: (clone) => {
            setOpenTicketId(null);
            run(activate(clone.id));
          },
          onDeleteClone: (clone) => {
            // Deleting a parent cascades to its sub clones (server-side), so say so up front.
            const subCount = state.hosts.filter((h) => h.parent === clone.id).length;
            const subs =
              subCount > 0 ? ` and its ${subCount} sub clone${subCount === 1 ? "" : "s"}` : "";
            const msg = clone.managed
              ? `Delete ${clone.id}${subs}? This destroys its container${subCount > 0 ? "s" : ""}.`
              : `Remove ${clone.id}? This unregisters the clone.`;
            if (confirm(msg)) run(deleteClone(clone.id));
          },
          onCommitClone: (clone) => setCommitClone(clone),
          onChangeAccountClone: (clone) => setChangeClone(clone),
          onPortForwardClone: (clone) => {
            setForwardError(null);
            setForwardClone(clone);
          },
          onArchiveClone: (clone) => archiveDrop(clone, true),
          onUnarchiveClone: (clone) => archiveDrop(clone, false),
          onCopySshCommand: copyText,
          onOpenInLinear: openInLinear,
          tickets: {
            tickets: visibleTickets,
            // Nothing asked yet is not an empty queue. Without this the column claims every
            // open ticket already has a clone for as long as the first fetch takes.
            loading: ticketsLoading,
            error: ticketsError,
            selectedId: openTicket?.id ?? null,
            onSelectTicket: (ticket) => setOpenTicketId(ticket.id),
            onNewTicket: () => setNewTicketOpen(true),
            onOpenInLinear: openInLinear,
            onCopyBranchName: copyText,
            onCopyTicketLink: copyText,
          },
          onNewCloneFromTicket: (ticket, columnId) => {
            setNewCloneColumn(columnId);
            setTicketPrefill(ticket.url);
            setCloneOpen(true);
          },
          onReorderTickets: applyTicketOrder,
          onNewClone: (columnId) => {
            setNewCloneColumn(columnId);
            setCloneOpen(true);
          },
          onMoveCard: (cloneId, toColumnId, toIndex) =>
            applyColumns(moveCard(columns, cloneId, toColumnId, toIndex)),
          onRenameColumn: (columnId, title) =>
            applyColumns(columns.map((c) => (c.id === columnId ? { ...c, title } : c))),
        }}
        notes={
          selectedClone ? (
            <ClientOnly>
              <Suspense
                fallback={
                  <div className="p-6 text-sm text-slate-400 dark:text-slate-500">Loading editor…</div>
                }
              >
                <NotesEditorContainer key={selectedClone.id} cloneId={selectedClone.id} />
              </Suspense>
            </ClientOnly>
          ) : null
        }
        chat={
          selectedClone ? (
            <ClientOnly>
              <Suspense
                fallback={
                  <div className="p-4 text-sm text-slate-400 dark:text-slate-500">Loading chat…</div>
                }
              >
                <ChatContainer
                  key={selectedClone.id}
                  cloneId={selectedClone.id}
                  archived={selectedClone.archived === true}
                />
              </Suspense>
            </ClientOnly>
          ) : null
        }
      />

      {newTicketOpen ? (
        <TicketModalContainer
          presets={presets}
          onClose={() => setNewTicketOpen(false)}
          // Linear answered with the whole ticket, so the column can draw it now. Opening
          // its panel is the useful next step: a ticket worth creating is one you are about
          // to work on, and the panel needs the ticket to be in the list to find it.
          onCreated={(created) => {
            upsertTicket(created);
            setOpenTicketId(created.id);
            refetchTickets();
          }}
        />
      ) : null}

      {cloneOpen ? (
        <CloneModalContainer
          images={images}
          imagesLoading={imagesLoading}
          operations={state.operations}
          parentCandidate={subCloneParent}
          accounts={accounts}
          initialTicket={ticketPrefill}
          onClose={() => {
            setCloneOpen(false);
            setNewCloneColumn(null);
            setTicketPrefill("");
            // Land on the clone that was just made. The dialog only closes once its
            // operation has settled, so by the time this runs the clone either exists or
            // the create failed — hence the check, so a failed create leaves the current
            // selection alone rather than pointing at a clone that never appeared.
            if (newClone && state.hosts.some((h) => h.id === newClone)) {
              setOpenTicketId(null);
              run(activate(newClone));
            }
            setNewClone(null);
          }}
          // The dialog owns the whole lifecycle now: it keeps itself open, renders the op's
          // progress, and closes when the op settles. So this just starts it and hands the
          // Operation back — errors surface inside the dialog, not in the page banner.
          // The op's target is the new clone's id, which is how it reaches the column
          // whose button opened this.
          onClone={(image, payload) =>
            duplicateClone(image, payload).then((op) => {
              if (newCloneColumn) {
                setPendingColumn({ columnId: newCloneColumn, target: op.target });
              }
              // The op's target is the new clone's id; `onClose` selects it once the
              // dialog settles, so making a clone leaves the operator looking at it.
              setNewClone(op.target);
              return op;
            })
          }
        />
      ) : null}

      {settingsOpen ? (
        <SettingsPanelContainer
          accounts={accounts}
          onClose={() => setSettingsOpen(false)}
          getConfig={getConfig}
          putConfig={putConfig}
          testConfig={testConfig}
          getUpdateStatus={getUpdateStatus}
          updateServer={updateServer}
          operations={state.operations}
          restartServer={restartServer}
          images={images}
          imagesLoading={imagesLoading}
          pullBusy={state.operations.some(
            (o) => o.kind === "pull" && o.status === "running",
          )}
          onPullTemplate={(reference) => run(pullTemplate(reference))}
          onDeleteImage={(reference) => run(deleteImage(reference))}
          onImportAccount={() => setImportOpen(true)}
          onDeleteAccount={onDeleteAccount}
          onDeleteCodexAccount={onDeleteCodexAccount}
          boardColumns={columns}
          boardColumnCounts={columnCounts}
          onAddBoardColumn={(title) =>
            applyColumns([
              ...columns,
              { id: newColumnId(title, columns), title, cloneIds: [], archive: false },
            ])
          }
          onRenameBoardColumn={(columnId, title) =>
            applyColumns(columns.map((c) => (c.id === columnId ? { ...c, title } : c)))
          }
          onSetBoardColumnArchive={(columnId, archive) =>
            applyColumns(columns.map((c) => (c.id === columnId ? { ...c, archive } : c)))
          }
          onDeleteBoardColumn={(columnId) => applyColumns(removeColumn(columns, columnId))}
          onReorderBoardColumns={(ids) =>
            applyColumns(
              ids.flatMap((id) => {
                const column = columns.find((c) => c.id === id);
                return column ? [column] : [];
              }),
            )
          }
        />
      ) : null}

      {commitClone ? (
        <CommitImageModal
          cloneId={commitClone.id}
          busy={committing}
          onClose={() => setCommitClone(null)}
          onCommit={(name) => {
            setCommitting(true);
            commitImage(commitClone.id, name)
              .then(() => setError(null))
              .catch((e: Error) => setError(e.message))
              .finally(() => {
                setCommitting(false);
                setCommitClone(null);
              });
          }}
        />
      ) : null}

      {importOpen ? (
        <ImportAccountModalContainer
          clones={state.hosts}
          onClose={() => setImportOpen(false)}
          onImported={() => {
            setImportOpen(false);
            // The account is in the server's store immediately; its usage windows fill in on
            // the next poll, which these kick off rather than waiting out the interval.
            setError(null);
            run(refreshClaudeUsage());
            run(refreshCodexUsage());
          }}
        />
      ) : null}

      {changeClone ? (
        <ChangeAccountModalContainer
          clone={changeClone}
          accounts={claudeAccounts}
          codexAccounts={codexAccounts}
          busy={changing}
          onClose={() => setChangeClone(null)}
          onSubmit={(claude, codex) => {
            setChanging(true);
            Promise.all([
              swapClaudeAccount(changeClone.id, claude),
              swapCodexAccount(changeClone.id, codex),
            ])
              .then(() => {
                setError(null);
                setChangeClone(null);
              })
              .catch((e: Error) => setError(e.message))
              .finally(() => setChanging(false));
          }}
        />
      ) : null}

      {forwardClone ? (
        <PortForwardModal
          clone={state.hosts.find((h) => h.id === forwardClone.id) ?? forwardClone}
          runtime={forwards[forwardClone.id] ?? []}
          busy={forwarding}
          error={forwardError}
          onClose={() => setForwardClone(null)}
          onSubmit={(list) => {
            setForwarding(true);
            setForwardError(null);
            putForwards(forwardClone.id, list)
              .then(() => setForwardClone(null))
              .catch((e: Error) => setForwardError(e.message))
              .finally(() => setForwarding(false));
          }}
        />
      ) : null}
    </>
  );
}
