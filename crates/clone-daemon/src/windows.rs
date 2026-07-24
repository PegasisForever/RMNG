//! Window-management MCP tools via gnome-shell `org.gnome.Shell.Eval`.
//!
//! Mutter's remote-desktop / screen-cast APIs inject input + capture frames but
//! can't enumerate or move windows — that lives inside the compositor. We reach it
//! by running JavaScript in gnome-shell via its `Eval(s) -> (b, s)` D-Bus method.
//! `Eval` is gated behind `unsafe_mode` (off since GNOME 41), so this needs the
//! `shell-03-enable-eval` patch (already on the template). Ported from
//! `../../computer-use/src/windows.rs`.

use serde_json::{Value, json};
use zbus::proxy;

#[proxy(
    interface = "org.gnome.Shell",
    default_service = "org.gnome.Shell",
    default_path = "/org/gnome/Shell"
)]
trait ShellEval {
    fn eval(&self, script: &str) -> zbus::Result<(bool, String)>;
}

/// MCP tool defs appended to the daemon's `tools/list`.
pub fn tools() -> Vec<Value> {
    let obj = |props: Value, required: Value| {
        json!({ "type": "object", "properties": props, "required": required })
    };
    vec![
        json!({ "name": "list_windows", "description": "List open windows (id, title, wm_class, monitor, geometry, state)", "inputSchema": obj(json!({}), json!([])) }),
        json!({ "name": "move_window", "description": "Tile a window: mode \"maximize\" (default) or \"center-half\", optionally onto monitor index", "inputSchema": obj(json!({ "id": { "type": "integer" }, "monitor": { "type": "integer" }, "mode": { "type": "string", "enum": ["maximize", "center-half"] } }), json!(["id"])) }),
    ]
}

/// Dispatch a window-management tool. Returns an MCP `content` array (a single text
/// item carrying the JSON result from gnome-shell).
pub async fn call(conn: &zbus::Connection, name: &str, args: &Value) -> Result<Value, String> {
    let script = match name {
        "list_windows" => js_list_windows(),
        "move_window" => {
            let id = args.get("id").and_then(Value::as_u64).ok_or("id required")?;
            let monitor = args.get("monitor").and_then(Value::as_i64).unwrap_or(-1) as i32;
            let mode = args.get("mode").and_then(Value::as_str).unwrap_or("maximize");
            if mode != "maximize" && mode != "center-half" {
                return Err(format!("unknown mode {mode:?} (want \"maximize\" or \"center-half\")"));
            }
            js_move_resize(id, monitor, mode)
        }
        other => return Err(format!("unknown window tool '{other}'")),
    };
    let json = eval(conn, &script).await?;
    Ok(json!([{ "type": "text", "text": json }]))
}

/// Run a JS snippet via `Eval`; return the JSON result string, or a clear error for
/// the three gnome-shell failure shapes ((false,"")=gated off, (false,err)=threw).
async fn eval(conn: &zbus::Connection, script: &str) -> Result<String, String> {
    let proxy = ShellEvalProxy::new(conn).await.map_err(|e| e.to_string())?;
    let (success, result) = proxy.eval(script).await.map_err(|e| {
        let s = e.to_string();
        if s.contains("ServiceUnknown") || s.contains("NameHasNoOwner") {
            format!("gnome-shell is not on the session bus (no active GNOME session yet): {s}")
        } else {
            format!("org.gnome.Shell.Eval failed: {s}")
        }
    })?;
    if !success {
        if result.is_empty() {
            return Err("org.gnome.Shell.Eval is disabled (gnome-shell unsafe_mode off) — needs the \
                        shell-03-enable-eval patch on the template"
                .into());
        }
        return Err(format!("gnome-shell Eval script error: {result}"));
    }
    Ok(result)
}

// --- injected JavaScript (touches only the `global` singleton; Meta enums inlined) ---

const JS_WINDOW_ACTORS: &str = "(global.compositor && global.compositor.get_window_actors \
      ? global.compositor.get_window_actors() : global.get_window_actors())";

fn js_list_windows() -> String {
    format!(
        "(() => {{ \
          const out = []; \
          const primary = global.display.get_primary_monitor(); \
          for (const a of {actors}) {{ \
            const w = a.meta_window; \
            if (!w || w.is_override_redirect()) continue; \
            const t = w.get_window_type(); \
            if (t !== 0 && t !== 3 && t !== 4 && t !== 7) continue; \
            const r = w.get_frame_rect(); \
            out.push({{ \
              id: w.get_id(), title: w.get_title() || '', wm_class: w.get_wm_class() || '', \
              monitor: w.get_monitor(), on_primary: w.get_monitor() === primary, \
              workspace: w.get_workspace() ? w.get_workspace().index() : -1, \
              maximized: w.maximized_horizontally && w.maximized_vertically, \
              minimized: w.minimized, fullscreen: w.is_fullscreen(), focus: w.has_focus(), \
              frame: {{ x: r.x, y: r.y, width: r.width, height: r.height }}, \
            }}); \
          }} \
          return out; \
        }})()",
        actors = JS_WINDOW_ACTORS
    )
}

fn js_move_resize(id: u64, monitor: i32, mode: &str) -> String {
    format!(
        "(() => {{ \
          const id = {id}; const monitor = {monitor}; const mode = '{mode}'; \
          let win = null; \
          for (const a of {actors}) {{ const w = a.meta_window; if (w && Number(w.get_id()) === id) {{ win = w; break; }} }} \
          if (!win) throw new Error('no window with id ' + id); \
          const n = global.display.get_n_monitors(); \
          const target = monitor >= 0 ? monitor : win.get_monitor(); \
          if (target < 0 || target >= n) throw new Error('monitor index ' + target + ' out of range (have ' + n + ')'); \
          if (win.minimized) win.unminimize(); \
          if (win.is_fullscreen()) win.unmake_fullscreen(); \
          win.unmaximize(3); \
          if (target !== win.get_monitor()) win.move_to_monitor(target); \
          if (mode === 'maximize') {{ win.maximize(3); }} \
          else if (mode === 'center-half') {{ \
            const wa = global.workspace_manager.get_active_workspace().get_work_area_for_monitor(target); \
            const geo = global.display.get_monitor_geometry(target); \
            const big = geo.width > 1920 || geo.height > 1080; \
            const width = Math.min(big ? 1920 : 1280, wa.width); \
            const height = Math.min(big ? 1080 : 720, wa.height); \
            const x = wa.x + Math.floor((wa.width - width) / 2); \
            const y = wa.y + Math.floor((wa.height - height) / 2); \
            win.move_resize_frame(true, x, y, width, height); \
          }} else {{ throw new Error('unknown mode ' + mode); }} \
          win.activate(global.get_current_time()); \
          return {{ id: win.get_id(), monitor: win.get_monitor() }}; \
        }})()",
        actors = JS_WINDOW_ACTORS
    )
}

