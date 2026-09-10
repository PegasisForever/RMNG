import type { Meta, StoryObj } from "@storybook/react-vite";
import { useCallback, useState } from "react";
import { fn } from "storybook/test";

import { CloneModalView } from "./CloneModalView";
import { MarkdownEditorView } from "./MarkdownEditorView";
import {
  accountsNow,
  makeClaudeAccounts,
  makeCloneGroups,
  makeCodexGroups,
} from "./__fixtures__/accounts";
import { cloneTicketUrl, makeCloneDraft } from "./__fixtures__/cloneDialog";
import { makeCloneWorking } from "./__fixtures__/clones";
import { makeOperation } from "./__fixtures__/operations";
import { makeClonePresets } from "./__fixtures__/presets";
import {
  cloneDraftValid,
  linearKeyMissing,
  resolvePreset,
  teamKeysOf,
  type CloneDraft,
} from "~/lib/cloneDraft";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";
import { parseTicketInput } from "~/lib/workspace";

/** Everything the dialog is TOLD rather than asked to work out: the parse, the preset the
 *  open tab resolves to, whether the request needs a Linear key nobody configured, and
 *  whether the button may fire. The container derives these from exactly these functions, so
 *  deriving them here keeps a story from claiming a combination the dialog cannot be in.
 *
 *  `configLoaded` is true throughout: the pre-config flicker lasts one round trip and is not
 *  a state anyone reviews. */
function derive(draft: CloneDraft, presets: PresetRedacted[]) {
  const parsedTicket = parseTicketInput(draft.ticket);
  const preset = resolvePreset(draft.mode, presets, {
    plainPreset: draft.plainPreset,
    templatePreset: draft.templatePreset,
    team: draft.team,
    ticketPrefix: parsedTicket?.prefix,
  });
  const keyMissing = linearKeyMissing(draft.mode, presets, preset, true);
  return {
    presets,
    teamKeys: teamKeysOf(presets),
    parsedTicket,
    preset,
    linearKeyMissing: keyMissing,
    valid: cloneDraftValid(draft, {
      presets,
      preset,
      ticketParsed: !!parsedTicket,
      keyMissing,
    }),
  };
}

/** One story's worth of form: the draft plus everything that follows from it. */
function form(draft: CloneDraft, presets: PresetRedacted[] = makeClonePresets()) {
  return { draft, ...derive(draft, presets) };
}

/** The server-side lists the dialog draws from, rebuilt per story. Nothing below copies them
 *  into state today, but a builder called once at module load is the shape that starts
 *  leaking the moment something does, so each story gets its own. */
function sources() {
  const clones = [
    makeCloneWorking(),
    makeCloneWorking({ id: "pega-dev-88", linearTicket: undefined }),
    makeCloneWorking({ id: "pega-ops-7", linearTicket: undefined }),
  ];
  return {
    clones,
    accounts: makeClaudeAccounts(accountsNow),
    claudeGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
  };
}

/** The real description slot, on a stub upload: a pasted image really appears, from the copy
 *  already in the browser's memory. The live field posts to /api/upload instead. */
const descriptionEditor = (
  <MarkdownEditorView
    onChange={fn()}
    uploadFile={async (file: File) => URL.createObjectURL(file)}
    placeholder="What needs doing — paste images, format freely"
  />
);

const meta = {
  title: "Clone/Components/CloneModalView",
  component: CloneModalView,
  parameters: { layout: "fullscreen" },
  args: {
    ...sources(),
    clonesLoading: false,
    descriptionEditor,
    busy: false,
    error: null,
    operation: null,
    onDraftChange: fn(),
    onSubmit: fn(),
    onClose: fn(),
    ...form(makeCloneDraft()),
  },
} satisfies Meta<typeof CloneModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** How the dialog opens from a column's own New clone button: a source clone already
 *  picked and the Existing-ticket tab. Fork stays dead until a ticket parses. */
export const Default: Story = { args: { ...sources() } };

/** Opened by a ticket — dragged onto a column, or from the ticket panel's own button. The
 *  link is already in the field, so the preset and the button resolve before the operator
 *  has done anything else, and Fork is live. */
export const FromTicket: Story = {
  args: { ...sources(), ...form(makeCloneDraft({ ticket: cloneTicketUrl })) },
};

/** The New-ticket tab. The team dropdown is also the preset selector, so the resolved-preset
 *  line is gone; the description editor takes its place. */
export const NewTicket: Story = {
  args: {
    ...sources(),
    ...form(makeCloneDraft({ mode: "create", team: "we", title: "Tighten the metric row" })),
  },
};

/** The No-ticket tab: a container title, an optional first turn for the agent, and a preset
 *  picked by hand. Nothing here touches Linear, so this is the only tab that cannot be blocked
 *  by a missing key — and the only one with no instruction overrides. */
export const NoTicket: Story = {
  args: {
    ...sources(),
    ...form(
      makeCloneDraft({
        mode: "plain",
        title: "encoder-scratch",
        message: "Read the VA-API notes, then summarize the options.",
        plainPreset: "webapp",
      }),
    ),
  },
};

/** A validation failure the operator cannot type their way out of: creating a ticket needs
 *  the resolved preset's own Linear key, and this team's preset has none. The warning names
 *  the preset and Fork stays dead until they pick another team or add the key. */
export const MissingLinearKey: Story = {
  args: {
    ...sources(),
    ...form(makeCloneDraft({ mode: "create", team: "ops", title: "Encoder spike" })),
  },
};

/** The fork is running. The form and both buttons lock, Escape is swallowed rather than
 *  closing over the operation, and the op's own progress renders under the fields. */
export const Forking: Story = {
  args: {
    ...sources(),
    ...form(makeCloneDraft({ ticket: cloneTicketUrl })),
    busy: true,
    operation: makeOperation({ target: "pega-we-143", source: "pega-we-142" }),
  },
};

/** The From-template tab: title plus a hand-picked preset onto a fresh empty home. No
 *  source picker (there is nothing to fork), no account overrides (the preset's defaults
 *  drive), no Linear warning, and the button reads Clone. */
export const Template: Story = {
  args: {
    ...sources(),
    ...form(
      makeCloneDraft({ mode: "template", title: "encoder-scratch", templatePreset: "webapp" }),
    ),
  },
};

/** The start failed. The dialog keeps the whole form so the attempt can be retried as it
 *  stands, and says why in its own footer rather than the page banner. */
export const WithError: Story = {
  args: {
    ...sources(),
    ...form(makeCloneDraft({ ticket: cloneTicketUrl })),
    error: "fork: a clone named 'pega-we-143' already exists",
  },
};

/** The dialog wired to local state instead of the container: every field edits, switching
 *  tabs re-derives the preset and the Fork button, and Fork runs a stand-in operation that
 *  finishes after a beat. */
export const Interactive: Story = {
  args: { ...sources() },
  render: function Render(args) {
    const [draft, setDraft] = useState(args.draft);
    const [busy, setBusy] = useState(false);
    const presets = makeClonePresets();
    const update = useCallback(
      <K extends keyof CloneDraft>(key: K, value: CloneDraft[K]) =>
        setDraft((d) => ({ ...d, [key]: value })),
      [],
    );
    return (
      <CloneModalView
        {...args}
        {...derive(draft, presets)}
        draft={draft}
        onDraftChange={(key, value) => {
          update(key, value);
          args.onDraftChange(key, value);
        }}
        busy={busy}
        onSubmit={() => {
          setBusy(true);
          args.onSubmit();
          // The point is the shape of a start, not the server: lock the form, then let go.
          window.setTimeout(() => setBusy(false), 1600);
        }}
      />
    );
  },
};
