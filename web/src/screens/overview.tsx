"use client";

/**
 * Overview: a calm status page. The space saved and one sentence about what
 * is happening; "Needs your attention" only when something needs fixing,
 * grouped by cause with its fix; live cards only while files are
 * converting; then each library and the latest results.
 */

import {
  CircleCheck,
  CirclePause,
  Clock,
  FileClock,
  FileWarning,
  FolderLock,
  FolderX,
  HardDrive,
  Hourglass,
  LoaderCircle,
  MonitorCog,
  RotateCcw,
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
import { useFileActions, useQueueActions } from "@/lib/actions";
import { errorMessage } from "@/lib/api";
import { settlingText } from "@/lib/convertible";
import { formatBytes, formatCount, formatHour, formatPercent, formatRelative, plural, splitBytes } from "@/lib/format";
import { SETUP_PROBLEMS, setupFix, type SetupProblem } from "@/lib/outcomes";
import {
  useFailures,
  useHardwareInfo,
  useJobs,
  useKeptAsConverted,
  useLibraries,
  useOverview,
  useSavedFileCount,
  useSettings,
  useSettling,
  type Failures,
} from "@/lib/queries";
import { href, openSheet } from "@/lib/router";
import { useLive, useServerDown } from "@/lib/store";
import type {
  HardwareInfo,
  Job,
  JobQuery,
  Library,
  LibraryStats,
  Overview,
  QueueState,
  ScanProgress,
  Settings,
} from "@/lib/types";
import { cn } from "@/lib/utils";
import { HardwareLine } from "./setup";

/** "All caught up" → "all caught up", after "Last known:". */
function lowerFirst(text: string): string {
  return text.charAt(0).toLowerCase() + text.slice(1);
}

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
  // While the server is away this is only what it last said: no live icon.
  const down = useServerDown();
  const status = overviewStatus(queue, settings.data, scanSentence(libraries.data ?? [], scans));
  return (
    <div className="mt-5 flex flex-wrap items-center gap-x-3 gap-y-2">
      <p
        role="status"
        className={cn(
          "flex items-center gap-2 text-[1.0625rem] [&_svg]:size-[1.125rem] [&_svg]:shrink-0",
          down ? "text-muted" : "text-fg",
        )}
      >
        {down ? null : status.icon}
        <span>
          {down ? <span>Last known: </span> : null}
          <span className={down ? undefined : "font-medium"}>{down ? lowerFirst(status.text) : status.text}</span>
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

/**
 * "from 12 converted files": the files the space saved comes from (those
 * with `saved_bytes > 0`, including converted files queued again), not
 * every converted file, since one that came out larger saved nothing.
 */
function SavedFrom({ totals }: { totals: LibraryStats }) {
  const count = useSavedFileCount(totals);
  // Counting failed (an unusual server): the converted files are the closest answer.
  const files = count.data ?? (count.error ? totals.done : null);
  if (files === null) return <Skeleton className="inline-block h-3.5 w-36 align-middle" />;
  return <>from {plural(files, "converted file")}</>;
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

  // Savings, not the "converted" count, decide: a converted file queued
  // again keeps its savings but isn't "done" meanwhile.
  if (saved === 0) {
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
        <SavedFrom totals={totals} />
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
  tone: "warning" | "danger" | "info";
  title: string;
  detail: string;
  /** Where to look or fix it. */
  action?: { label: string; href: string };
  /** Failed files to try again, once the cause is fixed. */
  retry?: string[];
}

/**
 * One cause's rows: a single row when its files are all in one library,
 * else one row per library named after it ("Movies: 2 files can't be
 * read"), so every "Review" lands on a list that holds all it counted.
 */
function perLibrary(
  key: string,
  libraries: Library[],
  counts: Record<string, number>,
  row: (n: number) => Pick<Problem, "icon" | "tone" | "title" | "detail">,
): Problem[] {
  const hit = libraries.filter((l) => (counts[l.id] ?? 0) > 0);
  const review = (library: Library) => ({
    label: "Review",
    href: href(`/library/${library.id}`, { status: "failed" }),
  });
  if (hit.length === 1) return [{ key, ...row(counts[hit[0].id]), action: review(hit[0]) }];
  return hit.map((library) => {
    const problem = row(counts[library.id]);
    return { ...problem, key: `${key}-${library.id}`, title: `${library.name}: ${problem.title}`, action: review(library) };
  });
}

const SETUP_ICON: Record<SetupProblem, ReactNode> = {
  hardware_unavailable: <MonitorCog aria-hidden />,
  work_folder: <FolderX aria-hidden />,
  disk_full: <HardDrive aria-hidden />,
  destination: <FolderLock aria-hidden />,
};

/** "3 files couldn't be converted because of it." */
function blockedText(n: number): string {
  return `${plural(n, "file")} couldn't be converted because of it.`;
}

/**
 * Problems the user can fix, one row per cause. Setup problems (work
 * folder, destination, disk space, chosen hardware) are one row each
 * across every library, with the exact fix, a link to the setting and
 * "Try again" for all their files; damaged originals and failed
 * conversions get a row per library with "Review".
 */
export function problemsFrom(
  libraries: Library[],
  failures: Failures,
  hw: HardwareInfo | undefined,
  settings?: Pick<Settings, "output_mode">,
): Problem[] {
  const problems: Problem[] = [];
  const hints = hw?.hints.filter((h) => h.level !== "info") ?? [];
  // Files the chosen hardware couldn't convert belong with the hardware tips.
  const hardwareFailed = failures.ready ? (failures.setup.hardware_unavailable ?? []) : [];
  if (hints.length) {
    const blocking = hints.some((h) => h.level === "error");
    const tips = hints.length > 1 ? `${hints[0].title}, and ${plural(hints.length - 1, "more tip")}.` : `${hints[0].title}.`;
    problems.push({
      key: "hardware",
      icon: <MonitorCog aria-hidden />,
      tone: blocking ? "danger" : "warning",
      title: blocking ? "Nothing can be converted until setup is fixed" : "Hardware setup needs a fix",
      detail: hardwareFailed.length ? `${tips} ${blockedText(hardwareFailed.length)}` : tips,
      action: { label: "Show me", href: href("/settings/hardware") },
      retry: hardwareFailed.length ? hardwareFailed.map((f) => f.id) : undefined,
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
    for (const kind of SETUP_PROBLEMS) {
      const files = failures.setup[kind] ?? [];
      // Already said with the hardware tips.
      if (!files.length || (kind === "hardware_unavailable" && hints.length)) continue;
      const fix = setupFix(kind, settings?.output_mode);
      problems.push({
        key: `setup-${kind}`,
        icon: SETUP_ICON[kind],
        tone: "warning",
        title: fix.title,
        detail: `${blockedText(files.length)} ${fix.fix}`,
        action: { label: fix.setting.label, href: href(fix.setting.path) },
        retry: files.map((f) => f.id),
      });
    }
    const count = (kind: "unreadable" | "conversion") =>
      Object.fromEntries(Object.entries(failures.byLibrary).map(([id, c]) => [id, c[kind]]));
    problems.push(
      ...perLibrary("unreadable", libraries, count("unreadable"), (n) => ({
        icon: <FileWarning aria-hidden />,
        tone: "warning",
        title: `${plural(n, "file")} can't be read`,
        detail: `${n === 1 ? "It looks" : "They look"} damaged or ${n === 1 ? "isn't a video" : "aren't videos"}. Chrysopoeia left ${n === 1 ? "it" : "them"} alone.`,
      })),
      ...perLibrary("failed", libraries, count("conversion"), (n) => ({
        icon: <TriangleAlert aria-hidden />,
        tone: "danger",
        title: `${plural(n, "file")} couldn't be converted`,
        detail: `The ${n === 1 ? "original is" : "originals are"} untouched. See why, then try again.`,
      })),
    );
    const changed = failures.changed;
    if (changed.length) {
      const one = changed.length === 1;
      problems.push({
        key: "changed",
        icon: <FileClock aria-hidden />,
        tone: "info",
        title: `${plural(changed.length, "file")} changed while ${one ? "it was" : "they were"} being converted`,
        detail: `${one ? "It was" : "They were"} being copied or replaced at the time, so nothing was replaced. Try again once ${one ? "it has" : "they have"} finished changing.`,
        retry: changed.map((f) => f.id),
      });
    }
  }
  return problems;
}

/** "Try again" for every file of one cause, once it's fixed. */
function RetryAll({ ids }: { ids: string[] }) {
  const { bulk } = useFileActions();
  return (
    <Button
      variant="secondary"
      size="sm"
      onClick={() => bulk.mutate({ action: "retry_failed", ids })}
      loading={bulk.isPending}
      aria-label={ids.length === 1 ? "Try the file again" : `Try the ${formatCount(ids.length)} files again`}
      needsServer
    >
      <RotateCcw aria-hidden />
      Try again
    </Button>
  );
}

function NeedsAttention() {
  const libraries = useLibraries();
  const failures = useFailures();
  const settings = useSettings();
  const { hw } = useHardwareInfo();
  const problems = problemsFrom(libraries.data ?? [], failures, hw, settings.data);
  if (!problems.length) return null;
  return (
    <section aria-labelledby="attention-heading">
      <SectionHeading id="attention-heading" title="Needs your attention" />
      <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
        {problems.map((p) => {
          // Two actions go under the text on phones, lined up with it.
          const two = Boolean(p.action && p.retry?.length);
          return (
            <li
              key={p.key}
              className={cn(
                "flex items-start gap-x-3 gap-y-2.5 px-4 py-3.5 sm:items-center sm:px-5",
                two && "max-sm:flex-wrap",
              )}
            >
              <span
                className={cn(
                  "mt-0.5 shrink-0 sm:mt-0 [&_svg]:size-[1.125rem]",
                  p.tone === "danger" ? "text-danger" : p.tone === "warning" ? "text-warning" : "text-info",
                )}
              >
                {p.icon}
              </span>
              <div className={cn("min-w-0 flex-1", two && "max-sm:basis-[calc(100%-1.875rem)]")}>
                <p className="text-sm font-medium text-fg">{p.title}</p>
                <p className="mt-0.5 max-w-[30rem] text-[0.8125rem] leading-snug text-muted">{p.detail}</p>
              </div>
              <div className={cn("flex shrink-0 gap-2", two && "max-sm:ml-[1.875rem]")}>
                {p.action ? (
                  <a href={p.action.href} className={buttonVariants({ variant: "secondary", size: "sm" })}>
                    {p.action.label}
                  </a>
                ) : null}
                {p.retry?.length ? <RetryAll ids={p.retry} /> : null}
              </div>
            </li>
          );
        })}
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
  const failures = useFailures();
  const copying = useSettling(library);
  const stats = library.stats;
  const unreadable = failures.byLibrary[library.id]?.unreadable ?? 0;
  const scanning = library.scanning || (scan && scan.phase !== "done");
  const left = remainingCount(stats);
  let status: ReactNode;
  // Files being copied in get their own line, unless that's the whole story.
  let settling = copying > 0 && Boolean(library.enabled && !library.path_error);
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
  } else if (left > 0) {
    status = `${plural(left, "file")} to go`;
  } else if (copying > 0 && stats.failed === 0) {
    status = settlingText(copying);
    settling = false;
  } else if (stats.file_count === 0) {
    status = "No videos found yet";
  } else {
    // Failed files aren't finished; "to review" follows.
    status = stats.failed > 0 ? "Everything else is finished" : "Everything is finished";
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
          <span className="shrink-0 tabular">{formatPercent(finishedPercent(stats, unreadable))} finished</span>
        ) : null}
      </div>
      {settling ? (
        <p className="mt-0.5 flex items-center gap-1.5 text-[0.8125rem] text-muted">
          <Hourglass className="size-3.5 shrink-0 text-accent-ink" aria-hidden />
          {settlingText(copying)}
        </p>
      ) : null}
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

/** The latest results (shared with the layout, which gives them a column only when there are some). */
const RECENT_QUERY: JobQuery = { state: "history", limit: 6 };
const EMPTY_JOBS: Job[] = [];

function RecentResults() {
  const history = useJobs(RECENT_QUERY);
  const libraries = useLibraries();
  const items = history.data?.items ?? EMPTY_JOBS;
  const kept = useKeptAsConverted(items);
  if (!history.isPending && items.length === 0) return null;
  const libraryName = (id: string) => libraries.data?.find((l) => l.id === id)?.name;
  const minSavings = (id: string) => libraries.data?.find((l) => l.id === id)?.profile.min_savings_pct;
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
                  <JobStateBadge job={job} kept={kept.has(job.id)} />
                </span>
                <span className="flex w-full items-baseline justify-between gap-3 text-[0.8125rem]">
                  <span className={cn("min-w-0 truncate", job.state === "done" ? "text-fg/85" : "text-muted")}>
                    {historyNote(job, minSavings(job.library_id), kept.has(job.id))}
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
  const settings = useSettings();
  const problems = problemsFrom(libraries.data ?? [], failures, hardware.hw, settings.data);
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
  const recent = useJobs(RECENT_QUERY);
  const hasRecent = recent.isPending || (recent.data?.items.length ?? 0) > 0;
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
        <div
          className={cn(
            "grid grid-cols-1 gap-10 lg:col-span-2",
            // Before anything has finished, the libraries take the full width.
            hasRecent && "xl:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]",
          )}
        >
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
