// The app's own question and message dialogs, in place of the browser's `confirm()` and
// `alert()`, which the browser draws in its own look over the page. `ask` puts a question and
// gives the answer; `tell` gives a message. One layer for the page (`ConfirmLayer`, in
// `routes/app.tsx`) shows them, one at a time, in order.
import { useEffect, useState } from "react";

import { BUTTON, DANGER_BUTTON, DONE_BUTTON, TEXT, TEXT_2 } from "~/desk/control/ui";

import { ModalShell } from "./ModalShell";

export type Question = {
  title: string;
  /** More about it, under the title. */
  body?: string;
  /** The button that says yes. */
  ok: string;
  /** Yes destroys something: its button is red text. */
  danger?: boolean;
  /** The button that says no; null: none (a message). */
  cancel?: string | null;
};

type Asked = Question & { id: number; answer: (yes: boolean) => void };

let next = 0;

let queue: Asked[] = [];
let shown: ((q: Asked[]) => void) | null = null;

/** Put a question to the operator; true when they said yes. With no layer on the page (a
 *  story), the answer is no. */
export function ask(q: Question): Promise<boolean> {
  if (!shown) return Promise.resolve(false);
  return new Promise((answer) => {
    queue = [...queue, { ...q, id: next++, answer }];
    shown?.(queue);
  });
}

/** Tell the operator something; resolves when they closed it. */
export function tell(title: string, body?: string): Promise<void> {
  return ask({ title, body, ok: "OK", cancel: null }).then(() => undefined);
}

export function ConfirmLayer() {
  const [list, setList] = useState<Asked[]>([]);
  useEffect(() => {
    shown = setList;
    return () => {
      shown = null;
      for (const q of queue) q.answer(false);
      queue = [];
    };
  }, []);
  const q = list[0];
  if (!q) return null;
  const done = (yes: boolean) => {
    q.answer(yes);
    queue = queue.filter((x) => x !== q);
    setList(queue);
  };
  return (
    <ModalShell key={q.id} size="sm" onExited={() => done(false)}>
      {(close) => (
        <div className={`flex flex-col gap-3 ${TEXT}`}>
          <h2 className="text-base font-semibold">{q.title}</h2>
          {q.body ? <p className={`whitespace-pre-line text-sm ${TEXT_2}`}>{q.body}</p> : null}
          <div className="flex justify-end gap-2">
            {q.cancel !== null ? (
              <button type="button" className={`${BUTTON} px-3 py-1.5 text-sm`} onClick={() => close(() => done(false))}>
                {q.cancel ?? "Cancel"}
              </button>
            ) : null}
            <button
              type="button"
              autoFocus
              className={
                q.danger
                  ? `${DANGER_BUTTON} px-3 py-1.5 text-sm font-medium`
                  : `${DONE_BUTTON} px-3 py-1.5 text-sm font-medium`
              }
              onClick={() => close(() => done(true))}
            >
              {q.ok}
            </button>
          </div>
        </div>
      )}
    </ModalShell>
  );
}
