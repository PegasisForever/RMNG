# RMNG subclones

This fork adds `isolation: "subclone"` to `nicobailon/pi-subagents`.
The upstream base is commit `11931c3` and package version `0.65.1`.
It runs inside an RMNG clone and uses terminal pi.
The fork lives in the RMNG repository at `pi-subagents-rmng/`.

## Installation

Run `scripts/rmng/install.sh` from the fork checkout.
From the RMNG repository root, run `pi-subagents-rmng/scripts/rmng/install.sh`.
The script requires Node.js and npm in the current shell.
It installs pi `0.85.0`, copies the Node.js executable, and registers the local package.
The runtime lives in `.rmng-runtime` inside the checkout.
The `pi` launcher lives at `~/.local/bin/pi`.
Add `~/.local/bin` to the shell's `PATH` if needed.

The RMNG control-server and fleet command must support `clone create --seed`.
That operation copies the selected directories before it reports the clone ready.
The dashboard assistant has its own extension list and does not load this package.

## Delegate a task

```typescript
subagent({
  agent: "worker",
  task: "Read the project and implement the requested fix.",
  cwd: "/home/rmng/project",
  isolation: "subclone",
  context: "fresh",
  async: true
})
```

Each call creates one headless subclone from the parent's source image.
RMNG copies the project, plugin runtime, and user agent and skill directories into the same paths.
The project copy includes git metadata, uncommitted files, dependencies, and symbolic links.
RMNG assigns the child its own credentials through the parent's account selection.
The copy reads live files and does not freeze the parent.

A systemd user service runs the remote host and restarts it after failure.
The remote host calls the upstream plugin's existing child runner.
The parent shows remote runs in the upstream fleet display and sends completion messages.
Use `async: false` when the parent must wait for completion.

## Control a subclone

Use the returned `rmng-subagent-*` clone identifier as `id`.

```typescript
subagent({ action: "status", id: "rmng-subagent-..." })
subagent({ action: "steer", id: "rmng-subagent-...", message: "Also check the error path." })
subagent({ action: "stop", id: "rmng-subagent-..." })
subagent({ action: "resume", id: "rmng-subagent-...", message: "Continue with the next change." })
subagent({ action: "close", id: "rmng-subagent-..." })
```

`interrupt` pauses the child through the upstream control path.
`stop` stops the child and keeps its clone and files.
`resume` continues the saved child session in the same clone.
`close` deletes the clone and its files.
Push or copy wanted changes before closing it.
Any pi session with the local control record can issue these controls.
Resuming a child routes later completion messages to the requesting pi session.

The parent stores control records and copied result records under `~/.pi/rmng-subclones`.
The remote host keeps sessions and upstream artifacts under `~/.pi/rmng-host`.
Reopening the same parent pi session restores its recorded runs.
The remote host saves completed output separately from upstream result cleanup.
The plugin retains failed clones for inspection.
After three failed background status requests, the parent reports the clone as unreachable and stops waiting.
A later `status` request can recover the connection.
The unreachable state does not prove that the child stopped.

## Clone connections

The plugin assumes that clone connections are private and all clones are trusted.
The remote host accepts control requests without authentication.
Control records and host configuration use normal filesystem permissions.
Model providers still require their own credentials.

## Tests

Run `npm run test:rmng` from `pi-subagents-rmng/` to check the RMNG extension.
Run `npm run test:all` to check the upstream unit and integration suites.

## Scope

Calls without subclone isolation retain upstream behavior.
Subclone calls require one named agent and a project directory below `/home/rmng`.
Use separate calls for parallel subclones.
Workflow scripts do not accept subclone isolation in this version.
The remote child starts with fresh context, so include relevant context in its task.
The plugin does not merge child changes into the parent.
