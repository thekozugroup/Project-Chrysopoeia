"use client";

/**
 * App frame: sidebar with libraries on desktop; a top bar and a bottom tab
 * bar on phones. Also the quiet status pieces that live in the frame: queue
 * state, "Reconnecting…", and the theme switch.
 */

import {
  TriangleAlert,
  CircleCheck,
  CirclePause,
  Clock,
  FolderPlus,
  LayoutDashboard,
  Library as LibraryIcon,
  ListVideo,
  LoaderCircle,
  Monitor,
  Moon,
  ScanSearch,
  Settings as SettingsIcon,
  Sun,
  WifiOff,
} from "lucide-react";
import { useEffect, useId, useRef, type ReactNode } from "react";
import { Brand } from "@/components/brand";
import { finishedPercent } from "@/components/library-bar";
import { Tooltip } from "@/components/ui/overlays";
import { formatHour, formatPercent, plural } from "@/lib/format";
import { useLibraries, useQueueState, useSettings } from "@/lib/queries";
import { href, type Route } from "@/lib/router";
import { useLive, useServerDown, type ConnectionState } from "@/lib/store";
import { setTheme, useTheme, type ThemeChoice } from "@/lib/theme";
import type { Library, QueueState, Settings } from "@/lib/types";
import { cn } from "@/lib/utils";

type Section = "overview" | "queue" | "libraries" | "settings" | "none";

function sectionOf(route: Route): Section {
  const [first] = route.segments;
  if (!first) return "overview";
  if (first === "queue") return "queue";
  if (first === "library" || first === "libraries") return "libraries";
  if (first === "settings") return "settings";
  return "none";
}

/** Whether any library is being scanned (from the library list or live events). */
export function useAnyScanning(): boolean {
  const libraries = useLibraries();
  const liveScan = useLive((s) => Object.values(s.scans).some((scan) => scan.phase !== "done"));
  return liveScan || Boolean(libraries.data?.some((l) => l.scanning));
}

/** Queue state in a few words, e.g. "Converting 2 files" or "Paused". */
export function queueSummary(
  queue: QueueState | undefined,
  settings: Settings | undefined,
  scanning = false,
): { text: string; icon: ReactNode; tone: "active" | "paused" | "idle" | "waiting" } {
  if (!queue) return { text: "Loading…", icon: <Clock aria-hidden />, tone: "idle" };
  if (queue.paused) {
    return {
      text: queue.running ? `Pausing · ${plural(queue.running, "file")} finishing` : "Paused",
      icon: <CirclePause aria-hidden />,
      tone: "paused",
    };
  }
  if (queue.running > 0) {
    return {
      text: `Converting ${plural(queue.running, "file")}`,
      icon: <LoaderCircle className="spin" aria-hidden />,
      tone: "active",
    };
  }
  if (queue.waiting_for_schedule) {
    const hours = settings?.active_hours;
    return {
      text: hours ? `Starts at ${formatHour(hours.start)}` : "Waiting for active hours",
      icon: <Clock aria-hidden />,
      tone: "waiting",
    };
  }
  if (queue.queued > 0) return { text: `${plural(queue.queued, "file")} waiting`, icon: <Clock aria-hidden />, tone: "waiting" };
  if (scanning) return { text: "Scanning", icon: <ScanSearch aria-hidden />, tone: "waiting" };
  return {
    text: settings?.watch_folders === false ? "All caught up" : "Watching for new files",
    icon: <CircleCheck aria-hidden />,
    tone: "idle",
  };
}

function QueuePill({ className }: { className?: string }) {
  const queue = useQueueState();
  const settings = useSettings();
  const scanning = useAnyScanning();
  const connection = useLive((s) => s.connection);
  const polling = useLive((s) => s.polling);
  const serverDown = useLive((s) => s.serverDown);
  const summary = queueSummary(queue.data, settings.data, scanning);
  // Without live updates (and before polling catches up), or while the
  // server can't be reached, this is only the last known state.
  const stale = connection !== "open" && (serverDown || !polling);
  return (
    <a
      href={href("/queue")}
      title={stale ? "Last known state" : undefined}
      className={cn(
        "inline-flex h-8 min-w-0 items-center gap-2 rounded-full px-3 text-[0.8125rem] font-medium whitespace-nowrap no-underline transition-[color,background-color,opacity] pointer-coarse:h-11 [&_svg]:size-4 [&_svg]:shrink-0",
        // Stale: neutral colours and a still icon, so it doesn't look live.
        stale
          ? "bg-raised text-muted hover:text-fg [&_.spin]:animate-none"
          : summary.tone === "active"
            ? "bg-accent-soft text-accent-ink"
            : summary.tone === "paused"
              ? "bg-warning-soft text-warning"
              : "bg-raised text-muted hover:text-fg",
        className,
      )}
    >
      {summary.icon}
      <span className="truncate">{summary.text}</span>
      {stale ? <span className="sr-only"> (last known state)</span> : null}
    </a>
  );
}

const CONNECTION_TEXT: Record<Exclude<ConnectionState, "open">, string> = {
  connecting: "Connecting…",
  reconnecting: "Reconnecting…",
  unavailable: "Live updates off",
};

/**
 * Shown only while the live connection is down. `compact` (phones) keeps
 * just the icon, with the words for screen readers and as a tooltip.
 */
function ConnectionNotice({ className, compact = false }: { className?: string; compact?: boolean }) {
  const connection = useLive((s) => s.connection);
  const polling = useLive((s) => s.polling);
  const serverDown = useLive((s) => s.serverDown);
  // The banner above every screen already says the server is away.
  const bannerShown = useServerDown();
  if (connection === "open" || bannerShown) return null;
  const text = serverDown ? "Server not answering" : CONNECTION_TEXT[connection];
  const detail = serverDown
    ? "Trying again every few seconds."
    : polling
      ? "Refreshing every 5 seconds instead."
      : undefined;
  const icon =
    connection === "connecting" ? <LoaderCircle className="spin" aria-hidden /> : <WifiOff aria-hidden />;
  const body = (
    <p
      role="status"
      tabIndex={compact ? 0 : undefined}
      className={cn(
        "inline-flex items-center gap-1.5 text-[0.8125rem] text-muted [&_svg]:size-3.5 [&_svg]:shrink-0",
        compact && "size-8 justify-center rounded-full bg-raised [&_svg]:size-4",
        className,
      )}
    >
      {icon}
      <span className={compact ? "sr-only" : undefined}>
        {text}
        {detail && !compact ? <span className="block text-xs">{detail}</span> : null}
        {detail && compact ? ` ${detail}` : null}
      </span>
    </p>
  );
  return compact ? <Tooltip content={detail ? `${text}. ${detail}` : text}>{body}</Tooltip> : body;
}

/**
 * A calm note above every screen while the server can't be reached (the
 * container stopped or is restarting). What's on screen stays as it was, and
 * the app reconnects on its own; nothing needs reloading.
 */
function ServerDownBanner() {
  const show = useServerDown();
  return (
    <div role="status" aria-live="polite">
      {show ? (
        <div className="mb-6 flex items-start gap-3 rounded-lg border border-line bg-raised px-4 py-3 md:mb-8">
          <LoaderCircle className="spin mt-0.5 size-4 shrink-0 text-muted" aria-hidden />
          <div className="min-w-0 text-[0.8125rem] leading-relaxed">
            <p className="font-semibold text-fg">Can&apos;t reach Chrysopoeia right now</p>
            <p className="text-muted">
              It may be restarting. You&apos;re seeing the last known state; actions come back when it does, and this
              page reconnects on its own.
            </p>
          </div>
        </div>
      ) : null}
    </div>
  );
}

const THEMES: { value: ThemeChoice; label: string; icon: ReactNode }[] = [
  { value: "system", label: "Match system", icon: <Monitor aria-hidden /> },
  { value: "light", label: "Light", icon: <Sun aria-hidden /> },
  { value: "dark", label: "Dark", icon: <Moon aria-hidden /> },
];

/** System / light / dark, as three small native radios (arrow keys work). */
export function ThemeSwitch({ className }: { className?: string }) {
  const theme = useTheme();
  const name = useId();
  return (
    <div role="radiogroup" aria-label="Appearance" className={cn("inline-flex rounded-md bg-raised p-0.5", className)}>
      {THEMES.map((t) => (
        <Tooltip key={t.value} content={t.label}>
          <label
            className={cn(
              "grid size-7 cursor-pointer place-items-center rounded-[5px] text-muted transition-colors hover:text-fg has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-1 has-[:focus-visible]:outline-accent-ink pointer-coarse:size-10 [&_svg]:size-4",
              theme === t.value && "bg-surface text-fg shadow-card ring-[1.5px] ring-accent-ink ring-inset",
            )}
          >
            <input
              type="radio"
              name={name}
              value={t.value}
              checked={theme === t.value}
              onChange={() => setTheme(t.value)}
              aria-label={t.label}
              className="sr-only"
            />
            {t.icon}
          </label>
        </Tooltip>
      ))}
    </div>
  );
}

function NavLink({
  to,
  active,
  icon,
  children,
}: {
  to: string;
  active: boolean;
  icon: ReactNode;
  children: ReactNode;
}) {
  return (
    <a
      href={to}
      aria-current={active ? "page" : undefined}
      className={cn(
        "flex h-9 items-center gap-2.5 rounded-md px-2.5 text-sm font-medium no-underline transition-colors [&_svg]:size-[1.125rem] [&_svg]:shrink-0",
        active ? "bg-surface text-fg shadow-card" : "text-muted hover:bg-raised/70 hover:text-fg",
      )}
    >
      <span className={cn(active ? "text-accent-ink" : "")}>{icon}</span>
      <span className="min-w-0 flex-1 truncate">{children}</span>
    </a>
  );
}

function LibraryLink({ library, active }: { library: Library; active: boolean }) {
  const scan = useLive((s) => s.scans[library.id]);
  const scanning = library.scanning || Boolean(scan && scan.phase !== "done");
  let detail: ReactNode;
  if (library.path_error) {
    detail = (
      <span className="inline-flex items-center gap-1 text-warning">
        <TriangleAlert className="size-3.5" aria-hidden />
        Folder missing
      </span>
    );
  } else if (!library.enabled) {
    detail = "Paused";
  } else if (scanning) {
    detail = (
      <span className="inline-flex items-center gap-1">
        <LoaderCircle className="spin size-3.5" aria-hidden />
        Scanning
      </span>
    );
  } else if (library.stats.file_count === 0) {
    detail = "No files yet";
  } else {
    detail = `${formatPercent(finishedPercent(library.stats))} finished`;
  }
  return (
    <a
      href={href(`/library/${library.id}`)}
      aria-current={active ? "page" : undefined}
      className={cn(
        "block rounded-md px-2.5 py-2 no-underline transition-colors",
        active ? "bg-surface shadow-card" : "hover:bg-raised/70",
      )}
    >
      <span className={cn("block truncate text-sm font-medium", active ? "text-fg" : "text-fg/90")}>
        {library.name}
      </span>
      <span className="mt-0.5 block text-xs text-muted tabular">{detail}</span>
    </a>
  );
}

function Sidebar({ route }: { route: Route }) {
  const section = sectionOf(route);
  const libraries = useLibraries();
  const activeLibrary = route.segments[0] === "library" ? route.segments[1] : undefined;
  return (
    <aside
      aria-label="Main"
      className="sticky top-0 hidden h-dvh w-60 shrink-0 flex-col border-r border-line bg-sunken md:flex"
    >
      <div className="px-5 pt-5 pb-4">
        <a href={href("/")} className="no-underline" aria-label="Chrysopoeia overview">
          <Brand />
        </a>
      </div>
      <nav aria-label="Sections" className="flex flex-col gap-0.5 px-3">
        <NavLink to={href("/")} active={section === "overview"} icon={<LayoutDashboard />}>
          Overview
        </NavLink>
        {/* The status pill at the bottom says how many are converting. */}
        <NavLink to={href("/queue")} active={section === "queue"} icon={<ListVideo />}>
          Queue
        </NavLink>
      </nav>
      <div className="mt-6 flex min-h-0 flex-1 flex-col px-3">
        <p className="px-2.5 pb-1.5 text-xs font-semibold text-muted">Libraries</p>
        <nav aria-label="Libraries" className="-mx-1 flex min-h-0 flex-col gap-0.5 overflow-y-auto px-1">
          {libraries.data?.map((library) => (
            <LibraryLink key={library.id} library={library} active={library.id === activeLibrary} />
          ))}
          {libraries.isPending ? (
            <div className="space-y-2 px-2.5 py-2">
              <div className="skeleton h-3.5 w-28" />
              <div className="skeleton h-3 w-16" />
            </div>
          ) : null}
          <a
            href={href("/libraries/new")}
            aria-current={route.raw.startsWith("/libraries/new") ? "page" : undefined}
            className={cn(
              "mt-0.5 flex h-9 items-center gap-2.5 rounded-md px-2.5 text-sm font-medium text-muted no-underline transition-colors hover:bg-raised/70 hover:text-fg [&_svg]:size-[1.125rem]",
              route.raw.startsWith("/libraries/new") && "bg-surface text-fg shadow-card",
            )}
          >
            <FolderPlus aria-hidden />
            Add library
          </a>
        </nav>
      </div>
      <div className="flex flex-col gap-3 border-t border-line px-3 pt-3 pb-4">
        <div className="flex items-center gap-2">
          <div className="min-w-0 flex-1">
            <NavLink to={href("/settings")} active={section === "settings"} icon={<SettingsIcon />}>
              Settings
            </NavLink>
          </div>
          <ThemeSwitch />
        </div>
        <QueuePill className="w-full justify-start" />
        <ConnectionNotice className="px-1.5" />
      </div>
    </aside>
  );
}

function MobileTopBar() {
  return (
    <header className="sticky top-0 z-30 flex h-14 items-center justify-between gap-3 border-b border-line bg-bg/95 px-4 backdrop-blur-sm md:hidden">
      <a href={href("/")} className="no-underline" aria-label="Chrysopoeia overview">
        <Brand className="[&_svg]:size-6 [&>span:last-child]:text-xl" />
      </a>
      <div className="flex min-w-0 items-center gap-2">
        <ConnectionNotice compact />
        <QueuePill className="max-w-48" />
      </div>
    </header>
  );
}

function MobileTabBar({ route }: { route: Route }) {
  const section = sectionOf(route);
  const tabs: { key: Section; to: string; label: string; icon: ReactNode }[] = [
    { key: "overview", to: href("/"), label: "Overview", icon: <LayoutDashboard aria-hidden /> },
    { key: "queue", to: href("/queue"), label: "Queue", icon: <ListVideo aria-hidden /> },
    { key: "libraries", to: href("/libraries"), label: "Libraries", icon: <LibraryIcon aria-hidden /> },
    { key: "settings", to: href("/settings"), label: "Settings", icon: <SettingsIcon aria-hidden /> },
  ];
  return (
    <nav
      aria-label="Sections"
      className="fixed inset-x-0 bottom-0 z-30 grid grid-cols-4 border-t border-line bg-sunken/95 pb-[env(safe-area-inset-bottom)] backdrop-blur-sm md:hidden"
    >
      {tabs.map((tab) => {
        const active = section === tab.key;
        return (
          <a
            key={tab.key}
            href={tab.to}
            aria-current={active ? "page" : undefined}
            className={cn(
              "flex h-16 flex-col items-center justify-center gap-1 text-xs font-medium no-underline [&_svg]:size-5",
              active ? "text-accent-ink" : "text-muted",
            )}
          >
            {tab.icon}
            {tab.label}
          </a>
        );
      })}
    </nav>
  );
}

/** The frame around every screen except first-run setup. */
export function Shell({ route, children }: { route: Route; children: ReactNode }) {
  // Hash navigation keeps the old scroll position and focus. On a new screen,
  // start at the top and move focus to the content so screen readers follow.
  const screen = route.segments.join("/");
  const previous = useRef(screen);
  useEffect(() => {
    if (previous.current === screen) return;
    previous.current = screen;
    window.scrollTo({ top: 0 });
    document.getElementById("main")?.focus({ preventScroll: true });
  }, [screen]);

  return (
    <div className="flex min-h-dvh">
      <a
        href="#main"
        onClick={(e) => {
          e.preventDefault();
          document.getElementById("main")?.focus();
        }}
        className="sr-only-focusable fixed top-2 left-2 z-50 rounded-md bg-accent px-3 py-2 text-sm font-medium text-on-accent"
      >
        Skip to content
      </a>
      <Sidebar route={route} />
      <div className="flex min-w-0 flex-1 flex-col">
        <MobileTopBar />
        <main
          id="main"
          tabIndex={-1}
          className="mx-auto w-full max-w-[78rem] flex-1 px-4 pt-6 pb-28 outline-none sm:px-6 md:px-10 md:pt-9 md:pb-16"
        >
          <ServerDownBanner />
          {children}
        </main>
      </div>
      <MobileTabBar route={route} />
    </div>
  );
}

/** Screen title with optional description and actions. */
export function PageHeader({
  title,
  description,
  actions,
  children,
  inlineActions = false,
}: {
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  /** Keep a small action (a "…" menu) beside the title on phones too. */
  inlineActions?: boolean;
}) {
  return (
    <header
      className={cn(
        "mb-7 flex gap-4 md:mb-9 md:flex-row md:items-end md:justify-between",
        inlineActions ? "flex-row items-start justify-between" : "flex-col",
      )}
    >
      <div className="min-w-0">
        <h1 className="font-display text-[2rem] leading-[1.1] text-fg md:text-[2.5rem]">{title}</h1>
        {description ? <div className="mt-2 max-w-2xl text-sm leading-relaxed text-muted">{description}</div> : null}
        {children}
      </div>
      {actions ? <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div> : null}
    </header>
  );
}
