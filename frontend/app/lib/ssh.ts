/** The copy-paste one-liner: inline `-J` jump through the control-server bastion,
 *  terminating at the clone's own sshd. Mirrors the Rust `build_ssh_command`. */
export function buildSshCommand(publicHost: string, bastionPort: number, cloneId: string): string {
  return `ssh -J rmng@${publicHost}:${bastionPort} -o StrictHostKeyChecking=accept-new rmng@${cloneId}`;
}
