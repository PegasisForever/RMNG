// How RMNG tells a clone that is thinking from one that is waiting on you. Most clones are
// decided from their own files; the ones that cannot be get one question put to GPT, on an
// imported Codex account's ChatGPT plan.
//
// There is no key to paste. The server already holds that account's token to run the clones,
// which is also why the account picker only offers accounts that have been imported.
//
// The consequence of importing none is stated in the section rather than left for the
// operator to discover from a dashboard full of grey dots: with nobody to ask, no clone is
// ever reported as working.
import { Field, Section, settingsInput } from "~/components/SettingsFields";

export function SettingsStuckSection({
  model,
  email,
  accounts,
  onModelChange,
  onEmailChange,
  onTest,
  testMessage,
}: {
  model: string;
  email: string;
  /** Imported Codex account emails. Empty means nothing can answer the question. */
  accounts: string[];
  onModelChange: (v: string) => void;
  onEmailChange: (v: string) => void;
  onTest: () => void;
  testMessage: string | null;
}) {
  return (
    <Section
      title="Stuck detection"
      effect="immediate"
      hint="RMNG reads each clone's own session registry and agent hooks to tell working from stuck. Most clones are decided from those files alone; the undecidable ones are settled by one GPT call on a Codex account you have imported. Import none and nothing settles them, so no clone reads as working. Per-clone token counting is unaffected either way."
    >
      <div className="grid grid-cols-2 gap-3">
        <Field label="Model">
          <input
            value={model}
            onChange={(e) => onModelChange(e.target.value)}
            placeholder="gpt-5.6-luna"
            spellCheck={false}
            className={settingsInput}
          />
        </Field>
        <Field label="Account">
          <select
            value={email}
            onChange={(e) => onEmailChange(e.target.value)}
            className={settingsInput}
          >
            <option value="">First imported account</option>
            {accounts.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
        </Field>
      </div>
      <p className="mt-1 text-xs text-slate-400 dark:text-slate-500">
        {accounts.length === 0
          ? "No Codex account is imported, so no clone can read as working. Import one under Codex."
          : "The calls go through the Codex CLI's own endpoint at medium reasoning effort, and they come out of that account's weekly ChatGPT allowance: the same one its clones spend. Nothing to pay per token, but a fleet near its weekly cap will feel it."}
      </p>
      <div className="mt-1.5 flex items-center gap-2">
        <button
          type="button"
          onClick={onTest}
          className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          Test judge
        </button>
        {testMessage ? (
          <p className="text-xs text-slate-500 dark:text-slate-400">{testMessage}</p>
        ) : null}
      </div>
    </Section>
  );
}
