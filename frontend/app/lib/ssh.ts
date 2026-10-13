/** The bastion `sshd` port, hardcoded server-side (`wire::PORT_BASTION`). It is a
 *  constant here rather than a prop because nothing can change it. */
export const BASTION_PORT = 2222;

/** The ssh command that jumps through the control-server bastion to the clone's own sshd,
 *  with `options` (if any) placed before the host-key option. */
function jumpCommand(publicHost: string, bastionPort: number, cloneId: string, options = ""): string {
  return `ssh -J rmng@${publicHost}:${bastionPort} ${options}-o StrictHostKeyChecking=accept-new rmng@${cloneId}`;
}

/** The copy-paste one-liner: inline `-J` jump through the control-server bastion,
 *  terminating at the clone's own sshd. Mirrors the Rust `build_ssh_command`. */
export function buildSshCommand(publicHost: string, bastionPort: number, cloneId: string): string {
  return jumpCommand(publicHost, bastionPort, cloneId);
}

/** The port-forward one-liner (the same jump as `buildSshCommand`): while it runs,
 *  `localhost:<localPort>` on the machine that runs it reaches `<clonePort>` inside the clone.
 *  `-N`: no remote shell, only the forward. */
export function buildForwardCommand(
  publicHost: string,
  bastionPort: number,
  cloneId: string,
  localPort: number,
  clonePort: number,
): string {
  return jumpCommand(publicHost, bastionPort, cloneId, `-N -L ${localPort}:localhost:${clonePort} `);
}
