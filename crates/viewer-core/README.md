# viewer-core

The toolkit-free pieces every RMNG viewer front-end shares. Nothing here touches a window, a
platform framework, or a socket beyond plain `std::net`, so both the GTK viewer
([`crates/viewer`](../viewer/README.md)) and the native macOS viewer
([`crates/viewer-macos`](../viewer-macos/README.md)) build on it.

| Module | What |
|---|---|
| `config` | the persisted viewer config (`~/.config/rmng-viewer/config.json`): server address, `cmd_is_ctrl` |
| `auto_lock` | the debounced auto pointer-lock policy (engage on a sustained hidden remote cursor, release on a sustained visible one) + the reconciler |
| `outbound` | bounded background socket writer, input priority, motion coalescing, and connection cancellation |
| `forward` | local port-forward listeners driven by the server's rule set, reporting status back |
| `kvk_evdev` | the Carbon virtual-key → Linux evdev keycode table (macOS keyboards) |
| `terminal` | the terminal colour scheme and the escape-sequence encoders — which colour a cell resolves to and which bytes a key or click puts on the wire must not differ between the two front-ends |

## Outgoing writes

Outgoing socket writes run on a dedicated thread per connection. UI and pointer-lock threads only enqueue messages;
a stalled write does not hold the queue mutex or stop the UI. The queue bounds both message count
(1024) and bytes (32 MiB plus 64 KiB reserved headroom), with one additional message in flight.
Adjacent absolute motion on the same monitor keeps the latest position; adjacent relative motion
adds its deltas. Keys, buttons, scroll events, and monitor changes preserve motion boundaries.
Input can pass queued clipboard data, while clipboard offers/requests and ordered controls keep
their position. An in-flight clipboard frame must finish before input can use the same socket.

Disconnecting, changing servers, or dropping the writer cancels socket I/O and discards that
connection's queue. A write error or queue overflow closes the connection so the reader reconnects;
pending input is never replayed on a new connection. The outgoing queue tests include a
deterministically stalled transport, wire ordering, bounded overload, and real TCP cancellation:

```sh
cargo test -p viewer-core outbound
```
