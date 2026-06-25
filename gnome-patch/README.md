# RMNG GNOME host patch

RMNG does **not** use gnome-remote-desktop (no RDP): the `clone-daemon` talks
straight to Mutter's private D-Bus APIs and does its own GStreamer VA-API encoding.
So of the old client's host patches (see `../../gnome-patch/` for the full legacy set
incl. the `grd-*` gnome-remote-desktop patches and shell-02), only two are still
relevant — and both patch **gnome-shell**:

| Patch | Why RMNG needs it |
|-------|-----------------------|
| `shell-01-hide-screen-sharing-indicator` | The clone's Mutter RemoteDesktop session is a remote-access handle → gnome-shell paints the orange "being watched" pill, which gets composited into the captured frames the viewer shows. This hides it. |
| `shell-03-enable-eval` | Allows `org.gnome.Shell.Eval` without `unsafe_mode`. `clone-daemon`'s window-management MCP tools (list/move/launch windows, `crates/clone-daemon/src/windows.rs`) drive gnome-shell through `Eval`. Without it they return an "unsafe_mode off" error. |

> **Security:** shell-03 lets anything on the session bus run code inside gnome-shell.
> Acceptable on a locked-down headless automation clone; do not apply on a shared desktop.

## How it ships

`build-shell-deb.sh` runs **in the build CT** and produces a patched
`gnome-shell_<ver>+ngshell1_amd64.deb`. It rebuilds only `libshell-<N>.so` (both
patches are JS compiled into the gresource baked into that one library) and swaps it
into the stock gnome-shell `.deb`, bumping the version so it installs cleanly over
stock.

`scripts/cs-build-ct.sh` calls this and gzips the result into
`crates/control-server/embedded-bin/gnome-shell-deb.gz`, so the **control-server is a
single self-contained artifact** carrying the patched shell. At template-bootstrap
time the control-server stages the deb on the Proxmox node (`orchestrate.rs` →
`bootstrap.sh`), which `pct push`es it into the new CT, and `provision-clone.sh`
installs it on top of the stock shell. CoW clones inherit the patched shell from the
template automatically.

Build standalone (in the build CT):

```sh
bash gnome-patch/build-shell-deb.sh   # prints DEB=<path>; FORCE=1 to rebuild
```

Verify on a host with the deb installed (needs a freshly started shell):

```sh
gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
  --method org.gnome.Shell.Eval "1+1"      # patched: (true, '2')   stock: (false, '')
```
