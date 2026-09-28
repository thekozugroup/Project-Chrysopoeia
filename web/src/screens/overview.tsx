"use client";

/**
 * Overview: space saved first, then what is converting now, how each library
 * is doing, recent results, and (quietly, at the bottom) what the library is
 * made of.
 */

import { CircleCheck, CircleX, FolderPlus, LoaderCircle, ScanSearch, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";
import { Breakdown, SavingsChart } from "@/components/charts";
import { SetupHints } from "@/components/hints";
import { JobCard, JobCardSkeleton, savingsText } from "@/components/jobs";
import { LibraryBar, LibraryLegend, finishedPercent, remainingCount } from "@/components/library-bar";
import { QueueControls, queueSentence } from "@/components/queue-controls";
import { JobStateBadge } from "@/components/status";
import { buttonVariants } from "@/components/ui/button";
import { Callout, EmptyState, SectionHeading, Skeleton } from "@/components/ui/display";
import { formatBytes, formatCount, formatPercent, formatRelative, plural, splitBytes } from "@/lib/format";
import { sourceCodecLabel } from "@/lib/labels";
import {
  useActivity,
  useHardware,
  useJobs,
  useLibraries,
  useOverview,
  useSettings,
} from "@/lib/queries";
import { href, updateParams } from "@/lib/router";
import { useLive } from "@/lib/store";
import type { ActivityLevel, Job, Library, Overview } from "@/lib/types";
import { cn } from "@/lib/utils";

function SavedHero({ overview }: { overview: Overview }) {
  const { totals, projected_savings_bytes: projected } = overview;
  const saved = Math.max(0, totals.saved_bytes);
  const { value, unit } = splitBytes(saved);
  const remaining = totals.pending + totals.queued + totals.processing;
  let detail: ReactNode;
  if (totals.done === 0) {
    detail =
      totals.file_count === 0
        ? "Once a library is scanned and files are converted, the space you get back shows up here."
        : remaining > 0
          ? `${plural(remaining, "file")} can be converted. The first results appear here as soon as they're verified.`
          : "Nothing in your libraries needs converting right now.";
  } else {
    detail = (
      <>
        from {plural(totals.done, "converted file")}
        {projected && projected > 0 && remaining > 0 ? (
          <>
            . About <span className="font-semibold text-fg">{formatBytes(projected)}</span> more once the remaining{" "}
            {plural(remaining, "file")} {remaining === 1 ? "is" : "are"} done.
          </>
        ) : (
          "."
        )}
      </>
    );
  }
  return (
    <div className="min-w-0">
      <h1 className="sr-only">Overview</h1>
      <p className="flex flex-wrap items-baseline gap-x-3 text-fg">
        <span className="font-display text-[4.25rem] leading-[0.95] tracking-[-0.02em] sm:text-[5.5rem]">
          {saved === 0 ? "0" : value}
          <span className="ml-2 text-[0.5em] tracking-normal text-accent-ink">{saved === 0 ? "GB" : unit}</span>
        </span>
        <span className="text-lg font-medium text-muted">saved</span>
      </p>
      <p className="mt-4 max-w-md text-[0.9375rem] leading-relaxed text-muted">{detail}</p>
      {totals.total_bytes > 0 ? (
        <p className="mt-3 text-[0.8125rem] text-muted">
          Your libraries hold {formatBytes(totals.total_bytes)} across {plural(totals.file_count, "file")}.
        </p>
      ) : null}
    </div>
  );
}

function NowConverting() {
  const overview = useOverview();
  const settings = useSettings();
  const running = useJobs({ state: "running", limit: 12 });
  const queue = overview.data?.queue;
  const jobs = running.data?.items ?? [];
  const open = (job: Job) => updateParams({ job: job.id }, { replace: false });

  let empty: ReactNode = null;
  if (jobs.length === 0 && queue) {
    if (queue.paused) {
      empty = (
        <EmptyState title="Paused" action={<QueueControls queue={queue} />}>
          {queue.queued
            ? `${plural(queue.queued, "file")} waiting. Nothing new starts until you resume.`
            : "Nothing new starts until you resume."}
        </EmptyState>
      );
    } else if (queue.waiting_for_schedule) {
      empty = <EmptyState title="Waiting for active hours">{queueSentence(queue, settings.data)}</EmptyState>;
    } else if (queue.queued > 0) {
      empty = <EmptyState title="Starting soon">{plural(queue.queued, "file")} in the queue.</EmptyState>;
    } else {
      empty = (
        <EmptyState icon={<CircleCheck aria-hidden />} title="All caught up">
          New and changed files are picked up automatically
          {settings.data?.watch_folders ? " as they appear." : " at the next scan."}
        </EmptyState>
      );
    }
  }

  return (
    <section aria-labelledby="now-heading" className="mt-12">
      <SectionHeading
        id="now-heading"
        title="Converting now"
        description={queue ? queueSentence(queue, settings.data) : undefined}
        action={
          queue && (jobs.length > 0 || queue.queued > 0) ? (
            <>
              <a href={href("/queue")} className={buttonVariants({ variant: "quiet", size: "sm" })}>
                Open queue
              </a>
              <QueueControls queue={queue} compact />
            </>
          ) : null
        }
      />
      {running.isPending ? (
        <div className="grid gap-4 lg:grid-cols-2">
          <JobCardSkeleton />
          <JobCardSkeleton />
        </div>
      ) : jobs.length > 0 ? (
        <div className="grid gap-4 lg:grid-cols-2">
          {jobs.map((job, i) => (
            <JobCard key={job.id} job={job} onOpen={open} announce={i === 0} />
          ))}
        </div>
      ) : (
        empty
      )}
    </section>
  );
}

export function LibraryRow({ library }: { library: Library }) {
  const scan = useLive((s) => s.scans[library.id]);
  const stats = library.stats;
  const scanning = library.scanning || (scan && scan.phase !== "done");
  const left = remainingCount(stats);
  let status: ReactNode;
  if (library.path_error) {
    status = <span className="text-warning">{library.path_error}</span>;
  } else if (!library.enabled) {
    status = "Paused — not scanned or converted";
  } else if (scanning) {
    status = (
      <span className="inline-flex items-center gap-1.5">
        <LoaderCircle className="spin size-3.5" aria-hidden />
        {scan
          ? scan.phase === "discovering"
            ? `Looking for files · ${formatCount(scan.discovered)} found`
            : `Analysing ${formatCount(scan.analyzed)} of ${formatCount(scan.to_analyze)}`
          : "Scanning"}
      </span>
    );
  } else if (stats.file_count === 0) {
    status = "No video files found";
  } else {
    status = left > 0 ? `${plural(left, "file")} to go` : "Everything is done";
  }
  return (
    <li>
      <a
        href={href(`/library/${library.id}`)}
        className="group block rounded-lg px-4 py-4 no-underline transition-colors hover:bg-raised/60 sm:px-5"
      >
        <div className="flex items-baseline justify-between gap-4">
          <span className="min-w-0 truncate text-[0.9375rem] font-semibold text-fg group-hover:text-accent-ink">
            {library.name}
          </span>
          <span className="shrink-0 text-sm text-muted tabular">
            {stats.saved_bytes > 0 ? `${formatBytes(stats.saved_bytes)} saved` : null}
          </span>
        </div>
        <div className="mt-0.5 flex items-baseline justify-between gap-4 text-[0.8125rem] text-muted">
          <span className="min-w-0 truncate">{status}</span>
          {stats.file_count > 0 ? (
            <span className="shrink-0 tabular">{formatPercent(finishedPercent(stats))} done</span>
          ) : null}
        </div>
        <LibraryBar stats={stats} className="mt-3" />
      </a>
    </li>
  );
}

function Libraries() {
  const libraries = useLibraries();
  const list = libraries.data ?? [];
  const totals = list.reduce(
    (acc, l) => ({
      file_count: acc.file_count + l.stats.file_count,
      total_bytes: acc.total_bytes + l.stats.total_bytes,
      pending: acc.pending + l.stats.pending,
      queued: acc.queued + l.stats.queued,
      processing: acc.processing + l.stats.processing,
      done: acc.done + l.stats.done,
      skipped: acc.skipped + l.stats.skipped,
      failed: acc.failed + l.stats.failed,
      saved_bytes: acc.saved_bytes + l.stats.saved_bytes,
    }),
    { file_count: 0, total_bytes: 0, pending: 0, queued: 0, processing: 0, done: 0, skipped: 0, failed: 0, saved_bytes: 0 },
  );
  return (
    <section aria-labelledby="libraries-heading">
      <SectionHeading
        id="libraries-heading"
        title="Libraries"
        action={
          <a href={href("/libraries/new")} className={buttonVariants({ variant: "quiet", size: "sm" })}>
            <FolderPlus aria-hidden />
            Add library
          </a>
        }
      />
      {list.length === 0 ? (
        <EmptyState
          icon={<FolderPlus aria-hidden />}
          title="Add your first library"
          action={
            <a href={href("/libraries/new")} className={buttonVariants({ variant: "primary" })}>
              Choose a folder
            </a>
          }
        >
          A library is a folder of videos. Chrysopoeia scans it, converts what&apos;s worth converting, and keeps
          watching for new files.
        </EmptyState>
      ) : (
        <div className="rounded-lg border border-line bg-surface">
          <ul className="divide-y divide-line">
            {list.map((library) => (
              <LibraryRow key={library.id} library={library} />
            ))}
          </ul>
          {totals.file_count > 0 ? (
            <div className="border-t border-line px-4 py-3 sm:px-5">
              <LibraryLegend stats={totals} />
            </div>
          ) : null}
        </div>
      )}
    </section>
  );
}

function RecentResults() {
  const history = useJobs({ state: "history", limit: 6 });
  const items = history.data?.items ?? [];
  return (
    <section aria-labelledby="recent-heading">
      <SectionHeading
        id="recent-heading"
        title="Recently finished"
        action={
          items.length ? (
            <a href={href("/queue/history")} className={buttonVariants({ variant: "quiet", size: "sm" })}>
              See all
            </a>
          ) : null
        }
      />
      {history.isPending ? (
        <div className="space-y-3 rounded-lg border border-line bg-surface p-4">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      ) : items.length === 0 ? (
        <EmptyState title="No results yet">Finished files show up here with their savings and checks.</EmptyState>
      ) : (
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
                    : "Cancelled";
            return (
              <li key={job.id}>
                <button
                  type="button"
                  onClick={() => updateParams({ job: job.id }, { replace: false })}
                  className="flex w-full flex-col gap-1.5 px-4 py-3 text-left transition-colors hover:bg-raised/60"
                >
                  <span className="flex w-full items-center justify-between gap-3">
                    <span className="min-w-0 truncate text-sm font-medium text-fg">{job.file_name}</span>
                    <JobStateBadge job={job} />
                  </span>
                  <span className="flex w-full items-baseline justify-between gap-3 text-[0.8125rem]">
                    <span
                      className={cn(
                        "min-w-0 truncate",
                        job.state === "done" ? "text-fg/85" : job.state === "failed" ? "text-danger" : "text-muted",
                      )}
                    >
                      {note}
                    </span>
                    <span className="shrink-0 text-xs text-muted">{formatRelative(job.finished_at)}</span>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

const ACTIVITY_ICON: Record<ActivityLevel, ReactNode> = {
  info: <ScanSearch className="text-muted" aria-hidden />,
  success: <CircleCheck className="text-success" aria-hidden />,
  warning: <TriangleAlert className="text-warning" aria-hidden />,
  error: <CircleX className="text-danger" aria-hidden />,
};

function Activity() {
  const activity = useActivity();
  const items = activity.data?.items.slice(0, 6) ?? [];
  if (!items.length) return null;
  return (
    <section aria-labelledby="activity-heading">
      <SectionHeading id="activity-heading" title="Activity" />
      <ol className="space-y-2.5">
        {items.map((entry) => (
          <li key={entry.id} className="flex gap-2.5 text-[0.8125rem] [&_svg]:mt-0.5 [&_svg]:size-4 [&_svg]:shrink-0">
            {ACTIVITY_ICON[entry.level]}
            <span className="sr-only">{entry.level}:</span>
            <span className="min-w-0 flex-1 text-fg/90">{entry.message}</span>
            <span className="shrink-0 text-xs text-muted">{formatRelative(entry.at)}</span>
          </li>
        ))}
      </ol>
    </section>
  );
}

function HardwareHints() {
  const hardware = useHardware();
  const hints = hardware.data?.hints.filter((h) => h.level !== "info") ?? [];
  if (!hints.length) return null;
  return (
    <section aria-labelledby="hints-heading" className="mt-10">
      <SectionHeading
        id="hints-heading"
        title="Setup needs attention"
        description="Conversions still work, but they may be slower than they could be."
        action={
          <a href={href("/settings/hardware")} className={buttonVariants({ variant: "quiet", size: "sm" })}>
            Hardware settings
          </a>
        }
      />
      <SetupHints hints={hints.slice(0, 2)} />
    </section>
  );
}

export function OverviewScreen() {
  const overview = useOverview();
  const libraries = useLibraries();
  const data = overview.data;
  const noLibraries = libraries.data !== undefined && libraries.data.length === 0;
  return (
    <div>
      <section className="grid gap-10 lg:grid-cols-[minmax(0,1fr)_minmax(0,26rem)] lg:items-end">
        {data ? (
          <SavedHero overview={data} />
        ) : overview.error ? (
          <Callout tone="danger" title="Couldn't load the overview">
            The rest of the app still works. This page retries on its own.
          </Callout>
        ) : (
          <div>
            <Skeleton className="h-20 w-72" />
            <Skeleton className="mt-5 h-4 w-96 max-w-full" />
          </div>
        )}
        {data && data.savings_history.some((p) => p.saved_bytes > 0) ? (
          <SavingsChart points={data.savings_history} className="rounded-lg border border-line bg-surface p-4" />
        ) : null}
      </section>

      <HardwareHints />

      {noLibraries ? (
        <div className="mt-12">
          <Libraries />
        </div>
      ) : (
        <>
          <NowConverting />
          <div className="mt-12 grid gap-10 xl:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]">
            <Libraries />
            <div className="flex min-w-0 flex-col gap-10">
              <RecentResults />
              <Activity />
            </div>
          </div>
        </>
      )}

      {data && data.totals.file_count > 0 ? (
        <section aria-labelledby="glance-heading" className="mt-14 border-t border-line pt-8">
          <SectionHeading
            id="glance-heading"
            title="What your libraries contain"
            description="By number of files, across every library."
          />
          <div className="grid gap-8 sm:grid-cols-3">
            <Breakdown title="Video" items={data.video_codecs} labelFor={sourceCodecLabel} />
            <Breakdown title="Resolution" items={data.resolutions} />
            <Breakdown title="Audio" items={data.audio_codecs} labelFor={sourceCodecLabel} />
          </div>
        </section>
      ) : null}
    </div>
  );
}
