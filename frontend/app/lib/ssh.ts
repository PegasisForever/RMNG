/** The bastion `sshd` port, hardcoded server-side (`wire::PORT_BASTION`). It is a
 *  constant here rather than a prop because nothing can change it. */
export const BASTION_PORT = 2222;

/** The copy-paste one-liner: inline `-J` jump through the control-server bastion,
 *  terminating at the clone's own sshd. Mirrors the Rust `build_ssh_command`. */
export function buildSshCommand(publicHost: string, bastionPort: number, cloneId: string): string {
  return `ssh -J rmng@${publicHost}:${bastionPort} -o StrictHostKeyChecking=accept-new rmng@${cloneId}`;
}
