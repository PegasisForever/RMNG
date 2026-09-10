/** Real-browser drag regression checks. Start Storybook, then run:
 * bun run scripts/check-groups-editor.ts http://localhost:6007
 * Uses the same dependency-free CDP client as the Storybook console sweep. */
import { strict as assert } from "node:assert";
import { launch } from "./cdp";

const origin = process.argv[2] ?? "http://localhost:6007";
const browser = await launch();
const page = await browser.newPage();
const errors: string[] = [];
page.cdp.on((method, params) => {
  if (method === "Runtime.exceptionThrown") errors.push(JSON.stringify(params));
  if (method === "Runtime.consoleAPICalled" && params.type === "error")
    errors.push(JSON.stringify(params));
});
await page.cdp.send("Runtime.enable");
await page.cdp.send("Page.enable");

async function evaluate<T>(expression: string): Promise<T> {
  const response = await page.cdp.send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (response.exceptionDetails)
    throw new Error(JSON.stringify(response.exceptionDetails));
  return (response.result as { value: T }).value;
}
async function frame() {
  await evaluate(
    "new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))",
  );
}
async function waitFor(expression: string) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await evaluate(expression)) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Timed out: ${expression}`);
}
const group = (name: string) => `[data-group-name=${JSON.stringify(name)}]`;
const member = (name: string, email: string) =>
  `${group(name)} [data-account=${JSON.stringify(email)}] button[aria-label^="reorder "]`;
const slot = (name: string, index: number) =>
  `${group(name)} [data-drop-kind="member"][data-drop-index="${index}"]`;
const groupSlot = (index: number) =>
  `[data-drop-kind="group"][data-drop-index="${index}"]`;
async function point(selector: string, scroll = true) {
  if (scroll) {
    await evaluate(
      `document.querySelector(${JSON.stringify(selector)}).scrollIntoView({block:'center'})`,
    );
    await frame();
  }
  return evaluate<{ x: number; y: number }>(
    `(() => {const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2};})()`,
  );
}
async function moveTo(point: { x: number; y: number }) {
  await page.cdp.send("Input.dispatchMouseEvent", {
    type: "mouseMoved",
    ...point,
    buttons: 1,
  });
  await frame();
}
async function start(selector: string) {
  const p = await point(selector);
  await page.cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", ...p });
  await page.cdp.send("Input.dispatchMouseEvent", {
    type: "mousePressed",
    ...p,
    button: "left",
    clickCount: 1,
  });
  await moveTo({ x: p.x + 8, y: p.y });
  await waitFor("!!document.querySelector('[data-drag-preview]')");
}
async function hover(selector: string) {
  const p = await point(selector);
  await moveTo(p);
  return p;
}
async function release(p: { x: number; y: number }) {
  await page.cdp.send("Input.dispatchMouseEvent", {
    type: "mouseReleased",
    ...p,
    button: "left",
    clickCount: 1,
  });
  await frame();
}
async function key(code: string, key = code) {
  await page.cdp.send("Input.dispatchKeyEvent", {
    type: "keyDown",
    code,
    key,
    windowsVirtualKeyCode:
      code === "Space"
        ? 32
        : code === "Escape"
          ? 27
          : code === "ArrowDown"
            ? 40
            : 38,
  });
  await page.cdp.send("Input.dispatchKeyEvent", { type: "keyUp", code, key });
  await frame();
}
async function reset() {
  await evaluate(
    "[...document.querySelectorAll('button')].find(b => b.textContent === 'Reset example').dispatchEvent(new PointerEvent('pointerdown', {bubbles:true}))",
  );
  await frame();
  await evaluate(
    "document.querySelector('[data-editor-scroll]').scrollTop = 0",
  );
  await frame();
}
async function snapshot() {
  return evaluate<{ name: string; accounts: string[] }[]>(
    "JSON.parse(document.querySelector('[data-draft-value]').textContent)",
  );
}
async function changes() {
  return evaluate<number>(
    "Number(document.querySelector('[data-draft-changes]').textContent)",
  );
}
async function test(name: string, body: () => Promise<void>) {
  await reset();
  await body();
  process.stdout.write(`PASS ${name}\n`);
}

try {
  await page.cdp.send("Page.navigate", {
    url: `${origin}/iframe.html?id=settings-components-settingsgroupseditor--drag-and-drop&viewMode=story`,
  });
  await waitFor("!!document.querySelector('[data-groups-editor]')");
  const original = await snapshot();

  await test("hover keeps the source mounted and commits only the final destination", async () => {
    const selector = member("pooled", "sam@example.com");
    await evaluate(
      `void (window.originalHandle = document.querySelector(${JSON.stringify(selector)}))`,
    );
    await start(selector);
    for (const target of [
      slot("team", 0),
      slot("empty", 0),
      slot("pooled", 0),
      slot("team", 1),
    ]) {
      await hover(target);
      assert.deepEqual(await snapshot(), original);
      assert.equal(await changes(), 0);
      assert.equal(await evaluate("window.originalHandle.isConnected"), true);
      assert.equal(
        await evaluate(
          "document.querySelectorAll('[data-drag-preview]').length",
        ),
        1,
      );
    }
    const p = await hover(slot("team", 1));
    await release(p);
    assert.deepEqual(
      (await snapshot()).map((g) => g.accounts),
      [
        ["alex@example.com"],
        ["alex@openai.com", "sam@example.com"],
        ["alex@example.com"],
        [],
      ],
    );
    assert.equal(await changes(), 1);
    assert.equal(
      await evaluate("document.querySelectorAll('[data-drag-preview]').length"),
      0,
    );
  });

  await test("Escape and outside drops leave the original draft unchanged", async () => {
    await start(member("pooled", "sam@example.com"));
    await hover(slot("team", 0));
    await key("Escape");
    await release({ x: 2, y: 2 });
    assert.deepEqual(await snapshot(), original);
    assert.equal(await changes(), 0);
    await start(member("pooled", "sam@example.com"));
    await hover(slot("team", 0));
    await moveTo({ x: 2, y: 2 });
    await release({ x: 2, y: 2 });
    assert.deepEqual(await snapshot(), original);
    assert.equal(await changes(), 0);
  });

  await test("hidden content outside the scroll viewport cannot receive a drop", async () => {
    for (const selector of [
      member("pooled", "sam@example.com"),
      `${group("pooled")} button[aria-label="reorder group pooled"]`,
    ]) {
      for (const side of ["above", "below", "left", "right"]) {
        await reset();
        await start(selector);
        const outside = await evaluate<{ x: number; y: number }>(`(() => {
          const r=document.querySelector('[data-editor-scroll]').getBoundingClientRect();
          const side=${JSON.stringify(side)};
          return {x:side==='left'?r.left-20:side==='right'?r.right+20:r.x+r.width/2,
            y:side==='above'?r.top-30:side==='below'?r.bottom+30:r.y+r.height/2};
        })()`);
        await moveTo(outside);
        assert.equal(
          await evaluate(
            "document.querySelectorAll('[data-drop-active]').length",
          ),
          0,
        );
        await release(outside);
        assert.deepEqual(await snapshot(), original);
        assert.equal(await changes(), 0);
      }
    }
  });

  await test("duplicate membership is refused and a later valid drop still works", async () => {
    await start(member("pooled", "alex@example.com"));
    const p = await hover(slot("shared", 0));
    assert.match(
      await evaluate<string>(
        "document.querySelector('[data-groups-editor] [role=status]').textContent",
      ),
      /already contains/,
    );
    await release(p);
    assert.deepEqual(await snapshot(), original);
    assert.equal(await changes(), 0);
    await start(member("pooled", "alex@example.com"));
    await release(await hover(slot("team", 0)));
    assert.deepEqual((await snapshot())[2].accounts, ["alex@example.com"]);
    assert.deepEqual((await snapshot())[0].accounts, ["sam@example.com"]);
    assert.equal(await changes(), 1);
  });

  await test("same-group moves work in both directions", async () => {
    await start(member("pooled", "alex@example.com"));
    await release(await hover(slot("pooled", 2)));
    assert.deepEqual((await snapshot())[0].accounts, [
      "sam@example.com",
      "alex@example.com",
    ]);
    await start(member("pooled", "alex@example.com"));
    await release(await hover(slot("pooled", 0)));
    assert.deepEqual(await snapshot(), original);
    assert.equal(await changes(), 2);
  });

  await test("empty groups accept accounts after scrolling", async () => {
    await start(member("pooled", "sam@example.com"));
    await release(await hover(slot("empty", 0)));
    assert.deepEqual((await snapshot())[3].accounts, ["sam@example.com"]);
    assert.equal(await changes(), 1);
  });

  await test("group reorders retain identity and all members", async () => {
    const id = await evaluate<string>(
      `document.querySelector(${JSON.stringify(group("pooled"))}).dataset.groupId`,
    );
    await start(`${group("pooled")} button[aria-label="reorder group pooled"]`);
    const p = await hover(groupSlot(4));
    assert.equal(
      await evaluate(
        `document.querySelector(${JSON.stringify(groupSlot(4))}).dataset.dropActive`,
      ),
      "true",
      await evaluate<string>(
        "document.querySelector('[data-groups-editor] [role=status]').textContent",
      ),
    );
    assert.deepEqual(await snapshot(), original);
    await release(p);
    assert.deepEqual(
      (await snapshot()).map((g) => g.name),
      ["team", "shared", "empty", "pooled"],
    );
    assert.equal(
      await evaluate(
        `document.querySelector(${JSON.stringify(group("pooled"))}).dataset.groupId`,
      ),
      id,
    );
    await start(`${group("pooled")} button[aria-label="reorder group pooled"]`);
    await release(await hover(groupSlot(0)));
    assert.deepEqual(await snapshot(), original);
    assert.equal(await changes(), 2);
  });

  await test("keyboard moves an account to another group", async () => {
    const selector = member("pooled", "sam@example.com");
    await point(selector);
    await evaluate(
      `document.querySelector(${JSON.stringify(selector)}).focus()`,
    );
    await key("Space", " ");
    assert.equal(
      await evaluate("document.querySelectorAll('[data-drag-preview]').length"),
      1,
    );
    await key("ArrowDown");
    assert.match(
      await evaluate<string>(
        "document.querySelector('[data-groups-editor] [role=status]').textContent",
      ),
      /Move to team/,
    );
    assert.equal(await changes(), 0);
    await key("Space", " ");
    assert.deepEqual((await snapshot())[1].accounts, [
      "sam@example.com",
      "alex@openai.com",
    ]);
    assert.equal(await changes(), 1);
    assert.equal(
      await evaluate(
        `document.activeElement === document.querySelector(${JSON.stringify(member("team", "sam@example.com"))})`,
      ),
      true,
      "Focus follows the moved account",
    );
  });

  await test("external replacement cancels a drag without publishing stale data", async () => {
    await start(member("pooled", "sam@example.com"));
    await hover(slot("team", 1));
    await reset();
    await release({ x: 2, y: 2 });
    assert.deepEqual(await snapshot(), original);
    assert.equal(await changes(), 0);
    assert.equal(
      await evaluate("document.querySelectorAll('[data-drag-preview]').length"),
      0,
    );
  });

  await test("the scroll container moves at its edge without changing the draft", async () => {
    await start(member("pooled", "sam@example.com"));
    const edge = await evaluate<{ x: number; y: number }>(
      "(() => {const r=document.querySelector('[data-editor-scroll]').getBoundingClientRect(); return {x:r.x+r.width/2,y:r.bottom-8};})()",
    );
    await moveTo(edge);
    await waitFor(
      "document.querySelector('[data-editor-scroll]').scrollTop > 20",
    );
    assert.deepEqual(await snapshot(), original);
    assert.equal(await changes(), 0);
    await key("Escape");
    await release(edge);
  });

  await test("rename, reference, removal and creation still edit the draft", async () => {
    const id = await evaluate<string>(
      `document.querySelector(${JSON.stringify(group("pooled"))}).dataset.groupId`,
    );
    await evaluate(
      `document.querySelector(${JSON.stringify(`${group("pooled")} input`)}).select()`,
    );
    await page.cdp.send("Input.insertText", { text: "renamed" });
    await frame();
    assert.equal((await snapshot())[0].name, "renamed");
    assert.equal(
      await evaluate(
        `document.querySelector(${JSON.stringify(group("renamed"))}).dataset.groupId`,
      ),
      id,
    );
    await evaluate(
      `(() => {const s=document.querySelector(${JSON.stringify(`${group("empty")} select`)}); s.value='sam@example.com'; s.dispatchEvent(new Event('change',{bubbles:true}));})()`,
    );
    await frame();
    assert.deepEqual((await snapshot())[3].accounts, ["sam@example.com"]);
    assert.deepEqual((await snapshot())[0].accounts, original[0].accounts);
    await evaluate(
      `document.querySelector(${JSON.stringify(`${group("empty")} button[aria-label="remove sam@example.com from this group"]`)}).click()`,
    );
    await frame();
    assert.deepEqual((await snapshot())[3].accounts, []);
    await evaluate(
      `document.querySelector(${JSON.stringify(`${group("empty")} button[aria-label="Remove group empty"]`)}).click()`,
    );
    await frame();
    assert.equal((await snapshot()).length, 3);
    await evaluate(
      "[...document.querySelectorAll('button')].find(b => b.textContent === '+ Create group').click()",
    );
    await frame();
    assert.deepEqual((await snapshot())[3], { name: "", accounts: [] });
  });

  await test("the group rows use logos and icon buttons without counters or guide lines", async () => {
    assert.equal(
      await evaluate(
        "document.querySelector('[data-account] img[alt=Claude]').naturalWidth > 0",
      ),
      true,
    );
    assert.equal(
      await evaluate(
        "document.querySelector('[data-account] img[alt=ChatGPT]').naturalWidth > 0",
      ),
      true,
    );
    assert.equal(
      await evaluate(
        "document.querySelector('[data-groups-editor] [role=status]').getBoundingClientRect().height",
      ),
      1,
    );
    assert.equal(
      await evaluate(
        "document.querySelector('[data-group-name] button[aria-label^=\"Remove group\"]').textContent",
      ),
      "",
    );
    assert.equal(
      await evaluate(
        "document.querySelectorAll('[data-group-name] .tabular-nums').length",
      ),
      0,
    );
    assert.equal(
      await evaluate(
        "getComputedStyle(document.querySelector('[data-account]').parentElement).borderLeftWidth",
      ),
      "0px",
    );
    assert.equal(
      await evaluate(
        "document.querySelector('[data-group-name] input').getBoundingClientRect().left",
      ),
      await evaluate(
        "document.querySelector('[data-account]').getBoundingClientRect().left",
      ),
    );
    assert.equal(
      await evaluate(
        "document.querySelector('[data-group-name] input').getBoundingClientRect().left",
      ),
      await evaluate(
        "[...document.querySelectorAll('[data-group-name] button')].find(b => b.textContent === '+ Import claude').getBoundingClientRect().left",
      ),
    );
  });

  await page.cdp.send("Page.navigate", {
    url: `${origin}/iframe.html?id=settings-components-settingspanelview--llm&viewMode=story`,
  });
  await waitFor("!!document.querySelector('[data-groups-editor]')");
  assert.equal(
    await evaluate(
      "[...document.querySelectorAll('h1,h2,h3,h4')].filter(h => h.textContent === 'Stuck detection').length",
    ),
    1,
  );
  assert.equal(
    await evaluate(
      "document.body.innerText.includes('Pools of accounts a clone binds as one')",
    ),
    false,
  );
  process.stdout.write(
    "PASS the settings pane has one Stuck detection heading and no group explanation\n",
  );
  const screenshot = await page.cdp.send("Page.captureScreenshot", {
    format: "png",
  });
  await Bun.write(
    "/tmp/groups-editor-rewrite.png",
    Buffer.from(screenshot.data as string, "base64"),
  );
  assert.deepEqual(errors, [], "Browser errors");
  process.stdout.write(
    "All group editor browser checks passed. Screenshot: /tmp/groups-editor-rewrite.png\n",
  );
} catch (error) {
  const screenshot = await page.cdp.send("Page.captureScreenshot", {
    format: "png",
  });
  await Bun.write(
    "/tmp/groups-editor-failure.png",
    Buffer.from(screenshot.data as string, "base64"),
  );
  process.stderr.write(`Browser errors: ${JSON.stringify(errors)}\n`);
  throw error;
} finally {
  await page.close();
  browser.kill();
}
