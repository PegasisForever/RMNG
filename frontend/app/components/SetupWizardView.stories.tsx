import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SetupWizardView } from "./SetupWizardView";
import { EnvChecklistView } from "./EnvChecklistView";
import { nextDisabled, type SetupDraft } from "~/lib/setupDraft";
import { makeSetupDraft } from "./__fixtures__/appConfig";
import { makeEnvCheckRow, makeEnvRows } from "./__fixtures__/setupEnv";

/** The checklist as the container mounts it, minus the probe. The wizard takes it as a slot,
 *  so a story hands over the View directly and picks which host it describes. */
function checklist(rows = makeEnvRows()) {
  return (
    <EnvChecklistView rows={rows} loading={false} error={null} onRetry={fn()} />
  );
}

/** Everything a story edits, rebuilt per story. The Server step replaces the monitors array
 *  on every edit, so one set behind every story is how a drag in one shows up in the next. */
function base() {
  return {
    draft: makeSetupDraft(),
  };
}

const meta = {
  title: "Setup/Pages/SetupWizardView",
  component: SetupWizardView,
  parameters: { layout: "fullscreen" },
  args: {
    ...base(),
    step: 0,
    onDraftChange: fn(),
    envChecklist: checklist(),
    error: null,
    saving: false,
    nextDisabled: false,
    onNext: fn(),
    onBack: fn(),
    onFinish: fn(),
  },
} satisfies Meta<typeof SetupWizardView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Step 1 on a ready host. Back is dead because there is nowhere behind this, and Next is
 *  live because every required check passed. */
export const Environment: Story = { args: { ...base() } };

/** Step 1 on a host that cannot run clones. Next is dead, and the reason is on screen. */
export const EnvironmentBlocked: Story = {
  args: {
    ...base(),
    envChecklist: checklist([
      makeEnvCheckRow({
        ok: false,
        detail: "connect /var/run/docker.sock: permission denied",
      }),
      ...makeEnvRows().slice(1),
    ]),
    nextDisabled: true,
  },
};

/** Step 2: the fleet's defaults. The first step is ticked in the indicator, so Back now
 *  works. */
export const Server: Story = { args: { ...base(), step: 1 } };

/** Step 3: the summary, and the last moment the subnet can be changed. Next is replaced by
 *  Finish setup, which latches the config and ensures the rmng bridge. Clone images build
 *  on demand from each preset's Dockerfile, so there is no template step. */
export const Finish: Story = {
  args: {
    ...base(),
    step: 2,
  },
};

/** A step's PUT is in flight. Both footer buttons are dead so the config cannot be advanced
 *  past a save that has not landed. */
export const Saving: Story = {
  args: { ...base(), step: 1, saving: true, nextDisabled: true },
};

/** The Finish click, mid-flight. Same lock, different word, because this one latches setup. */
export const Finishing: Story = {
  args: { ...base(), step: 2, saving: true },
};

// Keep the helper imports referenced: the blocked/saving stories set nextDisabled by hand,
// but the rule they mirror lives here.
void nextDisabled;
type _Draft = SetupDraft;
