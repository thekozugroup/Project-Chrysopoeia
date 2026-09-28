"use client";

/** Pause / resume / stop for the queue, with its state in words. */

import { CirclePause, CirclePlay, CircleStop, Ellipsis } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { ActionMenu, ConfirmDialog } from "@/components/ui/overlays";
import { useQueueActions } from "@/lib/actions";
import { formatHour, plural } from "@/lib/format";
import type { QueueState, Settings } from "@/lib/types";

/** One sentence about what the queue is doing and why. */
export function queueSentence(queue: QueueState, settings: Settings | undefined): string {
  const slots = queue.max_jobs_auto
    ? `up to ${queue.max_jobs} at once (automatic)`
    : `up to ${queue.max_jobs} at once`;
  if (queue.paused) {
    return queue.running
      ? `Paused. ${plural(queue.running, "file")} still finishing; nothing new will start.`
      : `Paused. ${queue.queued ? `${plural(queue.queued, "file")} waiting.` : "Nothing is waiting."}`;
  }
  if (queue.waiting_for_schedule) {
    const hours = settings?.active_hours;
    return hours
      ? `Waiting for active hours (${formatHour(hours.start)}–${formatHour(hours.end)}). ${plural(queue.queued, "file")} waiting.`
      : `Waiting for active hours. ${plural(queue.queued, "file")} waiting.`;
  }
  if (queue.running) {
    return `Converting ${plural(queue.running, "file")}, ${slots}.${queue.queued ? ` ${plural(queue.queued, "file")} up next.` : ""}`;
  }
  return queue.queued ? `${plural(queue.queued, "file")} waiting, ${slots}.` : `Nothing to do right now. Converts ${slots}.`;
}

export function QueueControls({ queue, compact = false }: { queue: QueueState; compact?: boolean }) {
  const { pause, resume, stop } = useQueueActions();
  const [confirmStop, setConfirmStop] = useState(false);
  return (
    <div className="flex items-center gap-2">
      {queue.paused ? (
        <Button variant="primary" size={compact ? "sm" : "md"} onClick={() => resume.mutate()} loading={resume.isPending}>
          <CirclePlay aria-hidden />
          Resume
        </Button>
      ) : (
        <Button variant="secondary" size={compact ? "sm" : "md"} onClick={() => pause.mutate()} loading={pause.isPending}>
          <CirclePause aria-hidden />
          Pause
        </Button>
      )}
      <ActionMenu
        trigger={
          <Button variant="quiet" size={compact ? "icon-sm" : "icon"} aria-label="More queue actions">
            <Ellipsis />
          </Button>
        }
        actions={[
          {
            label: "Stop now",
            icon: <CircleStop aria-hidden />,
            destructive: true,
            disabled: queue.running === 0,
            onSelect: () => setConfirmStop(true),
          },
        ]}
      />
      <ConfirmDialog
        open={confirmStop}
        onOpenChange={setConfirmStop}
        title="Stop converting now?"
        confirmLabel="Stop now"
        destructive
        loading={stop.isPending}
        onConfirm={() => stop.mutate(undefined, { onSettled: () => setConfirmStop(false) })}
      >
        <p>
          {plural(queue.running, "file")} will stop immediately and go back to the queue. Work on{" "}
          {queue.running === 1 ? "it" : "them"} so far is discarded; the originals are untouched.
        </p>
        <p>The queue is paused afterwards. Resume when you&apos;re ready.</p>
      </ConfirmDialog>
    </div>
  );
}
