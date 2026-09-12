import { Field, Section, settingsInput } from "~/components/SettingsFields";
import { DropdownSelect } from "~/components/DropdownSelect";
import type { JudgeProvider } from "~/lib/wire/JudgeProvider";

export function SettingsStuckSection({
  provider,
  geminiKey,
  onProviderChange,
  onGeminiKeyChange,
  onTest,
  testMessage,
}: {
  provider: JudgeProvider;
  geminiKey: string;
  onProviderChange: (v: JudgeProvider) => void;
  onGeminiKeyChange: (v: string) => void;
  onTest: () => void;
  testMessage: string | null;
}) {
  return (
    <Section title="Stuck detection" effect="immediate">
      <Field label="Provider">
        <DropdownSelect
          rows={[
            { value: "codex", label: "ChatGPT account" },
            { value: "gemini", label: "Gemini API" },
          ]}
          value={provider}
          onChange={(v) => onProviderChange(v as JudgeProvider)}
          label="Provider"
          className={settingsInput}
        />
      </Field>
      {provider === "gemini" ? (
        <div className="mt-3">
          <Field label="Gemini API key (visible)">
            <input
              value={geminiKey}
              onChange={(e) => onGeminiKeyChange(e.target.value)}
              placeholder="(none)"
              spellCheck={false}
              autoComplete="off"
              className={settingsInput}
            />
          </Field>
        </div>
      ) : null}
      <div className="mt-1.5 flex items-center gap-2">
        <button
          type="button"
          onClick={onTest}
          className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          Test judge
        </button>
        {testMessage ? (
          <p className="text-xs text-slate-500 dark:text-slate-400">
            {testMessage}
          </p>
        ) : null}
      </div>
    </Section>
  );
}
