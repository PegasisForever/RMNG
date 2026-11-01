import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { FakeDesktop } from "~/ng/__fixtures__/desktop";

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

/** Where the "sign in again" button lands: the account's sign-in no longer works. Signing in
 *  as it repairs it; signing in as another account puts that one in its place. The provider is
 *  the dead account's and the pools are the ones it sits in, so neither control is drawn. */
export const Replacing: Story = {
  args: { replacing: "test@talktomedi.com" },
};

/** The provider refused the code. The commonest real error on this path, because the code is
 *  single-use and expires within minutes of the sign-in. */
export const Refused: Story = {
  args: {
    pasted: "http://localhost:54545/callback?code=ac_01H8x9Qw",
    error: 'the provider refused the code: 400: {"error":"invalid_grant"}',
  },
};

/** Signed in, as the same account: the dialog waits for the account list on this page to show
 *  it working before it closes. */
export const Checking: Story = {
  args: {
    replacing: "test@talktomedi.com",
    progress: { email: "test@talktomedi.com", replaced: null, works: false, oldGone: true, slow: false, problem: null },
  },
};

/** Signed in as another account: its pools and clones are moved, it works, and the old one is
 *  on its way out of the list. */
export const Replaced: Story = {
  args: {
    replacing: "test@talktomedi.com",
    progress: { email: "new@talktomedi.com", replaced: "test@talktomedi.com", works: true, oldGone: false, slow: false, problem: null },
  },
};

/** Every step done: the dialog closes by itself a moment later. */
export const Done: Story = {
  args: {
    replacing: "test@talktomedi.com",
    progress: { email: "new@talktomedi.com", replaced: "test@talktomedi.com", works: true, oldGone: true, slow: false, problem: null },
  },
};

/** Half a minute on, the account does not work yet: the dialog says so, with the server's
 *  words, and can be closed. */
export const Slow: Story = {
  args: {
    replacing: "test@talktomedi.com",
    progress: {
      email: "test@talktomedi.com",
      replaced: null,
      works: false,
      oldGone: true,
      slow: true,
      problem: "usage poll failed: 429 Too Many Requests",
    },
  },
};

/** In /ng's glass, over the clone's screen (as the Edit layout dialog there). */
export const GlassReplacing: Story = {
  args: { replacing: "talktomedi.claude.dev.0@gmail.com", glass: true },
  decorators: [
    (Story) => (
      <div className="ng-theme relative h-screen w-screen overflow-hidden bg-black">
        <FakeDesktop />
        <Story />
      </div>
    ),
  ],
};

/** A new account in /ng's glass: the provider and the pool too. */
export const GlassAdd: Story = {
  args: { glass: true },
  decorators: GlassReplacing.decorators,
};

/** The steps in /ng's glass. */
export const GlassReplaced: Story = {
  args: { ...Replaced.args, glass: true },
  decorators: GlassReplacing.decorators,
};
