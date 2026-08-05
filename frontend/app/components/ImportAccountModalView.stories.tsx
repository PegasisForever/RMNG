import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { ImportAccountModalView } from "./ImportAccountModalView";

/** A real Claude authorize URL, elided in the middle: the dialog has to stay readable while
 *  showing something this long, since the operator has to click it. */
const CLAUDE_URL =
  "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e" +
  "&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback" +
  "&scope=user%3Aprofile%20user%3Ainference%20user%3Asessions%3Aclaude_code" +
  "&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256" +
  "&state=Ny0kZDh1TGpDcVlXd0FfVA";

const meta = {
  title: "Settings/Components/ImportAccountModalView",
  component: ImportAccountModalView,
  parameters: { layout: "fullscreen" },
  args: {
    provider: "claude" as const,
    loginUrl: CLAUDE_URL,
    pasted: "",
    groups: ["Personal", "Medi"],
    group: "",
    importing: false,
    error: null,
    onProviderChange: fn(),
    onPastedChange: fn(),
    onGroupChange: fn(),
    onClose: fn(),
    onImport: fn(),
  },
} satisfies Meta<typeof ImportAccountModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** As it opens: the URL to click, and an empty box waiting for the address it lands on. */
export const Default: Story = { args: {} };

/** Before the server has answered `begin`. There is nothing to copy yet, so the dialog says
 *  so rather than showing an empty link. */
export const Preparing: Story = { args: { loginUrl: null } };

/** Pasted and about to be redeemed, joining a pool on the way in. */
export const Pasted: Story = {
  args: {
    pasted: "http://localhost:54545/callback?code=ac_01H8x9Qw#Ny0kZDh1TGpDcVlXd0FfVA",
    group: "Personal",
  },
};

/** Mid-exchange. The button holds its own label and takes no second click. */
export const Finishing: Story = {
  args: { pasted: "http://localhost:54545/callback?code=ac_01H8x9Qw", importing: true },
};

/** Codex, which has its own redirect port and its own pools. */
export const Codex: Story = {
  args: { provider: "codex", groups: ["Shared"], loginUrl: CLAUDE_URL.replace("claude.ai", "auth.openai.com") },
};

/** No pools configured for this provider: the picker is inert and says what that means, so
 *  an account added here does not silently become one no clone can be handed. */
export const NoPools: Story = { args: { groups: [] } };

/** The provider refused the code. The commonest real error on this path, because the code is
 *  single-use and expires within minutes of the sign-in. */
export const Refused: Story = {
  args: {
    pasted: "http://localhost:54545/callback?code=ac_01H8x9Qw",
    error: 'the provider refused the code: 400: {"error":"invalid_grant"}',
  },
};
