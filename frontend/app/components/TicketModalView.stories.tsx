import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { useArgs } from "storybook/preview-api";
import { fn } from "storybook/test";

import { MarkdownEditorView } from "./MarkdownEditorView";
import { TicketModalView, type TicketModalViewProps } from "./TicketModalView";
import { makeClonePresets } from "./__fixtures__/presets";
import { makeTeamPeople } from "./__fixtures__/people";
import { teamKeysOf } from "~/lib/cloneDraft";
import { defaultAssignee } from "~/lib/linear/people";

/** Resolves after a beat, so the button's "Creating…" state is visible in the story. */
const slowCreate = fn(
  (t: unknown) => new Promise((resolve) => setTimeout(() => resolve(t), 800)),
);

/** The real description slot, on a stub upload: a pasted image really appears, from the copy
 *  already in the browser's memory. The live field posts to Linear instead. */
const descriptionEditor = (
  <MarkdownEditorView
    onChange={fn()}
    uploadFile={async (file: File) => URL.createObjectURL(file)}
    placeholder="What needs doing — paste images, format freely"
  />
);

const people = makeTeamPeople();

const meta = {
  title: "Board/Components/TicketModalView",
  component: TicketModalView,
  parameters: { layout: "fullscreen" },
  args: {
    teams: teamKeysOf(makeClonePresets()),
    team: "we",
    onTeamChange: fn(),
    // Who the container's lookup answered with for this team, viewer first. The dialog starts
    // on that viewer, which is the whole default: a ticket you open is a ticket you take.
    people,
    assigneeId: defaultAssignee(people),
    onAssigneeChange: fn(),
    peopleLoading: false,
    description: "",
    descriptionEditor,
    onClose: fn(),
    onCreate: slowCreate,
  },
  /** The dialog positions itself over the page, so the story gives it a board-coloured one
   *  to sit on rather than a white void. Both dropdowns are live in every story, and the
   *  Controls panel follows along, so a story is a starting point rather than a still. */
  render: function Render(args) {
    const [{ team, assigneeId }, updateArgs] = useArgs<TicketModalViewProps>();
    return (
      <div className="h-screen bg-slate-50 dark:bg-slate-950">
        <TicketModalView
          {...args}
          team={team}
          assigneeId={assigneeId}
          onTeamChange={(next) => {
            updateArgs({ team: next });
            args.onTeamChange(next);
          }}
          onAssigneeChange={(next) => {
            updateArgs({ assigneeId: next });
            args.onAssigneeChange(next);
          }}
        />
      </div>
    );
  },
} satisfies Meta<typeof TicketModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The normal case: WE is the team, you are the assignee, and nothing is ranked yet. */
export const Default: Story = {};

/** The dialog reopened on the team the last ticket was created in, rather than on the first
 *  one. That memory is the container's (localStorage), and it arrives here as a prop. */
export const RememberedTeam: Story = { args: { team: "dev", people: makeTeamPeople("dev") } };

/** Somebody else takes it. The footer names them and says the consequence: this column lists
 *  what you are assigned, so the ticket will be opened and not seen here. */
export const AssignedToSomeoneElse: Story = {
  args: { assigneeId: people[1]?.id ?? "" },
};

/** The team's members are still being fetched. The field says so and takes no clicks, and a
 *  create started now still lands: an empty assignee falls back to the key's own owner. */
export const AssigneesLoading: Story = {
  args: { people: [], assigneeId: "", peopleLoading: true },
};

/** The lookup answered with nobody, or failed. The field reads "You" and is dead, because the
 *  create assigns to the key's owner when it is told nothing. */
export const NoAssignees: Story = {
  args: { people: [], assigneeId: "", peopleLoading: false },
};

/** The chosen team's preset carries no Linear key, so nothing can open a ticket there. The
 *  dialog says which preset and where to fix it, and the create button stays down. */
export const NoLinearKey: Story = {
  args: {
    teams: teamKeysOf(makeClonePresets().filter((p) => p.linearKey === "")),
    team: "ops",
    people: [],
    assigneeId: "",
  },
};

/** No preset declares a team key at all, which is the first-run state. */
export const NoTeams: Story = {
  args: { teams: [], team: "", people: [], assigneeId: "" },
};

/** Linear refused it. The dialog keeps everything typed and shows what came back. */
export const CreateFails: Story = {
  args: {
    onCreate: fn((_t: unknown) =>
      Promise.reject(new Error("Linear refused a new ticket in WE")),
    ),
  },
};

/** The dialog wired to local state, the way the container wires it: switching team swaps the
 *  people list and puts the assignee back on you, which is the one behaviour the static
 *  stories cannot show. */
export const Interactive: Story = {
  render: function Render(args) {
    const [team, setTeam] = useState(args.team);
    const forTeam = makeTeamPeople(team);
    const [assigneeId, setAssigneeId] = useState(defaultAssignee(forTeam));
    return (
      <div className="h-screen bg-slate-50 dark:bg-slate-950">
        <TicketModalView
          {...args}
          team={team}
          people={forTeam}
          assigneeId={assigneeId}
          onTeamChange={(next) => {
            setTeam(next);
            setAssigneeId(defaultAssignee(makeTeamPeople(next)));
            args.onTeamChange(next);
          }}
          onAssigneeChange={(next) => {
            setAssigneeId(next);
            args.onAssigneeChange(next);
          }}
        />
      </div>
    );
  },
};
