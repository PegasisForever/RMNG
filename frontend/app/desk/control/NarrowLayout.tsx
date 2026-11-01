// The control panel where it is narrow (a phone, a small window): the tool row
// (`../ToolRow.tsx`) over the open page. The control panel comes in as `children` (`./ControlPanelContainer.tsx` keeps each one
// it opened mounted, the others hidden).
import type { ComponentProps, ReactNode } from "react";

import { GAP } from "../geometry";
import { ToolRow } from "../ToolRow";

export function NarrowLayout({ row, children }: { row: ComponentProps<typeof ToolRow>; children: ReactNode }) {
  return (
    <div className="flex h-full flex-col p-1.5" style={{ gap: GAP }}>
      <ToolRow {...row} />
      <div className="relative min-h-0 flex-1">{children}</div>
    </div>
  );
}
