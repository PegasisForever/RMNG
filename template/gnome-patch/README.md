# RMNG GNOME host patch

RMNG does **not** use gnome-remote-desktop (no RDP): the `clone-daemon` talks
straight to Mutter's private D-Bus APIs and does its own GStreamer VA-API encoding.
So of the old client's host patches (the `grd-*` gnome-remote-desktop patches and a
prior shell-02, both obsolete — rmng bypasses g-r-d entirely), only two are still
relevant — and both patch **gnome-shell**:

| Patch | Why RMNG needs it |
| ------- | ----------------------- |
| `shell-01-hide-screen-sharing-indicator` | The clone's Mutter RemoteDesktop session is a remote-access handle → gnome-shell paints the orange "being watched" pill, which gets composited into the captured frames the viewer shows. This hides it. |
| `shell-02-enable-eval` | Allows `org.gnome.Shell.Eval` without `unsafe_mode`. `clone-daemon`'s window-management MCP tools (list/move/launch windows, `crates/clone-daemon/src/windows.rs`) drive gnome-shell through `Eval`. Without it they return an "unsafe_mode off" error. |

> **Security:** shell-02 lets anything on the session bus run code inside gnome-shell.
> Acceptable on a locked-down headless automation clone; do not apply on a shared desktop.

## How it ships

`build-shell-deb.sh` runs in the **`gnome-build` stage of `template/Dockerfile`**
(`docker build`) and produces a patched `gnome-shell_<ver>+ngshell1_amd64.deb`. It rebuilds
only `libshell-<N>.so` (both patches are JS compiled into the gresource baked into that one
library) and swaps it into the stock gnome-shell `.deb`, bumping the version so it installs
cleanly over stock. It prints the produced path as `DEB=<path>`.

`template/Dockerfile` copies that deb to `/tmp/gnome-shell.deb` in its final stage, where
`template/setup/15-gnome-patch.sh` `dpkg -i`s it over the stock shell **during the template
build** — every clone created from the published template inherits the patched shell. The deb
is not a control-server payload (the retired in-product bootstrap was its only consumer): a
missing/failed install fails the template build rather than publishing a degraded template.

Build standalone (needs the gnome-shell build-deps, i.e. `apt build-dep gnome-shell`):

```sh
bash template/gnome-patch/build-shell-deb.sh   # prints DEB=<path>; FORCE=1 to rebuild
```

Verify on a host with the deb installed (needs a freshly started shell):

```sh
gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
  --method org.gnome.Shell.Eval "1+1"      # patched: (true, '2')   stock: (false, '')
```
