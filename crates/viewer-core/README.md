# viewer-core

The toolkit-free pieces every RMNG viewer front-end shares. Nothing here touches a window, a
platform framework, or a socket beyond plain `std::net`, so both the GTK viewer
([`crates/viewer`](../viewer/README.md)) and the native macOS viewer
([`crates/viewer-macos`](../viewer-macos/README.md)) build on it.

| Module | What |
|---|---|
| `config` | the persisted viewer config (`~/.config/rmng-viewer/config.json`): server address, `cmd_is_ctrl` |
| `auto_lock` | the debounced auto pointer-lock policy (engage on a sustained hidden remote cursor, release on a sustained visible one) + the reconciler |
| `forward` | local port-forward listeners driven by the server's rule set, reporting status back |
| `kvk_evdev` | the Carbon virtual-key → Linux evdev keycode table (macOS keyboards) |
| `terminal` | the terminal colour scheme and the escape-sequence encoders — which colour a cell resolves to and which bytes a key or click puts on the wire must not differ between the two front-ends |
