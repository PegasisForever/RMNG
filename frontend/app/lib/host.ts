// The window that owns the browser's features for this page: the /ng shell
// (`public/ng-shell.html`) when this page runs in its frame, else this page's own window. The
// fullscreen and the key lock must be the shell's, so that a reload of the app (a new build)
// does not end them; so must the window's name, address and title, which belong to the browser
// window and not to the frame. The shell's own API (`rmngShell`) is only what the app cannot read
// from its window directly.

type Shell = { version: number; insetBottom(): number };

/** The shell's window, when this page is in its frame. */
export function shellWindow(): (Window & { rmngShell: Shell }) | null {
  if (typeof window === "undefined" || window.parent === window) return null;
  try {
    const p = window.parent as Window & { rmngShell?: Shell };
    return p.rmngShell ? (p as Window & { rmngShell: Shell }) : null;
  } catch {
    // Another origin's frame: not the shell
    return null;
  }
}

/** The browser window: the shell's, or this page's own. */
export function hostWindow(): Window {
  return shellWindow() ?? window;
}
