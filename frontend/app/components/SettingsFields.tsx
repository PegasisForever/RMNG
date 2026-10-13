// The settings panel's shared field furniture: the input class every control uses, the
// section wrapper, a labelled field, and the write-only secret.
//
// Every section below imports from here rather than restating the class strings, so one
// control cannot drift away from the rest of the panel.

/** The class every plain settings input carries. */
export const settingsInput =
  "w-full rounded border border-slate-300 dark:border-slate-600 px-2 py-1 text-sm focus:border-slate-400 dark:focus:border-slate-500 focus:outline-none dark:bg-slate-800 dark:text-slate-100";

export function Section({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="border-t border-slate-100 dark:border-slate-800 pt-4">
      <h3 className="text-sm font-semibold text-slate-800 dark:text-slate-100">{title}</h3>
      {hint ? <p className="mb-2 mt-0.5 text-xs text-slate-400 dark:text-slate-500">{hint}</p> : <div className="mb-2" />}
      {children}
    </section>
  );
}

export function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-0.5 block text-xs font-medium text-slate-500 dark:text-slate-400">{label}</span>
      {children}
    </label>
  );
}

/** A write-only secret: blank input keeps the stored value; a "set"/"unset" badge
 *  reflects what the server currently holds. */
export function Secret({
  label,
  set,
  value,
  onChange,
}: {
  label: string;
  set: boolean;
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <Field label={label}>
      <div className="flex items-center gap-2">
        <input
          type="password"
          value={value}
          placeholder={set ? "•••••••• (set — leave blank to keep)" : "not set"}
          onChange={(e) => onChange(e.target.value)}
          className={settingsInput}
        />
        <span
          className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold ${
            set ? "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-400" : "bg-slate-100 dark:bg-slate-800 text-slate-400 dark:text-slate-500"
          }`}
        >
          {set ? "set" : "unset"}
        </span>
      </div>
    </Field>
  );
}
