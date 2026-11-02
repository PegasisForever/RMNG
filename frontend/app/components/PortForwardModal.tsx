// Port forwarding for a clone: the operator types a local port and a clone port and copies an
// `ssh` command (through the bastion) that forwards one to the other while it runs. RMNG stores
// nothing; the command is the whole feature.
import { useState } from "react";

import { ModalShell } from "~/components/ModalShell";
import { BOX, BUTTON, DONE_BUTTON, FIELD, LINE, R_PIECE, TEXT, TEXT_2, TEXT_3 } from "~/desk/control/ui";
import type { Clone } from "~/lib/types";
import { BASTION_PORT, buildForwardCommand } from "~/lib/ssh";

/** A TCP port typed in a box, or null when it is not one. */
function port(s: string): number | null {
  const n = Number(s);
  return /^\d+$/.test(s.trim()) && n >= 1 && n <= 65535 ? n : null;
}

export function PortForwardModal({
  clone,
  sshPublicHost,
  onClose,
  onCopy,
}: {
  clone: Clone;
  /** The `-J` jump host: this page's own address. */
  sshPublicHost: string;
  /** The dialog is finished: unmount it. The exit frames have already played. */
  onClose: () => void;
  /** Put the command on the clipboard; resolves to whether it landed. */
  onCopy: (command: string) => Promise<boolean>;
}) {
  const [clonePort, setClonePort] = useState("");
  const [localPort, setLocalPort] = useState("");
  const [copied, setCopied] = useState<boolean | null>(null);
  const remote = port(clonePort);
  // The local port is the clone port unless the operator typed another.
  const local = localPort.trim() ? port(localPort) : remote;
  const command =
    remote !== null && local !== null
      ? buildForwardCommand(sshPublicHost, BASTION_PORT, clone.id, local, remote)
      : null;

  const box = `min-w-0 ${FIELD} px-2.5 py-1.5 text-sm placeholder:text-black/40 dark:placeholder:text-white/40`;
  const caption = `flex flex-col gap-1 text-[11px] font-medium uppercase tracking-wide ${TEXT_3}`;
  return (
    <ModalShell size="md" onExited={onClose}>
      {(close) => (
        <>
          <h3 className={`text-[15px] font-semibold tracking-tight ${TEXT}`}>
            Port forward ·{" "}
            <span className={TEXT_2}>{clone.displayName ?? clone.id}</span>
          </h3>
          <p className={`mt-1 text-xs ${TEXT_2}`}>
            Run the command on your machine. While it runs, <code>localhost:&lt;local&gt;</code>{" "}
            there reaches the port inside this clone.
          </p>

          <div className="mt-4 grid grid-cols-2 gap-2">
            <label className={caption}>
              Clone port
              <input
                autoFocus
                inputMode="numeric"
                value={clonePort}
                onChange={(e) => {
                  setClonePort(e.target.value);
                  setCopied(null);
                }}
                placeholder="3000"
                className={box}
              />
            </label>
            <label className={caption}>
              Local port
              <input
                inputMode="numeric"
                value={localPort}
                onChange={(e) => {
                  setLocalPort(e.target.value);
                  setCopied(null);
                }}
                placeholder={remote !== null ? String(remote) : "same as clone port"}
                className={box}
              />
            </label>
          </div>

          <pre className={`mt-3 min-h-[3.5rem] whitespace-pre-wrap break-all border px-2.5 py-1.5 font-mono text-xs select-all ${R_PIECE} ${LINE} ${BOX} ${command ? TEXT : TEXT_3}`}>
            {command ?? "Type a port from 1 to 65535."}
          </pre>

          <div className="mt-5 flex justify-end gap-2">
            <button
              type="button"
              onClick={() => close()}
              className={`${BUTTON} px-3 py-1.5 text-sm`}
            >
              Close
            </button>
            <button
              type="button"
              disabled={!command}
              onClick={() => command && void onCopy(command).then(setCopied)}
              className={`${DONE_BUTTON} px-3 py-1.5 text-sm font-medium`}
            >
              {copied === true ? "Copied!" : copied === false ? "Copy failed — copy manually" : "Copy command"}
            </button>
          </div>
        </>
      )}
    </ModalShell>
  );
}
