"use client";

/** Queue: Running / Up next / History, with queue controls and job actions. */

import {
  ArrowUpToLine,
  CircleCheck,
  CirclePause,
  CircleX,
  FolderPlus,
  History,
  ListVideo,
  RotateCcw,
  ScanSearch,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
import { useState, type ReactNode } from "react";
import { JobCard, JobCardSkeleton, historyNote } from "@/components/jobs";
import { QueueControls, queueSentence } from "@/components/queue-controls";
import { PageHeader } from "@/components/shell";
import { JobStateBadge } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Callout, Disclosure, EmptyState, Skeleton } from "@/components/ui/display";
import { NavTabs, Pager, useClampedOffset } from "@/components/ui/nav-tabs";
import { ConfirmDialog } from "@/components/ui/overlays";
import { useJobActions } from "@/lib/actions";
import { errorMessage } from "@/lib/api";
import { formatBytes, formatRelative, plural } from "@/lib/format";
import { isUnreadableSource } from "@/lib/outcomes";
import { useActivity, useJobs, useLibraries, useQueueState, useSettings } from "@/lib/queries";
import { href, openSheet, updateParams, type Route } from "@/lib/router";
import type { ActivityLevel, Job } from "@/lib/types";
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
          <li key={job.id} className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-2.5 sm:flex-nowrap sm:gap-4 sm:px-4">
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
            {/* Words, not bare icons: a lone ✕ beside a file name reads as "delete the file". */}
            <div className="flex w-full shrink-0 justify-end gap-1 sm:w-auto">
              {offset + i > 0 ? (
                <Button
                  variant="quiet"
                  size="sm"
                  aria-label={`Move ${job.file_name} to the top`}
                  disabled={moveToTop.isPending}
                  onClick={() => moveToTop.mutate(job)}
                  needsServer
                >
                  <ArrowUpToLine aria-hidden />
                  Move to top
                </Button>
              ) : null}
              <Button
                variant="quiet"
                size="sm"
                aria-label={`Remove ${job.file_name} from the queue`}
                disabled={cancel.isPending}
                onClick={() => cancel.mutate(job)}
                needsServer
              >
                <X aria-hidden />
                Remove
              </Button>
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
        <Button variant="quiet" size="sm" onClick={() => setConfirmClear(true)} needsServer>
          <Trash2 aria-hidden />
          Clear history
        </Button>
      </div>
      <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
        {items.map((job) => {
          // Retrying a damaged original can't help; its sheet offers the fix.
          const retryable =
            job.state === "cancelled" || (job.state === "failed" && !isUnreadableSource(job.error));
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
                  <span className={cn("min-w-0", job.state === "done" ? "text-fg/85" : "text-muted")}>
                    {historyNote(job)}
                  </span>
                  <span className="text-muted">
                    · {libraryName(job.library_id)} · {formatRelative(job.finished_at)}
                  </span>
                </span>
              </button>
              {retryable ? (
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => retry.mutate(job)}
                  disabled={retry.isPending}
                  aria-label={`${job.state === "failed" ? "Try again" : "Queue again"}: ${job.file_name}`}
                  needsServer
                >
                  <RotateCcw aria-hidden />
                  {job.state === "failed" ? "Try again" : "Queue again"}
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

const LOG_ICON: Record<ActivityLevel, ReactNode> = {
  info: <ScanSearch className="text-muted" aria-hidden />,
  success: <CircleCheck className="text-success" aria-hidden />,
  warning: <TriangleAlert className="text-warning" aria-hidden />,
  error: <CircleX className="text-danger" aria-hidden />,
};

/** Scans, warnings and problems as they happened, closed until wanted. */
function Log() {
  const activity = useActivity();
  const items = activity.data?.items.filter((e) => e.level !== "success").slice(0, 30) ?? [];
  if (!items.length) return null;
  return (
    <Disclosure title="Log: scans, warnings and problems" className="mt-8">
      <ol className="flex flex-col gap-2.5">
        {items.map((entry) => (
          <li key={entry.id} className="flex gap-2.5 text-[0.8125rem] [&_svg]:mt-0.5 [&_svg]:size-4 [&_svg]:shrink-0">
            {LOG_ICON[entry.level]}
            <span className="sr-only">{entry.level}:</span>
            <span className="min-w-0 flex-1 text-fg/90">{entry.message}</span>
            <span className="shrink-0 text-xs text-muted">{formatRelative(entry.at)}</span>
          </li>
        ))}
      </ol>
    </Disclosure>
  );
}

export function QueueScreen({ route }: { route: Route }) {
  const tab = route.segments[1] === "next" ? "next" : route.segments[1] === "history" ? "history" : "running";
  const queue = useQueueState();
  const settings = useSettings();
  const history = useJobs({ state: "history", limit: 1 });
  const noLibraries = useNoLibraries();
  const offset = Math.max(0, Number(route.params.get("offset")) || 0);
  const q = queue.data;
  // Pause and Stop only mean something while there's work, or while paused.
  const controls = q && !noLibraries && (q.running > 0 || q.queued > 0 || q.paused);
  return (
    <div>
      <PageHeader
        title="Queue"
        description={q ? (noLibraries ? "Add a library to start converting." : queueSentence(q, settings.data)) : " "}
        actions={controls ? <QueueControls queue={q} /> : null}
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
      {tab === "history" ? (
        <>
          <HistoryTab offset={offset} />
          <Log />
        </>
      ) : null}
    </div>
  );
}
