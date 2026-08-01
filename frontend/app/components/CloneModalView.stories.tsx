import type { Meta, StoryObj } from "@storybook/react-vite";
import { useCallback, useState } from "react";
import { fn } from "storybook/test";

import { CloneModalView } from "./CloneModalView";
import { MarkdownEditorView } from "./MarkdownEditorView";
import { makeClaudeAccounts, makeCloneGroups, makeCodexGroups } from "./__fixtures__/accounts";
import { cloneTicketUrl, makeCloneDraft } from "./__fixtures__/cloneDialog";
import { makeCloneWorking } from "./__fixtures__/clones";
import { images } from "./__fixtures__/images";
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
    // Read-only server data: the dialog filters and maps these and never holds them, so one
    // set behind the stories cannot leak an edit from one into the next. The draft is the one
    // thing the dialog does edit, which is why every story builds its own.
    images,
    imagesLoading: false,
    accounts: makeClaudeAccounts(),
    claudeGroups: makeCloneGroups(),
    codexGroups: makeCodexGroups(),
    parentCandidate: makeCloneWorking(),
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

/** How the dialog opens from a column's own New clone button: an image already picked, the
 *  Existing-ticket tab, and nothing typed. Clone is dead until a ticket parses. */
export const Default: Story = {};

/** Opened by a ticket — dragged onto a column, or from the ticket panel's own button. The
 *  link is already in the field, so the id, the hostname and the preset are all resolved
 *  before the operator has done anything, and Clone is live. */
export const FromTicket: Story = {
  args: form(makeCloneDraft({ ticket: cloneTicketUrl })),
};

/** The New-ticket tab. The team dropdown is also the preset selector, so the resolved-preset
 *  line is gone; the description editor takes its place, and the button reads Create & clone. */
export const NewTicket: Story = {
  args: form(makeCloneDraft({ mode: "create", team: "we", title: "Tighten the metric row" })),
};

/** The No-ticket tab: a container title, an optional first turn for the agent, and a preset
 *  picked by hand. Nothing here touches Linear, so this is the only tab that cannot be blocked
 *  by a missing key — and the only one with no instruction overrides. */
export const NoTicket: Story = {
  args: form(
    makeCloneDraft({
      mode: "plain",
      title: "encoder-scratch",
      message: "Read the VA-API notes, then summarize the options.",
      plainPreset: "webapp",
    }),
  ),
};

/** A validation failure the operator cannot type their way out of: creating a ticket needs
 *  the resolved preset's own Linear key, and this team's preset has none. The warning names
 *  the preset and Clone stays dead until they pick another team or add the key. */
export const MissingLinearKey: Story = {
  args: form(makeCloneDraft({ mode: "create", team: "per", title: "Encoder spike" })),
};

/** The clone is running. The form and both buttons lock, Escape is swallowed rather than
 *  closing over the operation, and the op's own progress renders under the fields. */
export const Cloning: Story = {
  args: {
    ...form(makeCloneDraft({ ticket: cloneTicketUrl })),
    busy: true,
    operation: makeOperation({ target: "pega-we-142", source: images[0].reference }),
  },
};

/** The start failed. The dialog keeps the whole form so the attempt can be retried as it
 *  stands, and says why in its own footer rather than the page banner. */
export const WithError: Story = {
  args: {
    ...form(makeCloneDraft({ ticket: cloneTicketUrl })),
    error: "clone: no image named pegasis0/rmng-template:latest",
  },
};

/** The dialog wired to local state instead of the container: every field edits, switching
 *  tabs re-derives the preset and the Clone button, and Clone runs a stand-in operation that
 *  finishes after a beat. */
export const Interactive: Story = {
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
