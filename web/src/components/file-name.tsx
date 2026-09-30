/**
 * A file name in a narrow list row. A long name whose episode comes late is
 * cut before it, so the part that tells similar names apart stays in view:
 * "Das außergewöhnlich…S01E01 - Pilot.mkv" (see `splitFileName`); any other
 * name is cut at its end. Screen readers and the tooltip get the whole name.
 */

import { splitFileName } from "@/lib/format";
import { cn } from "@/lib/utils";

export function FileName({ name, className }: { name: string; className?: string }) {
  const split = splitFileName(name);
  if (!split) {
    return (
      <span className={cn("block min-w-0 truncate", className)} title={name}>
        {name}
      </span>
    );
  }
  return (
    <span className={cn("flex min-w-0", className)} title={name}>
      <span className="sr-only">{name}</span>
      {/* The start gives way first, down to a few letters; then the end is cut too. */}
      <span aria-hidden className="min-w-[7ch] shrink-[1000] overflow-hidden text-ellipsis whitespace-pre">
        {split.head}
      </span>
      <span aria-hidden data-file-tail className="min-w-0 overflow-hidden text-ellipsis whitespace-pre">
        {split.tail}
      </span>
    </span>
  );
}
