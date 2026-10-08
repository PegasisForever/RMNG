# Implementing a ticket

When the message has a Linear ticket link
(`https://linear.app/<workspace>/issue/<PREFIX>-<n>/…`), the **PREFIX** selects the flow:
**`per`** is a personal task that you give to **Claude Code in a terminal**; **`we`**,
**`dev`**, and **`hh`** are coding tickets that you give to **Claude Code in Cursor**. Do the
steps in order, and stop when the task is handed off.

## `per`: Claude Code in a terminal

`per` is a personal task, not coding: there is no repo. You give the task to Claude Code; you
do not do it yourself.

1. Start a terminal. Type `claude --dangerously-skip-permissions` and press `Return`.
2. When Claude Code has loaded, send one short, single-line prompt with the ticket link, for
   example: `Do what this Linear ticket requires: <ticket link>. Don't commit, push, or reply
   to Linear unless explicitly told to.`

## `we` / `dev` / `hh`: Claude Code in Cursor

Project folder: `we` → `~/Projects/stack`, `dev` → `~/Projects/Dev`, `hh` → `~/Projects/hyperhost`.

1. In a terminal, type `cursor <folder>` and press `Return`. Maximize Cursor when it shows.
2. Press `ctrl+shift+q` to open the Claude Code side panel, and wait until its input box
   shows. If Cursor's own agent opens instead, press it again.
3. Send one single-line prompt: pull the latest commits, switch to the branch Linear gives
   for the ticket, read the repository guidance, implement the ticket, and do not commit,
   push, or reply to Linear unless told to.
4. Open the ticket link in Firefox. If there is a second monitor, move Firefox there.

## Known app quirks

- **Cursor** is slow to start and can show a blank white window first. Wait for it.
