// The clone dialog's No-ticket tab: a clone with no Linear ticket behind it. Nothing here
// touches Linear, so this is the one tab that never needs an API key.
//
// It is also the one tab that picks a preset by hand, because there is no ticket id and no
// team key for one to be derived from.
import { DropdownSelect } from "~/components/DropdownSelect";
import {
  cloneRow,
  cloneRowField,
  cloneRowLabel,
  cloneRowLabelTop,
  cloneRowTop,
} from "~/components/cloneFieldStyles";
import type { PresetRedacted } from "~/lib/wire/PresetRedacted";

export function ClonePlainFields({
  title,
  message,
  presets,
  preset,
  onTitleChange,
  onMessageChange,
  onPresetChange,
  onSubmit,
}: {
  title: string;
  /** Sent to the agent as its first turn. Empty means the clone starts idle. */
  message: string;
  /** Every configured preset. Empty hides the control entirely — there is nothing to pick
   *  and the server falls back on its own. */
  presets: PresetRedacted[];
  /** The picked preset's name. */
  preset: string;
  onTitleChange: (title: string) => void;
  onMessageChange: (message: string) => void;
  onPresetChange: (name: string) => void;
  /** Enter in the title field starts the clone. */
  onSubmit: () => void;
}) {
  return (
    <div className="space-y-3">
      <label className={cloneRow}>
        <span className={cloneRowLabel}>Title</span>
        <input
          autoFocus
          value={title}
          onChange={(e) => onTitleChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSubmit();
          }}
          placeholder="Container title"
          className={cloneRowField}
        />
      </label>
      <label className={cloneRowTop}>
        <span className={cloneRowLabelTop}>First message to the agent</span>
        <textarea
          value={message}
          onChange={(e) => onMessageChange(e.target.value)}
          rows={3}
          placeholder="Optional — leave empty to not auto-send a first message"
          className={`resize-y ${cloneRowField}`}
        />
      </label>
      {presets.length > 0 ? (
        <label className={cloneRow}>
          <span className={cloneRowLabel}>Preset</span>
          <DropdownSelect
            rows={presets.map((p) => ({
              value: p.name,
              label:
                p.name +
                (p.labels.length > 0 ? ` · ${p.labels.join(", ")}` : ""),
            }))}
            value={preset}
            onChange={onPresetChange}
            label="Preset"
            className={cloneRowField}
          />
        </label>
      ) : null}
    </div>
  );
}
