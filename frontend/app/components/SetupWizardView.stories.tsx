import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { SetupWizardView } from "./SetupWizardView";
import { EnvChecklistView } from "./EnvChecklistView";
import { canPull, nextDisabled, subnetOk, type SetupDraft } from "~/lib/setupDraft";
import type { Operation } from "~/lib/types";
import { makeSetupDraft } from "./__fixtures__/appConfig";
import { makePullOperation } from "./__fixtures__/operations";
import { makeEnvCheckRow, makeEnvRows } from "./__fixtures__/setupEnv";

/** The checklist as the container mounts it, minus the probe. The wizard takes it as a slot,
 *  so a story hands over the View directly and picks which host it describes. */
function checklist(rows = makeEnvRows()) {
  return <EnvChecklistView rows={rows} loading={false} error={null} onRetry={fn()} />;
}

/** Everything a story edits, rebuilt per story. The Server step replaces the monitors array
 *  on every edit, so one set behind every story is how a drag in one shows up in the next. */
function base() {
  const draft = makeSetupDraft();
  return { draft, templatePlaceholder: draft.templateReference };
}

const templateRef = makeSetupDraft().templateReference;

const meta = {
  title: "Setup/Pages/SetupWizardView",
  component: SetupWizardView,
  parameters: { layout: "fullscreen" },
  args: {
    ...base(),
    step: 0,
    onDraftChange: fn(),
    envChecklist: checklist(),
    pullOperation: null,
    pullTarget: null,
    pulling: false,
    pullRunning: false,
    pullDone: false,
    canPull: true,
    onPull: fn(),
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
 *  live because every required check passed and the subnet parses. */
export const Environment: Story = { args: { ...base() } };

/** Step 1 on a host that cannot run clones, with a subnet the server would reject as well.
 *  Next is dead, and both reasons are on screen. */
export const EnvironmentBlocked: Story = {
  args: {
    ...base(),
    draft: makeSetupDraft({ subnet: "10.0.0.0/8" }),
    envChecklist: checklist([
      makeEnvCheckRow({ ok: false, detail: "connect /var/run/docker.sock: permission denied" }),
      ...makeEnvRows().slice(1),
    ]),
    nextDisabled: true,
  },
};

/** Step 2: the fleet's defaults. The first step is ticked in the indicator, so Back now
 *  works. */
export const Server: Story = { args: { ...base(), step: 1 } };

/** Step 3 before anything is pulled. Download is live and Skip is offered, because a rig
 *  without a template still finishes setup. */
export const DownloadTemplate: Story = { args: { ...base(), step: 2 } };

/** Step 3 mid-pull, driven by a live operation off /events. Next is dead and Skip is gone:
 *  leaving now would strand several gigabytes in flight. */
export const TemplatePulling: Story = {
  args: {
    ...base(),
    step: 2,
    pullOperation: makePullOperation(),
    pullTarget: templateRef,
    pullRunning: true,
    canPull: false,
    nextDisabled: true,
  },
};

/** Step 3 once the pull lands. The reference is locked in and the green line names it, which
 *  is what the Finish step then reports. */
export const TemplatePulled: Story = {
  args: {
    ...base(),
    step: 2,
    pullOperation: makePullOperation({ status: "done", pct: 100, message: "Pulled." }),
    pullTarget: templateRef,
    pullDone: true,
    canPull: false,
  },
};

/** Step 4: the summary, and the last moment the subnet can be changed. Next is replaced by
 *  Finish setup, which latches the config and ensures the rmng bridge. */
export const Finish: Story = {
  args: {
    ...base(),
    step: 3,
    pullTarget: templateRef,
    pullDone: true,
  },
};

/** A step's PUT is in flight. Both footer buttons are dead so the config cannot be advanced
 *  past a save that has not landed. */
export const Saving: Story = {
  args: { ...base(), step: 1, saving: true, nextDisabled: true },
};

/** The Finish click, mid-flight. Same lock, different word, because this one also latches the
 *  one-time subnet. */
export const Finishing: Story = {
  args: { ...base(), step: 3, saving: true },
};

/** A step's PUT was rejected. The banner sits above the step, the form keeps what was typed,
 *  and the wizard stays where it is so the attempt can be retried as it stands. */
export const WithError: Story = {
  args: {
    ...base(),
    error: "PUT /api/config: 400 subnet 10.99.0.0/24 overlaps an existing docker network",
  },
};

/** The wizard wired to local state instead of the container: Next and Back really move, every
 *  field really edits, and Download really runs a pull that fills its bar and finishes. The
 *  gating comes from `~/lib/setupDraft`, so what is dead here is dead in the app. */
export const Interactive: Story = {
  args: { ...base() },
  render: function Render(args) {
    const [step, setStep] = useState(args.step);
    const [draft, setDraft] = useState<SetupDraft>(args.draft);
    const [saving, setSaving] = useState(false);
    const [pulling, setPulling] = useState(false);
    const [pullOperation, setPullOperation] = useState<Operation | null>(null);
    const pullRunning = pullOperation?.status === "running";
    const pullDone = pullOperation?.status === "done";

    /** The shape of a step save, not the server: lock the footer for a beat, then advance. */
    const advance = () => {
      setSaving(true);
      args.onNext();
      window.setTimeout(() => {
        setSaving(false);
        setStep((s) => Math.min(3, s + 1));
      }, 700);
    };

    return (
      <SetupWizardView
        {...args}
        step={step}
        draft={draft}
        onDraftChange={(key, value) => {
          setDraft((d) => ({ ...d, [key]: value }));
          args.onDraftChange(key, value);
        }}
        pullOperation={pullOperation}
        pullTarget={pullOperation?.target ?? null}
        pulling={pulling}
        pullRunning={pullRunning}
        pullDone={pullDone}
        canPull={canPull({
          templateReference: draft.templateReference,
          pulling,
          pullRunning,
          pullDone,
        })}
        onPull={() => {
          setPulling(true);
          args.onPull();
          const target = draft.templateReference.trim() || args.templatePlaceholder;
          // The POST answers first, then the op shows up on /events, then it finishes.
          window.setTimeout(() => {
            setPulling(false);
            setPullOperation(makePullOperation({ target }));
          }, 600);
          window.setTimeout(() => {
            setPullOperation(
              makePullOperation({ target, status: "done", pct: 100, message: "Pulled." }),
            );
          }, 2600);
        }}
        saving={saving}
        nextDisabled={nextDisabled({
          step,
          saving,
          // The probe is the container's job, and the fixture host passes it.
          envOk: true,
          subnetOk: subnetOk(draft.subnet),
          pullRunning,
        })}
        onNext={advance}
        onBack={() => {
          args.onBack();
          setStep((s) => Math.max(0, s - 1));
        }}
        onFinish={() => {
          setSaving(true);
          args.onFinish();
          window.setTimeout(() => setSaving(false), 700);
        }}
      />
    );
  },
};
