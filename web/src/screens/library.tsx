"use client";

/**
 * One library: its progress, the file table (search, status filter, sort,
 * pages, bulk actions) and its settings (goal, quality, advanced, rename,
 * pause, remove).
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  CirclePause,
  CirclePlay,
  CircleMinus,
  Ellipsis,
  FolderSearch,
  LoaderCircle,
  Play,
  RefreshCw,
  RotateCcw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { LibraryBar, LibraryLegend } from "@/components/library-bar";
import { ProfileEditor } from "@/components/profile-editor";
import { SaveBar } from "@/components/save-bar";
import { PageHeader } from "@/components/shell";
import { FileStatusBadge, fileStatusIcon } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/controls";
import { Badge, Callout, EmptyState, Skeleton } from "@/components/ui/display";
import { NavTabs, Pager, useClampedOffset } from "@/components/ui/nav-tabs";
import { ActionMenu, ConfirmDialog } from "@/components/ui/overlays";
import { useFileActions } from "@/lib/actions";
import { ApiError, api, errorMessage } from "@/lib/api";
import { formatBytes, formatCount, formatRelative, plural } from "@/lib/format";
import { FILE_STATUS_HELP, FILE_STATUS_LABEL, GOAL_LABEL, sourceCodecLabel } from "@/lib/labels";
import { sameProfile } from "@/lib/profile";
import { keys, useFiles, useHardwareInfo, useLibrary, usePresets, useSettings } from "@/lib/queries";
import { href, navigate, openSheet, updateParams, type Route } from "@/lib/router";
import { useFileLive, useLive } from "@/lib/store";
import type { FileSort, FileStatus, Library, MediaFile } from "@/lib/types";
import { FILE_STATUSES } from "@/lib/types";
import { cn } from "@/lib/utils";

const PAGE = 100;

const SORTS: { value: FileSort; label: string }[] = [
  { value: "name", label: "Name" },
  { value: "-size", label: "Largest first" },
  { value: "size", label: "Smallest first" },
  { value: "-updated", label: "Recently changed" },
  { value: "status", label: "Status" },
];

function useLibraryActions(library: Library) {
  const client = useQueryClient();
  const refreshLibrary = (updated?: Library) => {
    if (updated) {
      client.setQueryData<Library[]>(keys.libraries, (old) => old?.map((l) => (l.id === updated.id ? updated : l)));
    }
    void client.invalidateQueries({ queryKey: keys.libraries });
  };

  const scan = useMutation({
    mutationFn: () => api.scanLibrary(library.id),
    onSuccess: () => {
      toast(`Scanning ${library.name}`, { description: "New and changed files will appear as they're found." });
      refreshLibrary();
    },
    onError: (err) => {
      if (err instanceof ApiError && err.status === 409) toast("Already scanning", { description: err.message });
      else toast.error("Couldn't start a scan", { description: errorMessage(err) });
    },
  });

  const setEnabled = useMutation({
    mutationFn: (enabled: boolean) => api.updateLibrary(library.id, { enabled }),
    onSuccess: (updated) => {
      refreshLibrary(updated);
      toast(updated.enabled ? `${updated.name} resumed` : `${updated.name} paused`, {
        description: updated.enabled
          ? "It will be scanned and converted again."
          : "It won't be scanned or converted until you resume it.",
      });
    },
    onError: (err) => toast.error("Couldn't change that", { description: errorMessage(err) }),
  });

  const remove = useMutation({
    mutationFn: () => api.deleteLibrary(library.id),
    onSuccess: () => {
      client.setQueryData<Library[]>(keys.libraries, (old) => old?.filter((l) => l.id !== library.id));
      void client.invalidateQueries({ queryKey: keys.overview });
      void client.invalidateQueries({ queryKey: keys.jobs() });
      toast(`Removed ${library.name}`, { description: "Your media files were not touched." });
      navigate("/", { replace: true });
    },
    onError: (err) => toast.error("Couldn't remove the library", { description: errorMessage(err) }),
  });

  return { scan, setEnabled, remove };
}

function ScanBanner({ library }: { library: Library }) {
  const scan = useLive((s) => s.scans[library.id]);
  if (!library.scanning && (!scan || scan.phase === "done")) return null;
  const text = scan
    ? scan.phase === "discovering"
      ? `Looking for video files… ${formatCount(scan.discovered)} found so far.`
      : `Analysing new and changed files: ${formatCount(scan.analyzed)} of ${formatCount(scan.to_analyze)}.`
    : "Scanning for new and changed files…";
  return (
    <p role="status" className="mt-4 flex items-center gap-2 text-[0.8125rem] text-muted">
      <LoaderCircle className="spin size-4 text-accent-ink" aria-hidden />
      {text}
    </p>
  );
}

function Header({ library }: { library: Library }) {
  const { scan, setEnabled, remove } = useLibraryActions(library);
  const [confirmRemove, setConfirmRemove] = useState(false);
  return (
    <>
      <PageHeader
        title={library.name}
        actions={
          <>
            <Button
              variant="secondary"
              onClick={() => scan.mutate()}
              loading={scan.isPending}
              disabled={library.scanning || !library.enabled}
            >
              <RefreshCw aria-hidden />
              Scan now
            </Button>
            <ActionMenu
              trigger={
                <Button variant="secondary" size="icon" aria-label={`More actions for ${library.name}`}>
                  <Ellipsis />
                </Button>
              }
              actions={[
                library.enabled
                  ? {
                      label: "Pause library",
                      icon: <CirclePause aria-hidden />,
                      onSelect: () => setEnabled.mutate(false),
                    }
                  : {
                      label: "Resume library",
                      icon: <CirclePlay aria-hidden />,
                      onSelect: () => setEnabled.mutate(true),
                    },
                "separator",
                {
                  label: "Remove library…",
                  icon: <Trash2 aria-hidden />,
                  destructive: true,
                  onSelect: () => setConfirmRemove(true),
                },
              ]}
            />
          </>
        }
      >
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <span className="min-w-0 truncate font-mono text-[0.8125rem] text-muted">{library.path}</span>
          <Badge tone="accent">{GOAL_LABEL[library.profile.goal]}</Badge>
          {!library.enabled ? <Badge icon={<CirclePause aria-hidden />}>Paused</Badge> : null}
          {library.last_scan_at ? (
            <span className="text-xs text-muted">Scanned {formatRelative(library.last_scan_at)}</span>
          ) : null}
        </div>
      </PageHeader>
      <RemoveLibraryDialog
        library={library}
        open={confirmRemove}
        onOpenChange={setConfirmRemove}
        onConfirm={() => remove.mutate()}
        loading={remove.isPending}
      />
    </>
  );
}

function RemoveLibraryDialog({
  library,
  open,
  onOpenChange,
  onConfirm,
  loading,
}: {
  library: Library;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
  loading: boolean;
}) {
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={`Remove ${library.name}?`}
      confirmLabel="Remove library"
      destructive
      loading={loading}
      onConfirm={onConfirm}
    >
      <p>
        Chrysopoeia stops watching <span className="font-mono text-[0.8125rem] text-fg">{library.path}</span> and
        forgets its files and history. Anything converting in it is cancelled.
      </p>
      <p className="font-medium text-fg">Your media files are not deleted or changed.</p>
    </ConfirmDialog>
  );
}

function Summary({ library }: { library: Library }) {
  const stats = library.stats;
  return (
    <section aria-label="Progress" className="rounded-lg border border-line bg-surface p-4 sm:p-5">
      <div className="flex flex-wrap items-baseline justify-between gap-x-6 gap-y-1">
        <p className="text-sm text-fg">
          <span className="font-semibold tabular">{formatCount(stats.done + stats.skipped)}</span> of{" "}
          <span className="tabular">{plural(stats.file_count, "file")}</span> finished
          {stats.skipped > 0 ? (
            <span className="text-muted">
              {" "}
              ({formatCount(stats.done)} converted, {formatCount(stats.skipped)} left as they were)
            </span>
          ) : null}
        </p>
        <p className="text-sm text-muted">
          {stats.saved_bytes > 0 ? (
            <>
              <span className="font-semibold text-accent-ink">{formatBytes(stats.saved_bytes)}</span> saved ·{" "}
            </>
          ) : null}
          {formatBytes(stats.total_bytes)} in total
        </p>
      </div>
      <LibraryBar stats={stats} className="mt-3" />
      <LibraryLegend stats={stats} className="mt-3" />
    </section>
  );
}

/** Debounced value, for the search box. */
function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

function SearchBox({ initial }: { initial: string }) {
  const [text, setText] = useState(initial);
  const [lastInitial, setLastInitial] = useState(initial);
  const debounced = useDebounced(text, 300);
  const first = useRef(true);
  // Filters were cleared elsewhere: follow, unless it is our own debounce echoing back.
  if (initial !== lastInitial) {
    setLastInitial(initial);
    if (initial !== debounced.trim()) setText(initial);
  }
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    updateParams({ q: debounced.trim() || null, offset: null });
  }, [debounced]);
  return (
    <div className="relative min-w-0 flex-1 sm:max-w-xs">
      <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted" aria-hidden />
      <Input
        type="search"
        aria-label="Search files by name or folder"
        placeholder="Search files"
        value={text}
        onChange={(e) => setText(e.target.value)}
        className="pl-9"
      />
    </div>
  );
}

function StatusChips({ library, status }: { library: Library; status: FileStatus | null }) {
  const stats = library.stats;
  const chips: { value: FileStatus | null; label: string; count: number }[] = [
    { value: null, label: "All", count: stats.file_count },
    ...FILE_STATUSES.map((s) => ({ value: s, label: FILE_STATUS_LABEL[s], count: stats[s] })),
  ];
  return (
    <div role="group" aria-label="Filter by status" className="-mx-4 flex gap-1.5 overflow-x-auto px-4 pb-1 sm:mx-0 sm:flex-wrap sm:px-0">
      {chips.map((chip) => {
        const active = chip.value === status;
        if (chip.value && chip.count === 0 && !active) return null;
        return (
          <button
            key={chip.label}
            type="button"
            aria-pressed={active}
            title={chip.value ? FILE_STATUS_HELP[chip.value] : "Every file in this library"}
            onClick={() => updateParams({ status: chip.value, offset: null })}
            className={cn(
              "inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full border px-3 text-[0.8125rem] font-medium transition-colors [&_svg]:size-3.5",
              active
                ? "border-accent-ink bg-accent-soft text-fg"
                : "border-line-strong/50 bg-surface text-muted hover:border-line-strong hover:text-fg",
            )}
          >
            {chip.value ? <span className={cn(active ? "text-accent-ink" : "")}>{fileStatusIcon(chip.value)}</span> : null}
            {chip.label}
            <span className={cn("tabular text-xs", active ? "text-fg" : "text-muted")}>{formatCount(chip.count)}</span>
          </button>
        );
      })}
    </div>
  );
}

function formatLine(file: MediaFile): string {
  return [file.resolution, file.video_codec ? sourceCodecLabel(file.video_codec) : null, file.hdr ? "HDR" : null]
    .filter(Boolean)
    .join(" · ");
}

function SavedCell({ file }: { file: MediaFile }) {
  if (file.saved_bytes === null || file.status !== "done") return <span className="text-muted">—</span>;
  if (file.saved_bytes < 0) return <span className="text-danger">+{formatBytes(-file.saved_bytes)}</span>;
  return <span className="text-accent-ink">{formatBytes(file.saved_bytes)}</span>;
}

/** Status with live whole-file progress while converting (see `overallProgress`). */
function StatusCell({ file }: { file: MediaFile }) {
  const live = useFileLive(file.id);
  return <FileStatusBadge status={file.status} progress={file.status === "processing" ? (live?.overall ?? null) : null} />;
}

function Checkbox({
  checked,
  indeterminate,
  onChange,
  label,
}: {
  checked: boolean;
  indeterminate?: boolean;
  onChange: (checked: boolean) => void;
  label: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = Boolean(indeterminate);
  }, [indeterminate]);
  // On touch screens the label pads the 16px box out to a 44px target.
  return (
    <label className="-m-1 inline-flex cursor-pointer p-1 pointer-coarse:-m-3.5 pointer-coarse:p-3.5">
      <input
        ref={ref}
        type="checkbox"
        aria-label={label}
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="checkbox"
      />
    </label>
  );
}

function openFile(file: MediaFile) {
  openSheet({ file: file.id });
}

function FilesTab({ library, route }: { library: Library; route: Route }) {
  const statusParam = route.params.get("status");
  const status = FILE_STATUSES.includes(statusParam as FileStatus) ? (statusParam as FileStatus) : null;
  const q = route.params.get("q") ?? "";
  const sortParam = route.params.get("sort") as FileSort | null;
  const sort: FileSort = SORTS.some((s) => s.value === sortParam) ? (sortParam as FileSort) : "name";
  const offset = Math.max(0, Number(route.params.get("offset")) || 0);
  const files = useFiles({ library: library.id, status: status ?? undefined, q: q || undefined, sort, limit: PAGE, offset });
  const { bulk } = useFileActions();
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmSkip, setConfirmSkip] = useState(false);
  const items = useMemo(() => files.data?.items ?? [], [files.data]);
  const total = files.data?.total ?? 0;
  useClampedOffset(offset, files.data?.total, PAGE);

  // Selection only covers what is on screen.
  const visibleIds = useMemo(() => new Set(items.map((f) => f.id)), [items]);
  const selection = [...selected].filter((id) => visibleIds.has(id));
  const allSelected = items.length > 0 && selection.length === items.length;

  const toggle = useCallback((id: string, on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);

  const stats = library.stats;
  const filtered = Boolean(status || q);

  let body: ReactNode;
  if (files.isPending) {
    body = (
      <div className="divide-y divide-line rounded-lg border border-line bg-surface">
        {Array.from({ length: 6 }, (_, i) => (
          <div key={i} className="flex items-center gap-4 px-4 py-3.5">
            <Skeleton className="size-4" />
            <Skeleton className="h-4 flex-1" />
            <Skeleton className="hidden h-4 w-16 sm:block" />
            <Skeleton className="h-6 w-20 rounded-full" />
          </div>
        ))}
      </div>
    );
  } else if (files.error) {
    body = (
      <Callout tone="danger" title="Couldn't load the files" action={<Button size="sm" onClick={() => files.refetch()}>Try again</Button>}>
        {errorMessage(files.error)}
      </Callout>
    );
  } else if (items.length === 0 && offset > 0 && total > 0) {
    // Past the last page; the offset is being corrected.
    body = null;
  } else if (items.length === 0) {
    body = filtered ? (
      <EmptyState
        icon={<FolderSearch aria-hidden />}
        title="No files match"
        action={
          <Button variant="secondary" onClick={() => updateParams({ status: null, q: null, offset: null })}>
            Clear filters
          </Button>
        }
      >
        {q ? `Nothing named like “${q}”${status ? ` with status ${FILE_STATUS_LABEL[status].toLowerCase()}` : ""}.` : "No files have this status right now."}
      </EmptyState>
    ) : library.scanning ? (
      <EmptyState icon={<LoaderCircle className="spin" aria-hidden />} title="Scanning this folder">
        Files appear here as they&apos;re found and analysed.
      </EmptyState>
    ) : (
      <EmptyState icon={<FolderSearch aria-hidden />} title="No video files here yet">
        Chrysopoeia looked in <span className="font-mono text-[0.8125rem] text-fg">{library.path}</span> and found no
        videos. Check that the folder is mounted into the container, then scan again.
      </EmptyState>
    );
  } else {
    body = (
      <>
        {/* Wide screens: a table. */}
        <div className="hidden overflow-hidden rounded-lg border border-line bg-surface md:block">
          <table className="w-full table-fixed text-sm">
            <caption className="sr-only">
              Files in {library.name}
              {status ? `, ${FILE_STATUS_LABEL[status]}` : ""}
            </caption>
            <thead>
              <tr className="border-b border-line text-left text-xs text-muted">
                <th scope="col" className="w-11 py-2.5 pl-4">
                  <Checkbox
                    checked={allSelected}
                    indeterminate={selection.length > 0 && !allSelected}
                    onChange={(on) => setSelected(on ? new Set(items.map((f) => f.id)) : new Set())}
                    label="Select all files on this page"
                  />
                </th>
                <th scope="col" className="py-2.5 pr-4 font-medium">
                  Name
                </th>
                <th scope="col" className="w-24 py-2.5 pr-4 text-right font-medium">
                  Size
                </th>
                <th scope="col" className="w-36 py-2.5 pr-4 font-medium lg:w-44">
                  Format
                </th>
                <th scope="col" className="w-40 py-2.5 pr-4 font-medium">
                  Status
                </th>
                <th scope="col" className="w-24 py-2.5 pr-4 text-right font-medium">
                  Saved
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {items.map((file) => {
                const folder = file.relative_path.includes("/")
                  ? file.relative_path.slice(0, file.relative_path.lastIndexOf("/"))
                  : "";
                const isSelected = selected.has(file.id);
                return (
                  <tr key={file.id} className={cn("transition-colors hover:bg-raised/50", isSelected && "bg-accent-soft/30")}>
                    <td className="py-2.5 pl-4">
                      <Checkbox checked={isSelected} onChange={(on) => toggle(file.id, on)} label={`Select ${file.file_name}`} />
                    </td>
                    <td className="py-2.5 pr-4">
                      <button type="button" onClick={() => openFile(file)} className="block max-w-full text-left">
                        <span className="block truncate font-medium text-fg hover:text-accent-ink" title={file.file_name}>
                          {file.file_name}
                        </span>
                        {folder ? <span className="block truncate text-xs text-muted">{folder}</span> : null}
                      </button>
                    </td>
                    <td className="py-2.5 pr-4 text-right text-fg tabular">{formatBytes(file.size_bytes)}</td>
                    <td className="truncate py-2.5 pr-4 text-[0.8125rem] text-muted">{formatLine(file) || "—"}</td>
                    <td className="py-2.5 pr-4">
                      <StatusCell file={file} />
                    </td>
                    <td className="py-2.5 pr-4 text-right tabular">
                      <SavedCell file={file} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        {/* Phones: a list. */}
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface md:hidden">
          {items.map((file) => (
            <li key={file.id} className="flex items-start gap-3 px-3 py-3">
              <span className="pt-0.5">
                <Checkbox
                  checked={selected.has(file.id)}
                  onChange={(on) => toggle(file.id, on)}
                  label={`Select ${file.file_name}`}
                />
              </span>
              <button type="button" onClick={() => openFile(file)} className="min-w-0 flex-1 text-left">
                <span className="block truncate text-sm font-medium text-fg">{file.file_name}</span>
                <span className="mt-0.5 block text-xs text-muted">
                  {formatBytes(file.size_bytes)}
                  {formatLine(file) ? ` · ${formatLine(file)}` : ""}
                </span>
                <span className="mt-1.5 flex items-center gap-2">
                  <StatusCell file={file} />
                  {file.status === "done" && file.saved_bytes && file.saved_bytes > 0 ? (
                    <span className="text-xs text-accent-ink">{formatBytes(file.saved_bytes)} saved</span>
                  ) : null}
                </span>
              </button>
            </li>
          ))}
        </ul>
        <Pager
          offset={offset}
          limit={PAGE}
          total={total}
          onPage={(next) => {
            setSelected(new Set());
            updateParams({ offset: next || null });
          }}
          noun="files"
        />
      </>
    );
  }

  return (
    <div>
      <div className="mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <SearchBox key={library.id} initial={q} />
        <div className="flex items-center gap-2">
          <label htmlFor="file-sort" className="text-[0.8125rem] whitespace-nowrap text-muted">
            Sort by
          </label>
          <Select
            id="file-sort"
            value={sort}
            onChange={(e) => updateParams({ sort: e.target.value === "name" ? null : e.target.value, offset: null })}
            className="w-44"
          >
            {SORTS.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </Select>
        </div>
      </div>

      <StatusChips library={library} status={status} />

      <div className="mt-4 mb-3 flex min-h-9 flex-wrap items-center gap-2">
        {selection.length > 0 ? (
          <>
            <p className="mr-2 text-sm font-medium text-fg tabular" role="status">
              {plural(selection.length, "file")} selected
            </p>
            <Button
              variant="primary"
              size="sm"
              loading={bulk.isPending}
              onClick={() => bulk.mutate({ action: "queue", ids: selection }, { onSuccess: () => setSelected(new Set()) })}
            >
              <Play aria-hidden />
              Convert
            </Button>
            <Button variant="secondary" size="sm" onClick={() => setConfirmSkip(true)} disabled={bulk.isPending}>
              <CircleMinus aria-hidden />
              Skip
            </Button>
            <Button variant="quiet" size="sm" onClick={() => setSelected(new Set())}>
              <X aria-hidden />
              Clear selection
            </Button>
          </>
        ) : (
          <>
            {stats.pending > 0 ? (
              <Button
                variant="primary"
                size="sm"
                loading={bulk.isPending && bulk.variables?.action === "queue"}
                onClick={() => bulk.mutate({ action: "queue", library: library.id, status: "pending" })}
              >
                <Play aria-hidden />
                Add {plural(stats.pending, "file")} to the queue
              </Button>
            ) : null}
            {stats.failed > 0 ? (
              <Button
                variant="secondary"
                size="sm"
                loading={bulk.isPending && bulk.variables?.action === "retry_failed"}
                onClick={() => bulk.mutate({ action: "retry_failed", library: library.id })}
              >
                <RotateCcw aria-hidden />
                Retry {formatCount(stats.failed)} failed
              </Button>
            ) : null}
            {items.length > 0 ? (
              <p className="ml-auto hidden text-xs text-muted md:block">Select files to convert or skip them.</p>
            ) : null}
          </>
        )}
      </div>

      {body}

      <ConfirmDialog
        open={confirmSkip}
        onOpenChange={setConfirmSkip}
        title={`Skip ${plural(selection.length, "file")}?`}
        confirmLabel="Skip files"
        loading={bulk.isPending}
        onConfirm={() =>
          bulk.mutate(
            { action: "skip", ids: selection },
            {
              onSettled: () => setConfirmSkip(false),
              onSuccess: () => setSelected(new Set()),
            },
          )
        }
      >
        <p>They&apos;ll be left as they are and marked “Skipped by you”. Queued ones are taken out of the queue.</p>
        <p>You can convert them later from this list.</p>
      </ConfirmDialog>
    </div>
  );
}

function SettingsTab({ library }: { library: Library }) {
  const client = useQueryClient();
  const presets = usePresets();
  const hardware = useHardwareInfo();
  const settings = useSettings();
  const { remove, setEnabled } = useLibraryActions(library);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [draft, setDraft] = useState(() => ({ name: library.name, profile: library.profile }));
  const [base, setBase] = useState(library);
  const [valid, setValid] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  const [resetKey, setResetKey] = useState(0);

  const dirty = draft.name !== base.name || !sameProfile(draft.profile, base.profile);

  // Follow server changes while the user hasn't edited anything.
  if (library !== base && !dirty) {
    setBase(library);
    setDraft({ name: library.name, profile: library.profile });
  }

  const save = useMutation({
    mutationFn: () =>
      api.updateLibrary(library.id, {
        name: draft.name.trim() !== base.name ? draft.name.trim() : undefined,
        profile: !sameProfile(draft.profile, base.profile) ? draft.profile : undefined,
      }),
    onSuccess: (updated) => {
      client.setQueryData<Library[]>(keys.libraries, (old) => old?.map((l) => (l.id === updated.id ? updated : l)));
      void client.invalidateQueries({ queryKey: keys.files() });
      setBase(updated);
      setDraft({ name: updated.name, profile: updated.profile });
      setError(null);
      toast.success("Saved", {
        description: sameProfile(updated.profile, base.profile)
          ? undefined
          : "Files not yet converted are re-checked against the new settings.",
      });
    },
    onError: (err) => setError(errorMessage(err)),
  });

  const canSave = () => {
    if (!draft.name.trim()) {
      setNameError("Give the library a name.");
      return false;
    }
    return valid;
  };

  const discard = () => {
    setDraft({ name: base.name, profile: base.profile });
    setError(null);
    setNameError(null);
    setResetKey((k) => k + 1);
  };

  return (
    <div>
      <section className="grid gap-6 border-b border-line pb-8 md:grid-cols-[14rem_1fr] md:gap-8">
        <div>
          <h2 className="text-sm font-semibold text-fg">Library</h2>
          <p className="mt-1 text-[0.8125rem] leading-snug text-muted">Its name, and pausing it.</p>
        </div>
        <div className="flex max-w-xl flex-col gap-5">
          <Field label="Name" error={nameError}>
            <Input
              value={draft.name}
              maxLength={80}
              onChange={(e) => {
                setNameError(null);
                setDraft((d) => ({ ...d, name: e.target.value }));
              }}
            />
          </Field>
          {/* Pausing takes effect right away, like the menu action: no save needed. */}
          <div className="flex flex-col items-start gap-3 sm:flex-row sm:items-center sm:justify-between sm:gap-6">
            <div className="min-w-0">
              <p className="text-sm font-medium text-fg">{library.enabled ? "Active" : "Paused"}</p>
              <p className="mt-0.5 text-[0.8125rem] leading-snug text-muted">
                {library.enabled
                  ? "Scanned and converted as usual. Pausing stops both; nothing already converted changes."
                  : "Not scanned or converted until you resume it. Nothing already converted changed."}
              </p>
            </div>
            <Button
              variant="secondary"
              className="shrink-0"
              onClick={() => setEnabled.mutate(!library.enabled)}
              loading={setEnabled.isPending}
            >
              {library.enabled ? <CirclePause aria-hidden /> : <CirclePlay aria-hidden />}
              {library.enabled ? "Pause library" : "Resume library"}
            </Button>
          </div>
        </div>
      </section>

      <div className="pt-8">
        <ProfileEditor
          profile={draft.profile}
          onChange={(profile) => setDraft((d) => ({ ...d, profile }))}
          presets={presets.data}
          hardware={hardware.hw}
          hardwarePending={hardware.pending}
          preference={settings.data?.hardware}
          onValidityChange={setValid}
          resetKey={resetKey}
          headingLevel={2}
        />
      </div>

      <section className="mt-10 grid gap-3 border-t border-line pt-8 md:grid-cols-[14rem_1fr] md:gap-8">
        <div>
          <h2 className="text-sm font-semibold text-fg">Remove library</h2>
        </div>
        <div className="flex flex-col items-start gap-3">
          <p className="max-w-xl text-[0.8125rem] leading-relaxed text-muted">
            Stops watching this folder and forgets its files and history.{" "}
            <span className="font-medium text-fg">Your media files are not deleted.</span>
          </p>
          <Button variant="danger" onClick={() => setConfirmRemove(true)}>
            <Trash2 aria-hidden />
            Remove library…
          </Button>
        </div>
      </section>

      <SaveBar
        dirty={dirty}
        saving={save.isPending}
        onSave={() => {
          if (canSave()) save.mutate();
        }}
        onDiscard={discard}
        error={error ?? (valid ? null : "Fix the highlighted fields to save.")}
        disabled={!valid}
        blocks={(target) =>
          !(target.segments[0] === "library" && target.segments[1] === library.id && target.segments[2] === "settings")
        }
        saveAndLeave={async () => {
          if (!canSave()) return false;
          try {
            await save.mutateAsync();
            return true;
          } catch {
            return false;
          }
        }}
      />
      <RemoveLibraryDialog
        library={library}
        open={confirmRemove}
        onOpenChange={setConfirmRemove}
        onConfirm={() => remove.mutate()}
        loading={remove.isPending}
      />
    </div>
  );
}

export function LibraryScreen({ id, route }: { id: string; route: Route }) {
  const { library, isPending } = useLibrary(id);
  const tab = route.segments[2] === "settings" ? "settings" : "files";

  if (isPending) {
    return (
      <div>
        <Skeleton className="h-10 w-72" />
        <Skeleton className="mt-3 h-4 w-96 max-w-full" />
        <Skeleton className="mt-8 h-24 w-full" />
      </div>
    );
  }
  if (!library) {
    return (
      <EmptyState
        title="This library doesn't exist"
        action={
          <a href={href("/")} className={buttonVariants({ variant: "primary" })}>
            Go to overview
          </a>
        }
      >
        It may have been removed.
      </EmptyState>
    );
  }

  return (
    <div>
      <Header library={library} />
      {library.path_error ? (
        <Callout tone="warning" title="Chrysopoeia can't read this folder" className="mb-6">
          <p>{library.path_error}</p>
          <p className="mt-1">
            Make sure the folder is mapped into the container (for example{" "}
            <code className="font-mono text-xs">-v /mnt/user/media:/media</code>) and readable, then scan again.
          </p>
        </Callout>
      ) : null}
      <Summary library={library} />
      <ScanBanner library={library} />
      <NavTabs
        label="Library views"
        className="mt-8 mb-6"
        tabs={[
          { href: href(`/library/${library.id}`), label: "Files", active: tab === "files", count: library.stats.file_count },
          { href: href(`/library/${library.id}/settings`), label: "Settings", active: tab === "settings" },
        ]}
      />
      {tab === "files" ? <FilesTab library={library} route={route} /> : <SettingsTab key={library.id} library={library} />}
    </div>
  );
}
