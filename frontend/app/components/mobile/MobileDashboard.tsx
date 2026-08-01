// The phone app's network half: it owns which clone is open, talks to the server, and
// hands the two pure pages (MobileHome, MobileClone) everything they draw.
//
// It is the counterpart of `Dashboard` in routes/_index.tsx, not a wrapper around it. The
// board, its drag handlers, and every modal except account import stay on the desktop side
// and are never imported here, so a phone downloads none of them.
//
// Navigation is one piece of state: `openId`. Null is the clone list, an id is that clone's
// screen. Selecting a clone activates it server-side the same way a board card does, which
// is what clears its unread flag and stops the monitor from notifying about output the
// operator is looking at.
import { lazy, Suspense, useEffect, useState } from "react";

import { ImportAccountModal } from "~/components/ImportAccountModal";
import { MobileClone, type CloneTab } from "~/components/mobile/MobileClone";
import { MobileHome } from "~/components/mobile/MobileHome";
import { activate, refreshClaudeUsage, refreshCodexUsage } from "~/lib/api";
import { withDefaults } from "~/lib/board";
import type { ControlState } from "~/lib/types";
import { useCloneNotifications } from "~/lib/useCloneNotifications";
import type { CloneGroup } from "~/lib/wire/CloneGroup";

// BlockNote and the chat panel are browser-only, and both are the desktop's modules
// unchanged: the phone changes their surroundings, not what they do.
const NotesEditorContainer = lazy(() => import("~/components/NotesEditorContainer"));
const ChatContainer = lazy(() => import("~/components/ChatContainer"));

function ClientOnly({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  return mounted ? <>{children}</> : null;
}

function PaneFallback({ label }: { label: string }) {
  return <p className="p-4 text-sm text-slate-400 dark:text-slate-500">{label}</p>;
}

export function MobileDashboard({
  state,
  cloneGroups,
  codexGroups,
}: {
  state: ControlState;
  /** Configured Claude pools (`config.cloneGroups`) — the usage list groups by these. */
  cloneGroups: CloneGroup[];
  /** Configured Codex pools (`config.codexGroups`). */
  codexGroups: CloneGroup[];
}) {
  const [openId, setOpenId] = useState<string | null>(null);
  const [tab, setTab] = useState<CloneTab>("chat");
  const [usageOpen, setUsageOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = (p: Promise<unknown>) =>
    p.then(() => setError(null)).catch((e: Error) => setError(e.message));

  /** Open a clone's screen and tell the server it is on view. Always lands on the chat:
   *  the notes are where you were last time, the chat is where the news is. */
  const open = (id: string) => {
    setOpenId(id);
    setTab("chat");
    run(activate(id));
  };

  // A clone that stops working while its screen is closed still notifies, and tapping the
  // notification opens it.
  useCloneNotifications(state.hosts, open);

  // Derived, not stored: a clone deleted from the desktop while its screen is open simply
  // stops resolving, and the list comes back on the next frame with no cleanup to run.
  const openClone = openId ? (state.hosts.find((c) => c.id === openId) ?? null) : null;

  if (openClone) {
    return (
      <MobileClone
        clone={openClone}
        tab={tab}
        onTabChange={setTab}
        onBack={() => {
          setOpenId(null);
          run(activate(null));
        }}
        error={error}
        notes={
          <ClientOnly>
            <Suspense fallback={<PaneFallback label="Loading notes…" />}>
              <NotesEditorContainer key={openClone.id} cloneId={openClone.id} />
            </Suspense>
          </ClientOnly>
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
        cloneGroups={cloneGroups}
        codexGroups={codexGroups}
        usageOpen={usageOpen}
        onUsageOpenChange={setUsageOpen}
        onRefresh={() => run(Promise.all([refreshClaudeUsage(), refreshCodexUsage()]))}
        onImportAccount={() => setImportOpen(true)}
        columns={withDefaults(state.boardColumns ?? [])}
        clones={state.hosts}
        onSelectClone={(clone) => open(clone.id)}
        error={error}
      />
      {importOpen ? (
        <ImportAccountModal
          clones={state.hosts}
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
