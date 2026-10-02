"use client";

/** Pause / resume / stop for the queue, with its state in words. */

import { CirclePause, CirclePlay, CircleStop } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/overlays";
import { useQueueActions } from "@/lib/actions";
import { formatHour, plural } from "@/lib/format";
import type { QueueState, Settings } from "@/lib/types";

/** "up to 3 at once", naming MAX_JOBS when the container set the limit. */
export function limitText(queue: QueueState): string {
  return queue.max_jobs_source === "env"
    ? `up to ${queue.max_jobs} at once (set by MAX_JOBS)`
    : `up to ${queue.max_jobs} at once`;
}

/** One sentence about what the queue is doing and why. */
export function queueSentence(queue: QueueState, settings: Settings | undefined): string {
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
    return `Converting ${plural(queue.running, "file")}, ${limitText(queue)}.${queue.queued ? ` ${plural(queue.queued, "file")} up next.` : ""}`;
  }
  return queue.queued ? `${plural(queue.queued, "file")} waiting to start.` : "Nothing to convert right now.";
}

/**
 * Pause or resume, plus a quiet "Stop now" only while files are converting
 * (stopping an idle queue would do nothing).
 */
export function QueueControls({ queue, compact = false }: { queue: QueueState; compact?: boolean }) {
  const { pause, resume, stop } = useQueueActions();
  const [confirmStop, setConfirmStop] = useState(false);
  const size = compact ? "sm" : "md";
  return (
    // Wraps rather than runs past a narrow screen's edge (Stop now, then Pause).
    <div className="flex flex-wrap items-center gap-2">
      {queue.running > 0 ? (
        <Button variant="quiet" size={size} onClick={() => setConfirmStop(true)} needsServer>
          <CircleStop aria-hidden />
          Stop now
        </Button>
      ) : null}
      {queue.paused ? (
        <Button variant="primary" size={size} onClick={() => resume.mutate()} loading={resume.isPending} needsServer>
          <CirclePlay aria-hidden />
          Resume
        </Button>
      ) : (
        <Button variant="secondary" size={size} onClick={() => pause.mutate()} loading={pause.isPending} needsServer>
          <CirclePause aria-hidden />
          Pause
        </Button>
      )}
      <ConfirmDialog
        open={confirmStop}
        onOpenChange={setConfirmStop}
        title="Stop converting now?"
        confirmLabel="Stop now"
        cancelLabel="Keep converting"
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
