// A picture of a clone's desktop, for the stories that show something over the clone's screen.

/** A clone's GNOME desktop, drawn: the top bar, a terminal and an editor. In place of the video. */
export function FakeDesktop() {
  return (
    <div className="absolute inset-0 overflow-hidden bg-[linear-gradient(135deg,#1e3a5f,#4a2c6b_60%,#a0456e)] font-sans">
      <div className="flex h-[3.2%] min-h-4 items-center justify-between bg-black px-2 text-[10px] text-white">
        <span>Activities</span>
        <span>Thu 30 Oct 14:02</span>
        <span>● ◐ ⏻</span>
      </div>
      <div className="absolute left-[4%] top-[8%] h-[52%] w-[54%] overflow-hidden rounded-lg bg-[#1e1e1e] shadow-2xl ring-1 ring-black/40">
        <div className="h-5 bg-[#2d2d2d] text-center text-[10px] leading-5 text-neutral-300">pega@pega-we-142: ~/RMNG</div>
        <pre className="p-2 font-mono text-[10px] leading-snug text-emerald-300">
          {"$ cargo test -p control-server\n   Compiling control-server v0.1.0\n    Finished test [unoptimized]\n     Running 214 tests\ntest result: ok. 214 passed\n$ "}
          <span className="bg-emerald-300 text-transparent">_</span>
        </pre>
      </div>
      <div className="absolute right-[4%] top-[30%] h-[62%] w-[50%] overflow-hidden rounded-lg bg-[#fafafa] shadow-2xl ring-1 ring-black/20">
        <div className="h-5 bg-[#ebebeb] text-center text-[10px] leading-5 text-neutral-600">FullDesk.tsx — Visual Studio Code</div>
        <pre className="p-2 font-mono text-[10px] leading-snug text-neutral-700">
          {"export function FullDesk(props) {\n  const mode = useInputMode();\n  const r = useRemote({ ... });\n  return <NgScreenView ... />;\n}"}
        </pre>
      </div>
    </div>
  );
}
