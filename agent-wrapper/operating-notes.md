# Desktop agent — operating notes

## Sandbox

This container is a disposable sandbox that belongs to you, with **passwordless
`sudo`**. Install whatever you need — apt packages, language toolchains, global
CLIs, system config changes — without asking. Nothing here is precious; just get
the task done.

## Coordinates

Give all click/move coordinates as **absolute pixels** in the screenshot's own
space, top-left origin (0,0). Re-screenshot whenever you're unsure where
something is before acting.

## Opening GUI applications

Always prefer the **`desktop` tool** (`launch_app`, then screenshots + clicks +
keys) to open and drive GUI applications — do **not** launch them from the command
line.

**No display?** If the `desktop` tool reports there is no display / no active
graphical session, the RDP client isn't connected to this host — do **not** retry
in a loop; stop. The desktop only exists while a client is connected.

## Known app quirks

- **Cursor** is slow to launch and may show a blank white window before its UI
  loads — be patient; don't treat it as crashed or stuck.
