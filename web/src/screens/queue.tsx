"use client";

/** Queue: Running / Up next / History, with queue controls and job actions. */

import { ArrowUpToLine, CircleCheck, CirclePause, FolderPlus, History, ListVideo, RotateCcw, Trash2, X } from "lucide-react";
import { useState, type ReactNode } from "react";
import { JobCard, JobCardSkeleton, savingsText } from "@/components/jobs";
import { QueueControls, queueSentence } from "@/components/queue-controls";
import { PageHeader } from "@/components/shell";
import { JobStateBadge } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Callout, EmptyState, Skeleton } from "@/components/ui/display";
import { NavTabs, Pager, useClampedOffset } from "@/components/ui/nav-tabs";
import { ConfirmDialog, Tooltip } from "@/components/ui/overlays";
import { useJobActions } from "@/lib/actions";
import { errorMessage } from "@/lib/api";
import { formatBytes, formatRelative, plural } from "@/lib/format";
import { useJobs, useLibraries, useQueueState, useSettings } from "@/lib/queries";
import { href, openSheet, updateParams, type Route } from "@/lib/router";
import type { Job } from "@/lib/types";
import { cn } from "@/lib/utils";

const PAGE = 50;

function openJob(job: Job) {
  openSheet({ job: job.id });
}

function useLibraryName() {
  const libraries = useLibraries();
  return (id: string) => libraries.data?.find((l) => l.id === id)?.name ?? "";
}

function ListSkeleton() {
  return (
    <div className="divide-y divide-line rounded-lg border border-line bg-surface">
      {[0, 1, 2, 3].map((i) => (
        <div key={i} className="flex items-center gap-4 px-4 py-3.5">
          <Skeleton className="h-4 w-6" />
          <Skeleton className="h-4 flex-1" />
          <Skeleton className="h-4 w-20" />
        </div>
      ))}
    </div>
  );
}

/**
 * A list that couldn't load: an error with a retry instead of an empty
 * state that would claim there is nothing.
 */
function LoadFailed({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  return (
    <Callout
      tone="danger"
      title="Couldn't load the queue"
      action={
        <Button size="sm" variant="secondary" onClick={onRetry}>
          Try again
        </Button>
      }
    >
      {errorMessage(error)}
    </Callout>
  );
}

/** Above a list that failed to refresh but still shows what it last had. */
function StaleNote({ show }: { show: boolean }) {
  if (!show) return null;
  return (
    <p role="status" className="mb-3 text-[0.8125rem] text-muted">
      Showing the last known state. Couldn&apos;t refresh just now; trying again shortly.
    </p>
  );
}

/** Empty states point at adding a library when there are none yet. */
function useNoLibraries(): boolean {
  const libraries = useLibraries();
  return libraries.data !== undefined && libraries.data.length === 0;
}

function AddLibraryEmpty({ title }: { title: ReactNode }) {
  return (
    <EmptyState
      icon={<FolderPlus aria-hidden />}
      title={title}
      action={
        <a href={href("/libraries/new")} className={buttonVariants({ variant: "primary" })}>
          Add a library
        </a>
      }
    >
      Add a folder of videos first. Files that are worth converting are queued automatically after it&apos;s scanned.
    </EmptyState>
  );
}

function RunningTab() {
  const running = useJobs({ state: "running", limit: 50 });
  const queue = useQueueState();
  const noLibraries = useNoLibraries();
  if (running.isPending) {
    return (
      <div className="grid gap-4 lg:grid-cols-2">
        <JobCardSkeleton />
        <JobCardSkeleton />
      </div>
    );
  }
  if (running.error && !running.data) return <LoadFailed error={running.error} onRetry={() => running.refetch()} />;
  const items = running.data?.items ?? [];
  if (!items.length) {
    if (noLibraries) return <AddLibraryEmpty title="Nothing to convert yet" />;
    if (queue.data?.paused) {
      return (
        <EmptyState icon={<CirclePause aria-hidden />} title="The queue is paused">
          {queue.data.queued
            ? `${plural(queue.data.queued, "file")} waiting. Nothing new starts until you resume.`
            : "Nothing new starts until you resume."}
        </EmptyState>
      );
    }
    if (queue.data && queue.data.running > 0) {
      // A job just started; its card arrives with the next list refresh.
      return (
        <div className="grid gap-4 lg:grid-cols-2" aria-busy="true" aria-label="Loading what's converting">
          <JobCardSkeleton />
        </div>
      );
    }
    return (
      <EmptyState icon={<ListVideo aria-hidden />} title="Nothing is converting">
        Files in the queue start automatically, a few at a time. How many run at once is set in{" "}
        <a href={href("/settings/processing")}>Settings › Processing</a>.
      </EmptyState>
    );
  }
  return (
    <>
      <StaleNote show={Boolean(running.error)} />
      <div className="grid gap-4 lg:grid-cols-2">
        {items.map((job, i) => (
          <JobCard key={job.id} job={job} onOpen={openJob} announce={i === 0} />
        ))}
      </div>
    </>
  );
}

function UpNextTab({ offset }: { offset: number }) {
  const queued = useJobs({ state: "queued", limit: PAGE, offset });
  const libraryName = useLibraryName();
  const noLibraries = useNoLibraries();
  const { cancel, moveToTop } = useJobActions();
  useClampedOffset(offset, queued.data?.total, PAGE);
  if (queued.isPending) return <ListSkeleton />;
  if (queued.error && !queued.data) return <LoadFailed error={queued.error} onRetry={() => queued.refetch()} />;
  const items = queued.data?.items ?? [];
  if (!items.length) {
    if (offset > 0 && (queued.data?.total ?? 0) > 0) return <ListSkeleton />;
    if (noLibraries) return <AddLibraryEmpty title="The queue is empty" />;
    return (
      <EmptyState icon={<CircleCheck aria-hidden />} title="The queue is empty">
        New files are added automatically after each scan. You can also queue files from a library.
      </EmptyState>
    );
  }
  return (
    <>
      <StaleNote show={Boolean(queued.error)} />
      <ol className="divide-y divide-line rounded-lg border border-line bg-surface">
        {items.map((job, i) => (
          <li key={job.id} className="flex items-center gap-3 px-3 py-2.5 sm:gap-4 sm:px-4">
            <span className="w-7 shrink-0 text-right text-[0.8125rem] text-muted tabular">
              <span className="sr-only">Position </span>
              {offset + i + 1}
            </span>
            <button type="button" onClick={() => openJob(job)} className="min-w-0 flex-1 text-left">
              <span className="block truncate text-sm font-medium text-fg hover:text-accent-ink">{job.file_name}</span>
              <span className="block truncate text-[0.8125rem] text-muted">
                {[libraryName(job.library_id), formatBytes(job.input_size), `added ${formatRelative(job.created_at)}`]
                  .filter(Boolean)
                  .join(" · ")}
              </span>
            </button>
            <div className="flex shrink-0 gap-1">
              <Tooltip content="Move to top">
                <Button
                  variant="quiet"
                  size="icon-sm"
                  aria-label={`Move ${job.file_name} to the top`}
                  disabled={offset + i === 0 || moveToTop.isPending}
                  onClick={() => moveToTop.mutate(job)}
                >
                  <ArrowUpToLine />
                </Button>
              </Tooltip>
              <Tooltip content="Remove from queue">
                <Button
                  variant="quiet"
                  size="icon-sm"
                  aria-label={`Remove ${job.file_name} from the queue`}
                  disabled={cancel.isPending}
                  onClick={() => cancel.mutate(job)}
                >
                  <X />
                </Button>
              </Tooltip>
            </div>
          </li>
        ))}
      </ol>
      <Pager
        offset={offset}
        limit={PAGE}
        total={queued.data?.total ?? 0}
        onPage={(next) => updateParams({ offset: next || null })}
        noun="files"
      />
    </>
  );
}

function HistoryTab({ offset }: { offset: number }) {
  const history = useJobs({ state: "history", limit: PAGE, offset });
  const libraryName = useLibraryName();
  const { retry, clearHistory } = useJobActions();
  const [confirmClear, setConfirmClear] = useState(false);
  useClampedOffset(offset, history.data?.total, PAGE);
  if (history.isPending) return <ListSkeleton />;
  if (history.error && !history.data) return <LoadFailed error={history.error} onRetry={() => history.refetch()} />;
  const items = history.data?.items ?? [];
  if (!items.length) {
    if (offset > 0 && (history.data?.total ?? 0) > 0) return <ListSkeleton />;
    return (
      <EmptyState icon={<History aria-hidden />} title="No history yet">
        Every finished, skipped or failed conversion is listed here with its checks and savings.
      </EmptyState>
    );
  }
  return (
    <>
      <StaleNote show={Boolean(history.error)} />
      <div className="mb-3 flex justify-end">
        <Button variant="quiet" size="sm" onClick={() => setConfirmClear(true)}>
          <Trash2 aria-hidden />
          Clear history
        </Button>
      </div>
      <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
        {items.map((job) => {
          const savings = savingsText(job.input_size, job.output_size);
          const note =
            job.state === "done" && savings
              ? savings.text
              : job.state === "failed"
                ? (job.error ?? "Failed")
                : job.state === "skipped"
                  ? (job.skip_reason ?? "Kept the original")
                  : "Cancelled. The original was left as it is.";
          return (
            <li key={job.id} className="flex items-center gap-3 px-3 py-3 sm:px-4">
              <button type="button" onClick={() => openJob(job)} className="min-w-0 flex-1 text-left">
                <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
                  <span className="min-w-0 truncate text-sm font-medium text-fg hover:text-accent-ink">
                    {job.file_name}
                  </span>
                  <JobStateBadge job={job} />
                </span>
                <span className="mt-1 flex flex-wrap gap-x-1 text-[0.8125rem]">
                  <span
                    className={cn(
                      "min-w-0",
                      job.state === "failed" ? "text-danger" : job.state === "done" ? "text-fg/85" : "text-muted",
                    )}
                  >
                    {note}
                  </span>
                  <span className="text-muted">
                    · {libraryName(job.library_id)} · {formatRelative(job.finished_at)}
                  </span>
                </span>
              </button>
              {job.state === "failed" || job.state === "cancelled" ? (
                <Button variant="secondary" size="sm" onClick={() => retry.mutate(job)} disabled={retry.isPending}>
                  <RotateCcw aria-hidden />
                  <span className="hidden sm:inline">{job.state === "failed" ? "Try again" : "Queue again"}</span>
                  <span className="sr-only sm:hidden">Try {job.file_name} again</span>
                </Button>
              ) : null}
            </li>
          );
        })}
      </ul>
      <Pager
        offset={offset}
        limit={PAGE}
        total={history.data?.total ?? 0}
        onPage={(next) => updateParams({ offset: next || null })}
        noun="results"
      />
      <ConfirmDialog
        open={confirmClear}
        onOpenChange={setConfirmClear}
        title="Clear the history?"
        confirmLabel="Clear history"
        destructive
        loading={clearHistory.isPending}
        onConfirm={() => clearHistory.mutate(undefined, { onSettled: () => setConfirmClear(false) })}
      >
        <p>This removes the list of finished conversions, including their checks and logs.</p>
        <p>Your files and their status in each library stay exactly as they are.</p>
      </ConfirmDialog>
    </>
  );
}

export function QueueScreen({ route }: { route: Route }) {
  const tab = route.segments[1] === "next" ? "next" : route.segments[1] === "history" ? "history" : "running";
  const queue = useQueueState();
  const settings = useSettings();
  const history = useJobs({ state: "history", limit: 1 });
  const offset = Math.max(0, Number(route.params.get("offset")) || 0);
  return (
    <div>
      <PageHeader
        title="Queue"
        description={queue.data ? queueSentence(queue.data, settings.data) : " "}
        actions={queue.data ? <QueueControls queue={queue.data} /> : null}
      />
      <NavTabs
        label="Queue views"
        className="mb-6"
        tabs={[
          { href: href("/queue"), label: "Running", active: tab === "running", count: queue.data?.running },
          { href: href("/queue/next"), label: "Up next", active: tab === "next", count: queue.data?.queued },
          { href: href("/queue/history"), label: "History", active: tab === "history", count: history.data?.total },
        ]}
      />
      {tab === "running" ? <RunningTab /> : null}
      {tab === "next" ? <UpNextTab offset={offset} /> : null}
      {tab === "history" ? <HistoryTab offset={offset} /> : null}
    </div>
  );
}
