import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { SettingsPanel } from "./SettingsPanel";
import { accountEmails, appConfig, images } from "~/stories/fixtures";

// Mocked server calls — the component never imports the real API, so a story just
// injects these. `fn(impl)` both runs the implementation and records the call in the
// Actions panel.
const getConfig = () => fn(async () => appConfig);
const putConfig = (restartRequired = false) =>
  fn(async () => ({ config: appConfig, restartRequired }));
const testConfig = () =>
  fn(async () => ({ ok: true, message: "Docker reachable (Engine 27.1.1)" }));
const applyMonitors = () =>
  fn(async () => ({ ok: true, applied: ["pega-we-142", "pega-dev-88"], errors: [] }));
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

const meta = {
  title: "Settings/SettingsPanel",
  component: SettingsPanel,
  parameters: { layout: "fullscreen" },
  args: {
    accountEmails,
    onClose: fn(),
    getConfig: getConfig(),
    putConfig: putConfig(),
    testConfig: testConfig(),
    applyMonitors: applyMonitors(),
    getUpdateStatus: getUpdateStatus(),
    updateServer: fn(),
    restartServer: fn(async () => ({ ok: true })),
    images,
    imagesLoading: false,
    pullBusy: false,
    onPullTemplate: fn(),
    onDeleteImage: fn(),
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

/** No Claude accounts imported yet — groups can't add members. */
export const NoAccounts: Story = {
  args: { accountEmails: [] },
};
