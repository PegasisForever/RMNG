import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { fn } from "storybook/test";

import { ImportAccountModalView, type ImportCandidate } from "./ImportAccountModalView";
import { makeCloneIdle, makeCloneWorking } from "./__fixtures__/clones";

/** The clones the dialog offers: managed containers only, which is the filter the container
 *  applies before handing the list over. */
function makeImportableClones() {
  return [makeCloneWorking(), makeCloneIdle()];
}

const signedIn: ImportCandidate = { email: "alex@example.com", plan: "Max 20x" };

const meta = {
  title: "Settings/Components/ImportAccountModalView",
  component: ImportAccountModalView,
  parameters: { layout: "fullscreen" },
  args: {
    provider: "claude",
    clones: makeImportableClones(),
    cloneId: makeCloneWorking().id,
    info: null,
    checking: false,
    importing: false,
    error: null,
    onProviderChange: fn(),
    onCloneIdChange: fn(),
    onClose: fn(),
    onImport: fn(),
    mode: "clone" as const,
    loginUrl: null,
    pasted: "",
    onModeChange: fn(),
    onPastedChange: fn(),
  },
} satisfies Meta<typeof ImportAccountModalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** How it opens: the first managed clone selected and its login already being checked. Import
 *  stays dead until the check answers, because there is nothing yet to import. */
export const Checking: Story = {
  args: { checking: true },
};

/** The check came back. The clone is signed in, the account and its plan are named, and
 *  Import is live. */
export const SignedIn: Story = {
  args: { info: signedIn, clones: makeImportableClones() },
};

/** The Codex side of the same dialog. Same flow, different provider: the heading, the status
 *  line and both server calls follow the toggle. */
export const Codex: Story = {
  args: {
    provider: "codex",
    info: { email: "alex@openai.com", plan: "Pro" },
    clones: makeImportableClones(),
  },
};

/** The import is in flight. The button holds its label and takes no second click, and the
 *  dialog stays put until the server answers — closing it now would leave the harvest half
 *  done. */
export const Importing: Story = {
  args: { info: signedIn, importing: true, clones: makeImportableClones() },
};

/** The clone is not signed in, or the check could not reach it. The status line stays empty
 *  and the server's own words go in the banner. */
export const WithError: Story = {
  args: {
    error: "clone 'pega-we-142' is not signed in to Claude Code",
    clones: makeImportableClones(),
  },
};

/** Nothing to import from: no managed clone exists yet. The picker is replaced by the reason,
 *  and Import can never light up. */
export const NoClones: Story = {
  args: { clones: [], cloneId: "" },
};

/** The dialog wired to local state: switching provider or clone re-runs a stand-in check, and
 *  Import runs a stand-in harvest that fails for the clone nobody is signed in on. */
export const Interactive: Story = {
  render: function Render(args) {
    const clones = makeImportableClones();
    const [provider, setProvider] = useState(args.provider);
    const [cloneId, setCloneId] = useState(clones[0].id);
    const [info, setInfo] = useState<ImportCandidate | null>(signedIn);
    const [checking, setChecking] = useState(false);
    const [importing, setImporting] = useState(false);
    const [error, setError] = useState<string | null>(null);

    // The stand-in for the check. Only the first clone is signed in, so picking the second one
    // shows the failure path without a server.
    const check = (nextProvider: "claude" | "codex", nextCloneId: string) => {
      setInfo(null);
      setError(null);
      setChecking(true);
      window.setTimeout(() => {
        setChecking(false);
        if (nextCloneId === clones[0].id) {
          setInfo({
            email: nextProvider === "codex" ? "alex@openai.com" : "alex@example.com",
            plan: nextProvider === "codex" ? "Pro" : "Max 20x",
          });
        } else {
          setError(`clone '${nextCloneId}' is not signed in to Claude Code`);
        }
      }, 700);
    };

    return (
      <ImportAccountModalView
        {...args}
        clones={clones}
        provider={provider}
        cloneId={cloneId}
        info={info}
        checking={checking}
        importing={importing}
        error={error}
        onProviderChange={(next) => {
          setProvider(next);
          check(next, cloneId);
          args.onProviderChange(next);
        }}
        onCloneIdChange={(next) => {
          setCloneId(next);
          check(provider, next);
          args.onCloneIdChange(next);
        }}
        onImport={() => {
          setImporting(true);
          args.onImport();
          window.setTimeout(() => setImporting(false), 900);
        }}
      />
    );
  },
};

/** The sign-in path, waiting for its URL. Nothing to copy yet. */
export const SigningInPreparing: Story = {
  args: { mode: "login", loginUrl: null },
};

/** The sign-in path with the URL in hand: open it, land on a dead port, paste the address
 *  back. The dead page is the expected outcome, so the copy says so. */
export const SigningIn: Story = {
  args: {
    mode: "login",
    loginUrl:
      "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&scope=user%3Aprofile%20user%3Ainference&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&state=Ny0kZDh",
  },
};

/** Pasted, ready to finish. */
export const SigningInPasted: Story = {
  args: {
    mode: "login",
    loginUrl: "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a",
    pasted: "http://localhost:54545/callback?code=ac_01H8x9#Ny0kZDh",
  },
};

/** The provider refused the code, which is the one error this path produces often: the code
 *  is single-use and expires within minutes. */
export const SigningInRefused: Story = {
  args: {
    mode: "login",
    loginUrl: "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a",
    pasted: "http://localhost:54545/callback?code=ac_01H8x9",
    error: "the provider refused the code: 400: {\"error\":\"invalid_grant\"}",
  },
};
