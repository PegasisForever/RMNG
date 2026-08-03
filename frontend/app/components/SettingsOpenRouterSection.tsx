// The credential behind working-vs-stuck. RMNG decides whether a quiet clone is still
// getting somewhere by asking a cheap model one question about it, and this is the key it
// asks with.
//
// The consequence of leaving it blank is stated in the section itself rather than left for
// the operator to discover from a dashboard full of grey dots: with no key, nothing answers
// the question, so no clone is ever reported as working.
import { Field, Secret, Section, settingsInput } from "~/components/SettingsFields";

export function SettingsOpenRouterSection({
  keyValue,
  keySet,
  model,
  onKeyChange,
  onModelChange,
  onTest,
  testMessage,
}: {
  keyValue: string;
  keySet: boolean;
  model: string;
  onKeyChange: (v: string) => void;
  onModelChange: (v: string) => void;
  onTest: () => void;
  testMessage: string | null;
}) {
  return (
    <Section
      title="Stuck detection (OpenRouter)"
      effect="immediate"
      hint="RMNG reads each clone's own session registry and agent hooks to tell working from stuck. Most clones are decided from those files alone; the undecidable ones are settled by one cheap model call. Without a key nothing settles them, so no clone reads as working. Per-clone token counting is unaffected either way."
    >
      <Secret
        label="OpenRouter API key"
        set={keySet}
        value={keyValue}
        onChange={onKeyChange}
      />
      <div className="mt-1.5">
        <Field label="Model">
          <input
            value={model}
            onChange={(e) => onModelChange(e.target.value)}
            placeholder="~deepseek/deepseek-v4-flash-latest"
            spellCheck={false}
            className={settingsInput}
          />
        </Field>
      </div>
      <p className="mt-1 text-xs text-slate-400 dark:text-slate-500">
        Cheap and fast beats clever here: the question is one boolean about one container,
        asked once per clone per state change. The default costs roughly $0.01 per
        clone-hour.
      </p>
      <div className="mt-1.5 flex items-center gap-2">
        <button
          type="button"
          onClick={onTest}
          className="rounded border border-slate-300 dark:border-slate-600 px-2.5 py-1.5 text-xs text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
        >
          Test key
        </button>
        {testMessage ? (
          <p className="text-xs text-slate-500 dark:text-slate-400">{testMessage}</p>
        ) : null}
      </div>
    </Section>
  );
}
