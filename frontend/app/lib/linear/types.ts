// The Linear issue shapes the ticket column and its panel draw.
//
// Hand-written here rather than generated from Rust, because the browser is what asks Linear
// for an issue now: it holds a preset's key, posts the query in `queries.ts`, and maps the
// answer itself. Nothing on the server describes this shape any more.
//
// The field names are ours, not Linear's. `id` is the human identifier (`WE-142`) because
// that is what the operator types, drags, and reads on a card; Linear's own UUID rides
// alongside as `uuid` for the mutations that need it.

/** Where a Linear issue sits in its workflow: its state *type*, not the workspace's own
 *  state name, so `Shipped` and `Done` both arrive here as `done`.
 *
 *  All five are modelled even though the ticket column only ever lists two of them. A
 *  sub-issue can be in any state, and drawing a completed one as Todo because the union could
 *  not say otherwise would be worse than carrying three extra variants. */
export type TicketState = "backlog" | "todo" | "in_progress" | "done" | "canceled";

/** A pointer to another issue: this one's parent, or one of its sub-issues. Carries only
 *  what a one-line row draws, because opening it is a click away in Linear. */
export interface TicketLink {
  /** Human identifier, e.g. `WE-143`. */
  id: string;
  title: string;
  url: string;
  state: TicketState;
}

/** One Linear label, with the colour Linear stores for it (`#rrggbb`). The colour is the
 *  operator's own scheme over there, so it travels with the label rather than being mapped
 *  onto a palette of ours. */
export interface TicketLabel {
  name: string;
  color: string;
}

/** A Linear workspace, as its own API key describes it.
 *
 *  One per distinct configured key, so this is the operator's list of workspaces rather than
 *  anything RMNG stores. `urlKey` is what Linear puts in its own URLs (`linear.app/pegasis`),
 *  which is why the home link needs no lookup beyond this. */
export interface LinearWorkspace {
  /** Linear's organization UUID. Two presets holding two keys for one workspace collapse on
   *  this, which no other field can do: the name is editable and the key is not the identity. */
  id: string;
  /** What the workspace calls itself, e.g. `Personal`. */
  name: string;
  /** The URL slug, e.g. `pegasis`. */
  urlKey: string;
}

/** A Linear issue assigned to the owner of one of the configured preset API keys.
 *
 *  Each key is personal and answers only for its own owner, so the browser queries every
 *  configured key and draws the union. Nothing here says which key found a ticket: the
 *  operator holds all of them, and the answer would never change what they do next. */
export interface LinearTicket {
  /** Human identifier, e.g. `WE-142`. Also what the clone dialog parses. */
  id: string;
  title: string;
  /** Canonical Linear URL, which is what a drop hands the clone dialog. */
  url: string;
  state: TicketState;
  /** Linear's own UUID for the issue. A mutation addresses an issue by this and will not
   *  take the identifier. Absent on a ticket nobody fetched from Linear, which is every
   *  fixture. */
  uuid?: string;
  /** Team key, e.g. `WE`. It is what picks the preset, so it is worth showing. */
  team?: string;
  /** Linear's priority, 1 (urgent) to 4 (low). Absent when none is set. */
  priority?: number;
  /** Always present, empty list included: the column maps over it unconditionally, and an
   *  absent array would make every consumer guard a field that is never unknown. */
  labels: TicketLabel[];
  /** Linear's own `branchName`, which is the string its "copy git branch name" produces. */
  branchName?: string;
  /** The issue body, as Linear stores it: markdown. Absent when the issue has none. */
  description?: string;
  /** Display name of whoever it is assigned to. Nothing draws it, so the open-issues query
   *  does not ask for it and no ticket from Linear carries one. */
  assignee?: string;
  /** `YYYY-MM-DD`, as Linear sends it. Not queried, for the same reason as `assignee`. */
  dueDate?: string;
  /** Linear's point estimate, whatever scale the team set. Not queried either. */
  estimate?: number;
  /** The issue this one hangs under, when it is itself a sub-issue. */
  parent?: TicketLink;
  /** Sub-issues, in Linear's order, whatever state they are in. Always present so the panel
   *  can map over it without guarding a field that is never unknown. */
  children: TicketLink[];
}
