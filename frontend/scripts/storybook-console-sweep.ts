/**
 * Load every story in a running Storybook and report the console errors it produces.
 *
 * Two surfaces, because they are two different JavaScript realms and a clean bill of
 * health on one says nothing about the other:
 *
 *   preview  loads `iframe.html?viewMode=story&id=<id>` — the story's own realm.
 *   manager  loads `/?path=/story/<id>` — the Storybook UI itself, where the sidebar,
 *            the toolbar and the Controls panel run. Args cross the channel into this
 *            realm, so a value a story renders happily can still break the Controls
 *            panel here.
 *
 * The manager pass waits for the Controls panel to finish rendering and records what it
 * settled on, so "the panel showed its error boundary" is a reported outcome and not a
 * silent pass.
 *
 * Usage:
 *   bun run storybook:sweep -- [--surface preview|manager|both]
 *                              [--url http://localhost:6006]
 *                              [--concurrency 4] [--filter <substring>] [--json <path>]
 *
 * Exits non-zero when any story produced a console error.
 */

import { launch, type Browser, type Cdp, type Page } from "./cdp";

const CONTROLS_PANEL_ID = "addon-controls";
const PANEL_ROOT_ID = "storybook-panel-root";
const ADDON_ERROR_TITLE = "This addon has errors";
/** A story that has not settled by now is reported as a timeout rather than waited on. */
const STORY_BUDGET_MS = 45_000;

type Surface = "preview" | "manager";

interface Options {
  baseUrl: string;
  surfaces: Surface[];
  concurrency: number;
  filter: string;
  jsonPath: string;
}

interface ConsoleHit {
  kind: "exception" | "console.error" | "log.error";
  text: string;
}

/** What the Controls panel settled on. Only meaningful for the manager surface. */
type PanelOutcome = "controls" | "no-controls" | "addon-error" | "timeout" | "n/a";

interface StoryResult {
  id: string;
  surface: Surface;
  hits: ConsoleHit[];
  panel: PanelOutcome;
  controlRows: string[];
}

function parseArgs(argv: string[]): Options {
  const opts: Options = {
    baseUrl: "http://localhost:6006",
    surfaces: ["preview", "manager"],
    concurrency: 4,
    filter: "",
    jsonPath: "",
  };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const value = argv[i + 1] ?? "";
    if (flag === "--url") (opts.baseUrl = value.replace(/\/$/, "")), (i += 1);
    else if (flag === "--concurrency") (opts.concurrency = Number(value)), (i += 1);
    else if (flag === "--filter") (opts.filter = value), (i += 1);
    else if (flag === "--json") (opts.jsonPath = value), (i += 1);
    else if (flag === "--surface") {
      opts.surfaces = value === "both" ? ["preview", "manager"] : [value as Surface];
      i += 1;
    }
  }
  return opts;
}

/** Storybook's story index. Docs entries have no args table, so only stories are swept. */
async function fetchStoryIds(baseUrl: string, filter: string): Promise<string[]> {
  const res = await fetch(`${baseUrl}/index.json`);
  if (!res.ok) throw new Error(`GET ${baseUrl}/index.json returned ${res.status}`);
  const body = (await res.json()) as {
    entries?: Record<string, { id: string; type?: string }>;
  };
  return Object.values(body.entries ?? {})
    .filter((e) => (e.type ?? "story") === "story")
    .map((e) => e.id)
    .filter((id) => !filter || id.includes(filter))
    .sort();
}

function storyUrl(baseUrl: string, surface: Surface, id: string): string {
  if (surface === "preview") {
    return `${baseUrl}/iframe.html?viewMode=story&id=${encodeURIComponent(id)}`;
  }
  // `addonPanel` pins the Controls panel as the selected one, so every story exercises
  // it rather than whatever the previous story left selected.
  return `${baseUrl}/?path=/story/${encodeURIComponent(id)}&addonPanel=${CONTROLS_PANEL_ID}`;
}

/** Flatten a CDP RemoteObject list into one readable line. */
function describeConsoleArgs(args: unknown[]): string {
  return args
    .map((raw) => {
      const arg = raw as { value?: unknown; description?: string };
      if (typeof arg.description === "string") return arg.description;
      if (arg.value !== undefined) return String(arg.value);
      return "";
    })
    .filter(Boolean)
    .join(" ");
}

/**
 * Watch one tab's console. Chrome reports the same failure through different channels
 * depending on where it came from, so all three are collected and tagged:
 *   `exception`     an uncaught throw or an unhandled rejection
 *   `console.error` an explicit console.error call (React logs render failures here)
 *   `log.error`     the browser's own log, which is where failed network requests land
 */
function watchConsole(cdp: Cdp, sink: ConsoleHit[]): () => void {
  return cdp.on((method, params) => {
    if (method === "Runtime.exceptionThrown") {
      const detail = (params as { exceptionDetails?: Record<string, unknown> }).exceptionDetails;
      const thrown = detail?.exception as { description?: string; value?: unknown } | undefined;
      const text =
        thrown?.description ?? (thrown?.value !== undefined ? String(thrown.value) : "");
      sink.push({ kind: "exception", text: text || String(detail?.text ?? "unknown throw") });
    } else if (method === "Runtime.consoleAPICalled") {
      const call = params as { type?: string; args?: unknown[] };
      if (call.type !== "error") return;
      sink.push({ kind: "console.error", text: describeConsoleArgs(call.args ?? []) });
    } else if (method === "Log.entryAdded") {
      const entry = (params as { entry?: { level?: string; text?: string; url?: string } }).entry;
      if (entry?.level !== "error") return;
      sink.push({ kind: "log.error", text: `${entry.text ?? ""} ${entry.url ?? ""}`.trim() });
    }
  });
}

/** Injected into the page to ask what the Controls panel currently shows. */
const PANEL_PROBE = `(() => {
  const root = document.getElementById(${JSON.stringify(PANEL_ROOT_ID)});
  if (!root) return { state: "pending", rows: [] };
  const text = root.innerText || "";
  if (text.includes(${JSON.stringify(ADDON_ERROR_TITLE)})) return { state: "addon-error", rows: [] };
  const table = root.querySelector("table");
  if (table) {
    // Only the header row uses <th>; every arg row names itself in its first <td>.
    const rows = Array.from(table.querySelectorAll("tbody tr"))
      .map((tr) => { const cell = tr.querySelector("th, td"); return cell ? cell.innerText : ""; })
      .map((s) => s.split("\\n")[0].trim())
      .filter(Boolean);
    if (rows.length) return { state: "controls", rows };
  }
  if (/No inputs found|setting up controls/i.test(text)) return { state: "no-controls", rows: [] };
  return { state: "pending", rows: [] };
})()`;

/** True once the preview iframe has mounted a story root with content in it. */
const PREVIEW_PROBE = `(() => {
  const root = document.getElementById("storybook-root");
  return !!root && root.childElementCount > 0;
})()`;

interface ProbeResult {
  state: "pending" | "controls" | "no-controls" | "addon-error";
  rows: string[];
}

async function evaluate<T>(cdp: Cdp, expression: string): Promise<T | undefined> {
  const res = (await cdp.send("Runtime.evaluate", { expression, returnByValue: true })) as {
    result?: { value?: T };
  };
  return res.result?.value;
}

async function visitStory(
  cdp: Cdp,
  opts: Options,
  surface: Surface,
  id: string,
): Promise<StoryResult> {
  const hits: ConsoleHit[] = [];
  const stop = watchConsole(cdp, hits);
  let panel: PanelOutcome = surface === "manager" ? "timeout" : "n/a";
  let controlRows: string[] = [];

  try {
    await cdp.send("Page.navigate", { url: storyUrl(opts.baseUrl, surface, id) });
    const deadline = Date.now() + STORY_BUDGET_MS;
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 150));
      if (surface === "manager") {
        const probe = await evaluate<ProbeResult>(cdp, PANEL_PROBE);
        if (probe && probe.state !== "pending") {
          panel = probe.state;
          controlRows = probe.rows;
          break;
        }
      } else if ((await evaluate<boolean>(cdp, PREVIEW_PROBE)) === true) {
        break;
      }
    }
    // React and the channel both flush a tick after the DOM settles, so give late errors
    // a window to arrive before the listener comes off.
    await new Promise((r) => setTimeout(r, 400));
  } catch (err) {
    hits.push({
      kind: "exception",
      text: `sweep harness: ${err instanceof Error ? err.message : String(err)}`,
    });
  } finally {
    stop();
  }

  return { id, surface, hits, panel, controlRows };
}

async function sweepSurface(
  browser: Browser,
  opts: Options,
  surface: Surface,
  ids: string[],
): Promise<StoryResult[]> {
  const results: StoryResult[] = [];
  const queue = [...ids];
  let done = 0;

  const worker = async (): Promise<void> => {
    let page: Page | null = null;
    for (;;) {
      const id = queue.shift();
      if (!id) break;
      // A fresh tab per story: a cold manager boot every time, and a tab that dies takes
      // one story down rather than the rest of the queue.
      try {
        page = await browser.newPage();
        await page.cdp.send("Page.enable");
        await page.cdp.send("Runtime.enable");
        await page.cdp.send("Log.enable");
        results.push(await visitStory(page.cdp, opts, surface, id));
      } catch (err) {
        results.push({
          id,
          surface,
          hits: [
            {
              kind: "exception",
              text: `sweep harness: ${err instanceof Error ? err.message : String(err)}`,
            },
          ],
          panel: surface === "manager" ? "timeout" : "n/a",
          controlRows: [],
        });
      } finally {
        await page?.close().catch(() => undefined);
        page = null;
      }
      done += 1;
      process.stdout.write(`\r  ${surface}: ${done}/${ids.length}   `);
    }
  };

  await Promise.all(Array.from({ length: opts.concurrency }, worker));
  process.stdout.write("\n");
  results.sort((a, b) => a.id.localeCompare(b.id));
  return results;
}

function report(surface: Surface, results: StoryResult[]): number {
  const failing = results.filter((r) => r.hits.length > 0);
  const totalHits = results.reduce((sum, r) => sum + r.hits.length, 0);

  console.log(`\n=== ${surface} ===`);
  console.log(`stories loaded:        ${results.length}`);
  console.log(`stories with errors:   ${failing.length}`);
  console.log(`total console errors:  ${totalHits}`);

  if (surface === "manager") {
    const byPanel = new Map<PanelOutcome, number>();
    for (const r of results) byPanel.set(r.panel, (byPanel.get(r.panel) ?? 0) + 1);
    for (const [state, count] of [...byPanel].sort()) {
      console.log(`controls panel ${state.padEnd(12)} ${count}`);
    }
  }

  if (totalHits > 0) {
    // Group by message so one repeated 404 reads as one problem rather than as N.
    const byMessage = new Map<string, { count: number; stories: Set<string> }>();
    for (const r of results) {
      for (const hit of r.hits) {
        const key = `${hit.kind}: ${hit.text.split("\n")[0].slice(0, 160)}`;
        const slot = byMessage.get(key) ?? { count: 0, stories: new Set<string>() };
        slot.count += 1;
        slot.stories.add(r.id);
        byMessage.set(key, slot);
      }
    }
    console.log(`\ndistinct messages: ${byMessage.size}`);
    for (const [message, slot] of [...byMessage].sort((a, b) => b[1].count - a[1].count)) {
      console.log(`  [${slot.count}x across ${slot.stories.size} stories] ${message}`);
      console.log(`      e.g. ${[...slot.stories][0]}`);
    }
  }
  return totalHits;
}

async function main(): Promise<void> {
  const opts = parseArgs(process.argv.slice(2));
  const ids = await fetchStoryIds(opts.baseUrl, opts.filter);
  console.log(`${opts.baseUrl}: ${ids.length} stories, surfaces: ${opts.surfaces.join(", ")}`);

  const browser = await launch();
  const all: StoryResult[] = [];
  let failed = 0;
  try {
    for (const surface of opts.surfaces) {
      const results = await sweepSurface(browser, opts, surface, ids);
      all.push(...results);
      failed += report(surface, results);
    }
  } finally {
    browser.kill();
  }

  if (opts.jsonPath) await Bun.write(opts.jsonPath, JSON.stringify(all, null, 2));
  process.exit(failed === 0 ? 0 : 1);
}

await main();
