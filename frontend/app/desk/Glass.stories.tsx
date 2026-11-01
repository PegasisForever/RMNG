import type { Meta, StoryObj } from "@storybook/react-vite";
import { LayoutGrid, Maximize, Plus } from "lucide-react";
import type { ReactNode } from "react";

import { FakeDesktop } from "./__fixtures__/screen";

/** The glass (`./GLASS.md`, `./glass.css`): every piece, in the light or the dark look, over
 *  a clone's desktop or over black. */
function Glass({ look, ground }: { look: "light" | "dark"; ground: "desktop" | "black" }) {
  return (
    <div
      className={`relative overflow-hidden bg-black text-neutral-900 dark:text-white ${look === "dark" ? "dark" : ""}`}
      style={{ width: 1100, height: 700 }}
    >
      {ground === "desktop" ? <FakeDesktop /> : null}
      <div className="absolute inset-0 grid grid-cols-3 gap-6 p-8">
        <Sample name="glass glass-control: floating buttons; lit; with a count; a press">
          <div className="flex gap-2">
            <Button>
              <LayoutGrid aria-hidden className="size-5" />
            </Button>
            <Button lit>
              <Maximize aria-hidden className="size-5" />
            </Button>
            <Button>
              <LayoutGrid aria-hidden className="size-5" />
              <span className="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-rose-500 px-1 text-[10px] font-semibold leading-none text-white shadow-[inset_0_1px_0_rgb(255_255_255/0.4),0_1px_2px_rgb(0_0_0/0.45)]">
                2
              </span>
            </Button>
            <Button pressed>
              <Maximize aria-hidden className="size-5" />
            </Button>
          </div>
        </Sample>

        <Sample name="glass glass-thick: a tooltip">
          <div className="glass glass-thick flex flex-col items-end rounded-lg px-2.5 py-1.5 text-xs">
            <span>Fullscreen</span>
            <span className="text-black/55 dark:text-white/60">Right-click or hold: edit the layout</span>
          </div>
        </Sample>

        <Sample name="glass glass-thick: the monitor's number">
          <div className="glass glass-thick flex size-36 flex-col items-center justify-center rounded-2xl">
            <span className="text-7xl font-bold leading-none tabular-nums">1</span>
            <span className="mt-2 text-sm font-medium text-black/55 dark:text-white/60">Monitor</span>
          </div>
        </Sample>

        <div className="col-span-3">
          <Sample name="glass glass-thick: a panel; what is on it is flat">
            <div className="glass glass-thick w-[30rem] rounded-2xl p-5">
              <h3 className="text-[15px] font-semibold tracking-tight">A panel</h3>
              <p className="mt-1 text-xs text-black/60 dark:text-white/60">Secondary text, at 60 %.</p>
              <div className="mt-3 flex h-28 items-center justify-center gap-3 rounded-xl border border-black/10 bg-white/40 dark:border-white/10 dark:bg-black/25">
                <div className="flex h-16 w-24 items-center justify-center rounded-lg border border-black/15 bg-white text-xs font-semibold dark:border-white/20 dark:bg-neutral-800">
                  1
                </div>
                <div className="flex h-16 w-24 items-center justify-center rounded-lg border border-emerald-500 bg-emerald-50 text-xs font-semibold dark:border-emerald-400/80 dark:bg-emerald-950">
                  0 ★
                </div>
              </div>
              <div className="mt-3 grid grid-cols-[1fr_7rem] gap-2">
                <input
                  aria-label="A field"
                  defaultValue="A field"
                  className="rounded-lg border border-black/10 bg-white/60 px-2.5 py-1.5 text-sm focus:border-black/30 focus:outline-none dark:border-white/10 dark:bg-black/30 dark:focus:border-white/30"
                />
                <select aria-label="A select" className="rounded-lg border border-black/10 bg-white/60 px-2.5 py-1.5 text-sm dark:border-white/10 dark:bg-black/30">
                  <option>A select</option>
                </select>
              </div>
              <div className="mt-4 flex items-center justify-end gap-2">
                <span className="mr-auto text-[11px] text-black/40 dark:text-white/40">Least text, at 40 %.</span>
                <button
                  type="button"
                  className="inline-flex items-center gap-1 rounded-lg border border-black/15 bg-white/50 px-3 py-1.5 text-xs font-medium hover:bg-white/80 dark:border-white/15 dark:bg-white/5 dark:hover:bg-white/10"
                >
                  <Plus aria-hidden className="size-3.5" />
                  A button
                </button>
                <button type="button" className="rounded-lg bg-emerald-600 px-5 py-1.5 text-sm font-semibold text-white hover:bg-emerald-500">
                  Done
                </button>
              </div>
            </div>
          </Sample>
        </div>
      </div>
    </div>
  );
}

function Sample({ name, children }: { name: string; children: ReactNode }) {
  return (
    <div className="flex flex-col items-start gap-2">
      <span className="rounded bg-black/60 px-1.5 py-0.5 font-mono text-[10px] text-white/80">{name}</span>
      {children}
    </div>
  );
}

function Button({ lit = false, pressed = false, children }: { lit?: boolean; pressed?: boolean; children: ReactNode }) {
  return (
    <button
      type="button"
      className={`glass glass-control flex size-11 items-center justify-center rounded-full ${lit ? "glass-lit" : ""} ${
        pressed ? "scale-96 [--glass-inner:inset_0_2px_5px_rgb(0_0_0/0.35)] [--glass-over:rgb(0_0_0/0.08)] [--glass-sheen:rgb(255_255_255/0.03)]" : ""
      }`}
    >
      {children}
    </button>
  );
}

const meta = {
  title: "Desk/Design/Glass",
  component: Glass,
  parameters: { layout: "centered" },
  args: { look: "dark", ground: "desktop" },
} satisfies Meta<typeof Glass>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The dark look over a clone's desktop: the glass blurs its colors. */
export const Dark: Story = {};

/** The light look over a clone's desktop. On: the dark glass. */
export const Light: Story = { args: { look: "light" } };

/** The dark look over black (a clone with no picture yet): the rim and the sheen alone show the
 *  pieces. */
export const DarkOverBlack: Story = { args: { ground: "black" } };

/** The light look over black. */
export const LightOverBlack: Story = { args: { look: "light", ground: "black" } };
