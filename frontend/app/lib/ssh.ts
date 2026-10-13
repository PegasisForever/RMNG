/** The bastion `sshd` port, hardcoded server-side (`wire::PORT_BASTION`). It is a
 *  constant here rather than a prop because nothing can change it. */
export const BASTION_PORT = 2222;

/** The copy-paste one-liner: inline `-J` jump through the control-server bastion,
 *  terminating at the clone's own sshd. Mirrors the Rust `build_ssh_command`. */
export function buildSshCommand(publicHost: string, bastionPort: number, cloneId: string): string {
  return `ssh -J rmng@${publicHost}:${bastionPort} -o StrictHostKeyChecking=accept-new rmng@${cloneId}`;
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
  return `ssh -J rmng@${publicHost}:${bastionPort} -N -L ${localPort}:localhost:${clonePort} -o StrictHostKeyChecking=accept-new rmng@${cloneId}`;
}
