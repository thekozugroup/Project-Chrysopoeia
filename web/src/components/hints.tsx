"use client";

/** Hardware setup hints from detection, each with its copy-paste fix. */

import { Callout, CodeBlock } from "@/components/ui/display";
import type { SetupHint } from "@/lib/types";

const TONE = { info: "info", warning: "warning", error: "danger" } as const;

export function SetupHints({ hints, className }: { hints: SetupHint[]; className?: string }) {
  if (hints.length === 0) return null;
  const order = { error: 0, warning: 1, info: 2 };
  const sorted = [...hints].sort((a, b) => order[a.level] - order[b.level]);
  return (
    <div className={className}>
      <ul className="flex flex-col gap-3">
        {sorted.map((hint) => (
          <li key={`${hint.level}:${hint.title}`}>
            <Callout tone={TONE[hint.level]} title={hint.title}>
              <p>{hint.detail}</p>
              {hint.fix ? <CodeBlock className="mt-3" code={hint.fix} label="Fix" /> : null}
            </Callout>
          </li>
        ))}
      </ul>
    </div>
  );
}
