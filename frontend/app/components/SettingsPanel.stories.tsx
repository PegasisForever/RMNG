import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SettingsPanel } from "./SettingsPanel";
import type { Operation } from "~/lib/types";
import { appConfig, groups, images, usageGroups } from "~/stories/fixtures";

// Mocked server calls — the component never imports the real API, so a story just
// injects these. `fn(impl)` both runs the implementation and records the call in the
// Actions panel.
const getConfig = () => fn(async () => appConfig);
const putConfig = (restartRequired = false) =>
  fn(async () => ({ config: appConfig, restartRequired }));
const testConfig = () =>
  fn(async () => ({ ok: true, message: "Docker reachable (Engine 27.1.1)" }));
const getUpdateStatus = () =>
  fn(async () => ({
    currentRevision: "a1b2c3d",
    currentCreated: "2026-07-01T12:00:00Z",
    currentDigest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    remoteDigest: "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    available: false,
    reference: "pegasis0/rmng:latest",
    error: null,
  }));

// The group-proxy sidecar sitting on an older image than the control-server — the normal
// state right after a control-server update, since updating deliberately leaves the proxy
// running so agent turns aren't interrupted.
const getGroupProxyStatus = () =>
  fn(async () => ({
    container: "rmng-cliproxy",
    running: true,
    image: "9f8e7d6c5b4a",
    revision: "0f0e0d0",
    behind: true,
    detail: "running an older image than the control-server — restart it when clones are idle",
  }));

// A self-update op mid-flight, so the inline progress bar under the Update button renders.
const updateOp: Operation = {
  id: "op_update_1",
  kind: "update",
  target: "rmng-control",
  status: "running",
  step: "pull",
  pct: 45,
  message: "pulling pegasis0/rmng:latest",
  log: ["queued self-update", "pulling pegasis0/rmng:latest"],
  startedAt: 0,
};

const meta = {
  title: "Settings/SettingsPanel",
  component: SettingsPanel,
  parameters: { layout: "fullscreen" },
  args: {
    groups,
    usageGroups,
    onClose: fn(),
    getConfig: getConfig(),
    putConfig: putConfig(),
    testConfig: testConfig(),
    getUpdateStatus: getUpdateStatus(),
    updateServer: fn(async () => updateOp),
    // No op in flight in the default story; `UpdateInProgress` supplies one.
    operations: [],
    restartServer: fn(async () => ({ ok: true })),
    getGroupProxyStatus: getGroupProxyStatus(),
    restartGroupProxy: fn(async () => ({
      container: "rmng-cliproxy",
      running: true,
      image: "1a2b3c4d5e6f",
      revision: "a1b2c3d",
      behind: false,
      detail: "running the current image",
    })),
    images,
    imagesLoading: false,
    pullBusy: false,
    onPullTemplate: fn(),
    onDeleteImage: fn(),
    onCreateGroup: fn(),
    onDeleteGroup: fn(),
    onAddAccount: fn(),
    onDeleteAccount: fn(),
  },
} satisfies Meta<typeof SettingsPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The full settings modal, loaded from a redacted config. */
export const Default: Story = {};

/** After a save that touched a restart-required setting — shows the restart banner. */
export const RestartRequired: Story = {
  args: { putConfig: putConfig(true) },
};

/** First-run setup: subnet is still editable (not yet baked in). */
export const PreSetup: Story = {
  args: { getConfig: fn(async () => ({ ...appConfig, setupComplete: false })) },
};

/** No account groups configured yet — the manager shows the empty state. */
export const NoGroups: Story = {
  args: { groups: [], usageGroups: [] },
};

/** A self-update in flight: its progress renders inline under the Update button, so the
 *  operator doesn't have to watch the sidebar to know how far along the restart is. */
export const UpdateInProgress: Story = {
  args: { operations: [updateOp] },
};
