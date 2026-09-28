"use client";

/**
 * Overview: a calm status page. The space saved and one sentence about what
 * is happening; "Needs your attention" only when something needs fixing,
 * grouped by cause with one action each; live cards only while files are
 * converting; then each library and the latest results.
 */

import {
  CircleCheck,
  CirclePause,
  Clock,
  FileWarning,
  FolderX,
  LoaderCircle,
  MonitorCog,
  TriangleAlert,
} from "lucide-react";
import type { ReactNode } from "react";
import { SavingsChart, worthCharting } from "@/components/charts";
import { JobCard, JobCardSkeleton, historyNote } from "@/components/jobs";
import { LibraryBar, LibraryLegend, finishedPercent, remainingCount } from "@/components/library-bar";
import { QueueControls } from "@/components/queue-controls";
import { JobStateBadge } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Callout, SectionHeading, Skeleton } from "@/components/ui/display";
import { useQueueActions } from "@/lib/actions";
import { errorMessage } from "@/lib/api";
import { settlingCount } from "@/lib/convertible";
import { formatBytes, formatCount, formatHour, formatPercent, formatRelative, plural, splitBytes } from "@/lib/format";
import {
  useActivity,
  useFailures,
  useHardwareInfo,
  useJobs,
  useLibraries,
  useOverview,
  useSettings,
  type Failures,
} from "@/lib/queries";
import { href, openSheet } from "@/lib/router";
import { useLive } from "@/lib/store";
import type { HardwareInfo, Job, Library, Overview, QueueState, ScanProgress, Settings } from "@/lib/types";
import { cn } from "@/lib/utils";
import { HardwareLine } from "./setup";

/** "Looking through Movies: 64 files found so far." for the scans in progress. */
function scanSentence(libraries: Library[], scans: Record<string, ScanProgress>): string | null {
  const scanning = libraries.filter((l) => l.scanning || (scans[l.id] && scans[l.id].phase !== "done"));
  if (!scanning.length) return null;
  const first = scanning[0];
  const scan = scans[first.id];
  const found = scan ? Math.max(scan.discovered, scan.to_analyze) : first.stats.file_count;
  const where =
    scanning.length === 1 ? first.name : `${first.name} and ${plural(scanning.length - 1, "other library", "other libraries")}`;
  if (scan?.phase === "analyzing" && scan.to_analyze > 0) {
    return `Looking through ${where}: checked ${formatCount(scan.analyzed)} of ${plural(scan.to_analyze, "file")}`;
  }
  return found > 0 ? `Looking through ${where}: ${plural(found, "video")} found so far` : `Looking through ${where} for videos`;
}

interface Status {
  icon: ReactNode;
  text: string;
  /** Quiet words after the sentence. */
  detail?: string;
  /** Resume, when paused. */
  resume?: boolean;
}

/** The one status sentence: "Converting 2 files · 3 waiting", "All caught up", "Paused · Resume". */
export function overviewStatus(
  queue: QueueState,
  settings: Settings | undefined,
  scanning: string | null,
): Status {
  const waiting = queue.queued ? `${formatCount(queue.queued)} waiting` : undefined;
  if (queue.paused) {
    return queue.running
      ? { icon: <CirclePause aria-hidden />, text: `Pausing · ${plural(queue.running, "file")} finishing`, resume: true }
      : { icon: <CirclePause aria-hidden />, text: "Paused", detail: waiting, resume: true };
  }
  if (queue.running) {
    return {
      icon: <LoaderCircle className="spin text-accent-ink" aria-hidden />,
      text: `Converting ${plural(queue.running, "file")}`,
      detail: waiting,
    };
  }
  if (queue.waiting_for_schedule) {
    const hours = settings?.active_hours;
    return {
      icon: <Clock aria-hidden />,
      text: hours ? `Starts at ${formatHour(hours.start)}` : "Waiting for active hours",
      detail: waiting,
    };
  }
  if (scanning) return { icon: <LoaderCircle className="spin text-accent-ink" aria-hidden />, text: scanning };
  if (queue.queued) return { icon: <Clock aria-hidden />, text: `${plural(queue.queued, "file")} waiting to start` };
  return {
    icon: <CircleCheck className="text-success" aria-hidden />,
    text: "All caught up",
    detail: settings?.watch_folders === false ? "new files are picked up at the next scan" : "new files are picked up as they appear",
  };
}

function StatusLine({ queue }: { queue: QueueState }) {
  const settings = useSettings();
  const libraries = useLibraries();
  const scans = useLive((s) => s.scans);
  const { resume } = useQueueActions();
  const status = overviewStatus(queue, settings.data, scanSentence(libraries.data ?? [], scans));
  return (
    <div className="mt-5 flex flex-wrap items-center gap-x-3 gap-y-2">
      <p role="status" className="flex items-center gap-2 text-[1.0625rem] text-fg [&_svg]:size-[1.125rem] [&_svg]:shrink-0">
        {status.icon}
        <span>
          <span className="font-medium">{status.text}</span>
          {status.detail ? <span className="text-muted"> · {status.detail}</span> : null}
        </span>
      </p>
      {status.resume ? (
        <Button variant="primary" size="sm" onClick={() => resume.mutate()} loading={resume.isPending} needsServer>
          Resume
        </Button>
      ) : null}
    </div>
  );
}

function SavedHero({ overview }: { overview: Overview }) {
  const { totals, projected_savings_bytes: projected } = overview;
  const libraries = useLibraries();
  const scans = useLive((s) => s.scans);
  const saved = Math.max(0, totals.saved_bytes);
  const { value, unit } = splitBytes(saved);
  const remaining = totals.pending + totals.queued + totals.processing;
  const holdings =
    totals.total_bytes > 0 ? (
      <p className="mt-3 text-[0.8125rem] text-muted">
        Your libraries hold {formatBytes(totals.total_bytes)} across {plural(totals.file_count, "file")}.
      </p>
    ) : null;

  if (totals.done === 0 || saved === 0) {
    // Nothing to report yet: say what is happening instead of a giant "0 GB".
    const scanning = scanSentence(libraries.data ?? [], scans) !== null;
    const title = scanning
      ? "Getting to know your library"
      : remaining > 0
        ? "Your first files are on their way"
        : totals.file_count > 0
          ? "Nothing needs converting"
          : "Waiting for videos";
    const detail = scanning
      ? "Conversions start as soon as files are checked."
      : remaining > 0
        ? "The space you get back shows up here as soon as the first files are verified."
        : totals.file_count > 0
          ? "Every file is already in good shape or was left as it is."
          : "Once a library is scanned and files are converted, the space you get back shows up here.";
    return (
      <div className="min-w-0">
        <h1 className="font-display text-[2.5rem] leading-[1.05] text-fg sm:text-[3.25rem]">{title}</h1>
        <p className="mt-3 max-w-lg text-sm leading-relaxed text-muted">{detail}</p>
        <StatusLine queue={overview.queue} />
        {holdings}
      </div>
    );
  }

  return (
    <div className="min-w-0">
      <h1 className="sr-only">Overview</h1>
      <p className="flex flex-wrap items-baseline gap-x-3 text-fg">
        <span className="font-display text-[4.25rem] leading-[0.95] tracking-[-0.02em] sm:text-[5.5rem]">
          {value}
          <span className="ml-2 text-[0.5em] tracking-normal text-accent-ink">{unit}</span>
        </span>
        <span className="text-lg font-medium text-muted">saved</span>
      </p>
      <p className="mt-3 max-w-md text-sm leading-relaxed text-muted">
        from {plural(totals.done, "converted file")}
        {projected && projected > 0 && remaining > 0 ? (
          <>
            . About <span className="font-semibold text-fg">{formatBytes(projected)}</span> more once the remaining{" "}
            {plural(remaining, "file")} {remaining === 1 ? "is" : "are"} done.
          </>
        ) : (
          "."
        )}
      </p>
      <StatusLine queue={overview.queue} />
      {holdings}
    </div>
  );
}

interface Problem {
  key: string;
  icon: ReactNode;
  tone: "warning" | "danger";
  title: string;
  detail: string;
  action: { label: string; href: string };
}

/** The library with the most of something, to link a grouped problem to. */
function busiest(counts: Record<string, number>): string | undefined {
  return Object.entries(counts)
    .filter(([, n]) => n > 0)
    .sort((a, b) => b[1] - a[1])[0]?.[0];
}

/** Problems the user can fix, one per cause, each with one action. */
export function problemsFrom(
  libraries: Library[],
  failures: Failures,
  hw: HardwareInfo | undefined,
): Problem[] {
  const problems: Problem[] = [];
  const hints = hw?.hints.filter((h) => h.level !== "info") ?? [];
  if (hints.length) {
    const blocking = hints.some((h) => h.level === "error");
    problems.push({
      key: "hardware",
      icon: <MonitorCog aria-hidden />,
      tone: blocking ? "danger" : "warning",
      title: blocking ? "Nothing can be converted until setup is fixed" : "Hardware setup needs a fix",
      detail: hints.length > 1 ? `${hints[0].title}, and ${plural(hints.length - 1, "more tip")}.` : `${hints[0].title}.`,
      action: { label: "Show me", href: href("/settings/hardware") },
    });
  }
  for (const library of libraries.filter((l) => l.path_error)) {
    problems.push({
      key: `folder-${library.id}`,
      icon: <FolderX aria-hidden />,
      tone: "warning",
      title: `${library.name}: the folder can't be read`,
      detail: library.path_error ?? "",
      action: { label: "Show me", href: href(`/library/${library.id}`) },
    });
  }
  if (failures.ready) {
    const unreadable = failures.total.unreadable;
    if (unreadable > 0) {
      const target = busiest(Object.fromEntries(Object.entries(failures.byLibrary).map(([id, c]) => [id, c.unreadable])));
      problems.push({
        key: "unreadable",
        icon: <FileWarning aria-hidden />,
        tone: "warning",
        title: `${plural(unreadable, "file")} can't be read`,
        detail: `${unreadable === 1 ? "It looks" : "They look"} damaged or ${unreadable === 1 ? "isn't a video" : "aren't videos"}. Chrysopoeia left ${unreadable === 1 ? "it" : "them"} alone.`,
        action: { label: "Review", href: href(`/library/${target ?? ""}`, { status: "failed" }) },
      });
    }
    const failed = failures.total.conversion;
    if (failed > 0) {
      const target = busiest(Object.fromEntries(Object.entries(failures.byLibrary).map(([id, c]) => [id, c.conversion])));
      problems.push({
        key: "failed",
        icon: <TriangleAlert aria-hidden />,
        tone: "danger",
        title: `${plural(failed, "file")} couldn't be converted`,
        detail: `The ${failed === 1 ? "original is" : "originals are"} untouched. See why, then try again.`,
        action: { label: "Review", href: href(`/library/${target ?? ""}`, { status: "failed" }) },
      });
    }
  }
  return problems;
}

function NeedsAttention() {
  const libraries = useLibraries();
  const failures = useFailures();
  const { hw } = useHardwareInfo();
  const problems = problemsFrom(libraries.data ?? [], failures, hw);
  if (!problems.length) return null;
  return (
    <section aria-labelledby="attention-heading">
      <SectionHeading id="attention-heading" title="Needs your attention" />
      <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
        {problems.map((p) => (
          <li key={p.key} className="flex items-start gap-3 px-4 py-3.5 sm:items-center sm:px-5">
            <span
              className={cn(
                "mt-0.5 shrink-0 sm:mt-0 [&_svg]:size-[1.125rem]",
                p.tone === "danger" ? "text-danger" : "text-warning",
              )}
            >
              {p.icon}
            </span>
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium text-fg">{p.title}</p>
              <p className="mt-0.5 text-[0.8125rem] leading-snug text-muted">{p.detail}</p>
            </div>
            <a href={p.action.href} className={cn(buttonVariants({ variant: "secondary", size: "sm" }), "shrink-0")}>
              {p.action.label}
            </a>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Live cards, only while files are converting. */
function NowConverting({ queue }: { queue: QueueState }) {
  const running = useJobs({ state: "running", limit: 12 });
  const jobs = running.data?.items ?? [];
  const open = (job: Job) => openSheet({ job: job.id });
  if (queue.running === 0 && jobs.length === 0) return null;
  return (
    <section aria-labelledby="now-heading">
      <SectionHeading
        id="now-heading"
        title="Converting now"
        action={
          <>
            <a href={href("/queue")} className={buttonVariants({ variant: "quiet", size: "sm" })}>
              Open queue
            </a>
            <QueueControls queue={queue} compact />
          </>
        }
      />
      {running.error && !running.data ? (
        <Callout tone="danger" title="Couldn't load what's converting">
          {errorMessage(running.error)} This retries on its own.
        </Callout>
      ) : jobs.length > 0 ? (
        <div className="grid gap-4 lg:grid-cols-2">
          {jobs.map((job, i) => (
            <JobCard key={job.id} job={job} onOpen={open} announce={i === 0} />
          ))}
        </div>
      ) : (
        // A job just started; its card arrives with the next list refresh.
        <div className="grid gap-4 lg:grid-cols-2" aria-busy="true" aria-label="Loading what's converting">
          <JobCardSkeleton />
        </div>
      )}
    </section>
  );
}

export function LibraryRow({ library }: { library: Library }) {
  const scan = useLive((s) => s.scans[library.id]);
  const activity = useActivity();
  const failures = useFailures();
  const copying = settlingCount(library, activity.data?.items);
  const stats = library.stats;
  const unreadable = failures.byLibrary[library.id]?.unreadable ?? 0;
  const scanning = library.scanning || (scan && scan.phase !== "done");
  const left = remainingCount(stats);
  let status: ReactNode;
  if (library.path_error) {
    status = <span className="text-warning">{library.path_error}</span>;
  } else if (!library.enabled) {
    status = "Paused. Not scanned or converted.";
  } else if (scanning) {
    status = (
      <span className="inline-flex items-center gap-1.5">
        <LoaderCircle className="spin size-3.5" aria-hidden />
        {scan
          ? scan.phase === "discovering"
            ? `Looking for videos · ${formatCount(scan.discovered)} found`
            : `Checking ${formatCount(scan.analyzed)} of ${formatCount(scan.to_analyze)}`
          : "Scanning"}
      </span>
    );
  } else if (stats.file_count === 0) {
    status = copying > 0 ? `Waiting for ${plural(copying, "file")} to finish copying` : "No videos found yet";
  } else if (left > 0) {
    status = `${plural(left, "file")} to go${copying > 0 ? ` · ${formatCount(copying)} still copying` : ""}`;
  } else if (copying > 0) {
    status = `Waiting for ${plural(copying, "file")} to finish copying`;
  } else {
    status = "Everything is finished";
  }
  return (
    <li className="group relative px-4 py-4 transition-colors hover:bg-raised/60 sm:px-5">
      <div className="flex items-baseline justify-between gap-4">
        {/* The name's link covers the whole row; "to review" sits above it. */}
        <a
          href={href(`/library/${library.id}`)}
          className="min-w-0 truncate text-[0.9375rem] font-semibold text-fg no-underline outline-none group-hover:text-accent-ink after:absolute after:inset-0 after:rounded-lg focus-visible:after:ring-2 focus-visible:after:ring-accent-ink focus-visible:after:ring-inset"
        >
          {library.name}
        </a>
        <span className="shrink-0 text-sm text-muted tabular">
          {stats.saved_bytes > 0 ? `${formatBytes(stats.saved_bytes)} saved` : null}
        </span>
      </div>
      <div className="mt-0.5 flex items-baseline justify-between gap-4 text-[0.8125rem] text-muted">
        <span className="min-w-0 truncate">
          {status}
          {stats.failed > 0 ? (
            <>
              {" · "}
              <a
                href={href(`/library/${library.id}`, { status: "failed" })}
                className="relative z-10 text-fg underline decoration-line-strong underline-offset-2 hover:text-accent-ink"
              >
                {formatCount(stats.failed)} to review
              </a>
            </>
          ) : null}
        </span>
        {stats.file_count > 0 ? (
          <span className="shrink-0 tabular">{formatPercent(finishedPercent(stats))} finished</span>
        ) : null}
      </div>
      <LibraryBar stats={stats} unreadable={unreadable} className="mt-3" />
    </li>
  );
}

function Libraries() {
  const libraries = useLibraries();
  const failures = useFailures();
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
            Add library
          </a>
        }
      />
      <div className="rounded-lg border border-line bg-surface">
        <ul className="divide-y divide-line">
          {list.map((library) => (
            <LibraryRow key={library.id} library={library} />
          ))}
        </ul>
        {totals.file_count > 0 ? (
          <div className="hidden border-t border-line px-4 py-3 sm:block sm:px-5">
            <LibraryLegend stats={totals} unreadable={failures.total.unreadable} />
          </div>
        ) : null}
      </div>
    </section>
  );
}

function RecentResults() {
  const history = useJobs({ state: "history", limit: 6 });
  const libraries = useLibraries();
  const items = history.data?.items ?? [];
  if (!history.isPending && items.length === 0) return null;
  const libraryName = (id: string) => libraries.data?.find((l) => l.id === id)?.name;
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
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
          {items.map((job) => (
            <li key={job.id}>
              <button
                type="button"
                onClick={() => openSheet({ job: job.id })}
                className="flex w-full flex-col gap-1.5 px-4 py-3 text-left transition-colors hover:bg-raised/60"
              >
                <span className="flex w-full items-center justify-between gap-3">
                  <span className="min-w-0 truncate text-sm font-medium text-fg">{job.file_name}</span>
                  <JobStateBadge job={job} />
                </span>
                <span className="flex w-full items-baseline justify-between gap-3 text-[0.8125rem]">
                  <span className={cn("min-w-0 truncate", job.state === "done" ? "text-fg/85" : "text-muted")}>
                    {historyNote(job)}
                  </span>
                  <span className="shrink-0 text-xs text-muted">
                    {[libraryName(job.library_id), formatRelative(job.finished_at)].filter(Boolean).join(" · ")}
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Live updates can't reach this browser: say why, once, where it's seen. */
function LiveUpdatesOff() {
  const connection = useLive((s) => s.connection);
  if (connection !== "unavailable") return null;
  return (
    <Callout tone="info" title="Live updates aren't reaching this browser" className="mb-8">
      <p>
        The page refreshes every 5 seconds instead, so progress moves in steps. If you open Chrysopoeia through a
        reverse proxy (Nginx Proxy Manager, SWAG, Traefik), turn on WebSocket support for it.
      </p>
    </Callout>
  );
}

/**
 * No library yet (setup was skipped): the welcome's promise again, with the
 * one action that matters.
 */
function Welcome() {
  const hardware = useHardwareInfo();
  const libraries = useLibraries();
  const failures = useFailures();
  const problems = problemsFrom(libraries.data ?? [], failures, hardware.hw);
  return (
    <div className="max-w-2xl">
      <h1 className="font-display text-[2.75rem] leading-[1.05] text-fg sm:text-[3.5rem]">
        Make your video library smaller, safely.
      </h1>
      <p className="mt-5 max-w-xl text-sm leading-relaxed text-muted">
        Point Chrysopoeia at a folder and choose a goal. It converts files in the background, checks each result
        against the original, and only then replaces it.
      </p>
      <a href={href("/libraries/new")} className={cn(buttonVariants({ variant: "primary", size: "lg" }), "mt-8")}>
        Choose a folder
      </a>
      <div className="mt-5 text-[0.8125rem] text-muted">
        <HardwareLine hw={hardware.hw} detecting={hardware.detecting} />
      </div>
      {problems.length ? (
        <div className="mt-12">
          <NeedsAttention />
        </div>
      ) : null}
    </div>
  );
}

export function OverviewScreen() {
  const overview = useOverview();
  const libraries = useLibraries();
  const data = overview.data;
  if (libraries.data !== undefined && libraries.data.length === 0) {
    return (
      <div>
        <LiveUpdatesOff />
        <Welcome />
      </div>
    );
  }
  const chart = data && worthCharting(data.savings_history);
  return (
    <div>
      <LiveUpdatesOff />
      <div className="flex flex-col gap-12 lg:grid lg:grid-cols-[minmax(0,1fr)_minmax(0,26rem)] lg:gap-x-10">
        <div className={cn("lg:col-start-1 lg:row-start-1", !chart && "lg:col-span-2")}>
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
        </div>
        <div className="empty:hidden lg:col-span-2">
          <NeedsAttention />
        </div>
        {data ? (
          <div className="empty:hidden lg:col-span-2">
            <NowConverting queue={data.queue} />
          </div>
        ) : null}
        <div className="grid grid-cols-1 gap-10 lg:col-span-2 xl:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]">
          <Libraries />
          <RecentResults />
        </div>
        {/* Last on phones (after the libraries); beside the saved number on wide screens. */}
        {chart ? (
          <SavingsChart
            points={data.savings_history}
            className="rounded-lg border border-line bg-surface p-4 lg:col-start-2 lg:row-start-1 lg:self-end"
          />
        ) : null}
      </div>
    </div>
  );
}
