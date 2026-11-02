// The settings panel's shared field furniture: the input class every control uses, the
// section wrapper, a labelled field, and the write-only secret.
//
// Every section below imports from here rather than restating the class strings, so one
// control cannot drift away from the rest of the panel. The looks are the control panel's flat
// content (`~/desk/control/ui.tsx`), so they are right with no `.on-glass` round them.
import { BUTTON, DANGER_TEXT, FIELD, LINE, TEXT, TEXT_2, TEXT_3 } from "~/desk/control/ui";

/** A field's look (`FIELD`, `~/desk/GLASS.md`), full width, with a grey hint. Size is the caller's. */
const FIELD_LOOK = `w-full ${FIELD} px-2 py-1 placeholder:text-black/40 dark:placeholder:text-white/40`;
/** The class every plain settings input carries. */
export const settingsInput = `${FIELD_LOOK} text-sm`;
/** The same in small text (a preset's fields). */
export const settingsInputSmall = `${FIELD_LOOK} text-xs`;
/** A field of code or of a prompt: small, monospaced. */
export const settingsCode = `${FIELD_LOOK} font-mono text-xs`;
/** A small button in a section (Add, Check, Test). */
export const settingsButton = `${BUTTON} px-2.5 py-1.5 text-xs`;
/** A Remove button: red text on no fill (`~/desk/GLASS.md`: danger is never a red surface). */
export const settingsRemoveButton = `shrink-0 rounded-md px-2 py-1 text-xs ${DANGER_TEXT} hover:bg-black/5 active:bg-black/10 dark:hover:bg-white/10 dark:active:bg-white/15`;

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
    <section className={`border-t pt-4 ${LINE}`}>
      <h3 className={`text-sm font-semibold ${TEXT}`}>{title}</h3>
      {hint ? <p className={`mb-2 mt-0.5 text-xs ${TEXT_3}`}>{hint}</p> : <div className="mb-2" />}
      {children}
    </section>
  );
}

export function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className={`mb-0.5 block text-xs font-medium ${TEXT_2}`}>{label}</span>
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
          className={`shrink-0 rounded-md px-1.5 py-0.5 text-[10px] font-semibold ${
            set ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300" : `bg-black/5 dark:bg-white/10 ${TEXT_3}`
          }`}
        >
          {set ? "set" : "unset"}
        </span>
      </div>
    </Field>
  );
}
