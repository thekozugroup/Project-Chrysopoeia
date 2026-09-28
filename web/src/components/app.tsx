"use client";

/**
 * Root of the UI: decides between first-run setup, the "can't reach the
 * server" screen and the normal app, and maps hash routes to screens.
 */

import { RefreshCw, ServerCrash } from "lucide-react";
import { Component, useEffect, type ErrorInfo, type ReactNode } from "react";
import { Brand } from "@/components/brand";
import { FileSheet } from "@/components/files";
import { JobSheet } from "@/components/jobs";
import { NavigationPrompt } from "@/components/save-bar";
import { Shell } from "@/components/shell";
import { Button, buttonVariants } from "@/components/ui/button";
import { CodeBlock, EmptyState, Skeleton } from "@/components/ui/display";
import { ApiError, apiBase, errorMessage, isUuid } from "@/lib/api";
import { useLibraries, useSettings } from "@/lib/queries";
import { closeSheet, href, navigate, useRoute, type Route } from "@/lib/router";
import { AddLibraryScreen } from "@/screens/add-library";
import { LibrariesScreen } from "@/screens/libraries";
import { LibraryScreen } from "@/screens/library";
import { OverviewScreen } from "@/screens/overview";
import { QueueScreen } from "@/screens/queue";
import { SettingsScreen } from "@/screens/settings";
import { SetupScreen } from "@/screens/setup";

/** Catches render errors so one broken view never blanks the whole app. */
class ErrorBoundary extends Component<{ children: ReactNode; resetKey: string }, { error: Error | null }> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Chrysopoeia UI error", error, info.componentStack);
  }

  componentDidUpdate(prev: { resetKey: string }) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) this.setState({ error: null });
  }

  render() {
    if (this.state.error) {
      return (
        <EmptyState
          icon={<ServerCrash aria-hidden />}
          title="This view ran into a problem"
          action={
            <Button variant="primary" onClick={() => window.location.reload()}>
              <RefreshCw aria-hidden />
              Reload
            </Button>
          }
        >
          <p>Your files and settings are fine; only this page failed to draw.</p>
          <CodeBlock className="mt-4 text-left" code={this.state.error.message} label="Error" />
        </EmptyState>
      );
    }
    return this.props.children;
  }
}

function titleFor(route: Route): string {
  const [first, second] = route.segments;
  switch (first) {
    case undefined:
      return "Overview";
    case "queue":
      return "Queue";
    case "libraries":
      return second === "new" ? "Add library" : "Libraries";
    case "library":
      return "Library";
    case "settings":
      return "Settings";
    case "setup":
      return "Welcome";
    default:
      return "Not found";
  }
}

function Screen({ route }: { route: Route }) {
  const [first, second] = route.segments;
  switch (first) {
    case undefined:
    case "setup":
      return <OverviewScreen />;
    case "queue":
      return <QueueScreen route={route} />;
    case "libraries":
      return second === "new" ? <AddLibraryScreen /> : <LibrariesScreen />;
    case "library":
      return second ? <LibraryScreen id={second} route={route} /> : <LibrariesScreen />;
    case "settings":
      return <SettingsScreen route={route} />;
    default:
      return (
        <EmptyState
          title="This page doesn't exist"
          action={
            <a href={href("/")} className={buttonVariants({ variant: "primary" })}>
              Go to overview
            </a>
          }
        >
          The link may be from an older version of Chrysopoeia.
        </EmptyState>
      );
  }
}

/**
 * Job and file detail sheets, opened from any screen via `?job=` / `?file=`.
 * Ids that aren't UUIDs (a mangled or crafted link) are ignored.
 */
function RouteSheets({ route }: { route: Route }) {
  const jobParam = route.params.get("job");
  const fileParam = route.params.get("file");
  const jobId = isUuid(jobParam) ? jobParam : null;
  const fileId = isUuid(fileParam) ? fileParam : null;
  return (
    <>
      <JobSheet jobId={jobId} onClose={() => closeSheet(["job"])} />
      <FileSheet fileId={jobId ? null : fileId} onClose={() => closeSheet(["file"])} />
    </>
  );
}

function BootScreen() {
  return (
    <div className="flex min-h-dvh" aria-busy="true" aria-label="Loading Chrysopoeia">
      <div className="hidden w-60 shrink-0 flex-col gap-4 border-r border-line bg-sunken px-5 pt-5 md:flex">
        <Brand />
        <Skeleton className="mt-4 h-8 w-full" />
        <Skeleton className="h-8 w-full" />
      </div>
      <div className="flex-1 px-4 pt-8 md:px-10 md:pt-12">
        <Skeleton className="h-10 w-64" />
        <Skeleton className="mt-8 h-40 w-full max-w-4xl" />
        <Skeleton className="mt-6 h-28 w-full max-w-4xl" />
      </div>
    </div>
  );
}

function Unreachable({ error, retrying, onRetry }: { error: unknown; retrying: boolean; onRetry: () => void }) {
  const network = error instanceof ApiError && error.isUnavailable;
  return (
    <div className="grid min-h-dvh place-items-center px-5 py-10">
      <div className="w-full max-w-lg">
        <Brand className="mb-10" />
        <h1 className="font-display text-4xl leading-tight text-fg">
          {network ? "Can't reach Chrysopoeia" : "Chrysopoeia couldn't load"}
        </h1>
        <p className="mt-3 text-[0.9375rem] leading-relaxed text-muted">
          {network
            ? "The server isn't answering. It may be restarting or the container may have stopped. This page tries again every few seconds."
            : errorMessage(error)}
        </p>
        <ul className="mt-6 list-disc space-y-2 pl-5 text-sm text-fg/90 marker:text-muted">
          <li>Check that the container is running (on Unraid: the Docker tab).</li>
          <li>Look at the container log for errors.</li>
          <li>
            The app is looking for its API at <code className="font-mono text-[0.8125rem]">{apiBase()}</code>.
          </li>
        </ul>
        <div className="mt-8 flex items-center gap-3">
          <Button variant="primary" onClick={onRetry} loading={retrying}>
            <RefreshCw aria-hidden />
            Try again
          </Button>
          <p role="status" className="text-[0.8125rem] text-muted">
            {retrying ? "Checking…" : ""}
          </p>
        </div>
      </div>
    </div>
  );
}

export function App() {
  const route = useRoute();
  const settings = useSettings();
  const libraries = useLibraries();

  const needsSetup = settings.data !== undefined && libraries.data !== undefined && !settings.data.onboarded && libraries.data.length === 0;

  useEffect(() => {
    document.title = needsSetup ? "Welcome — Chrysopoeia" : `${titleFor(route)} — Chrysopoeia`;
  }, [route, needsSetup]);

  // Setup is only for the first run; send stale links to the overview.
  useEffect(() => {
    if (!needsSetup && settings.data && route.segments[0] === "setup") navigate("/", { replace: true });
  }, [needsSetup, settings.data, route.segments]);

  // Keep retrying quietly while the server is away.
  const failed = Boolean((settings.error && !settings.data) || (libraries.error && !libraries.data));
  const refetchSettings = settings.refetch;
  const refetchLibraries = libraries.refetch;
  useEffect(() => {
    if (!failed) return;
    const timer = setInterval(() => {
      void refetchSettings();
      void refetchLibraries();
    }, 5000);
    return () => clearInterval(timer);
  }, [failed, refetchSettings, refetchLibraries]);

  if (failed) {
    return (
      <Unreachable
        error={settings.error ?? libraries.error}
        retrying={settings.isFetching || libraries.isFetching}
        onRetry={() => {
          void settings.refetch();
          void libraries.refetch();
        }}
      />
    );
  }

  if (!settings.data || !libraries.data) return <BootScreen />;

  if (needsSetup) return <SetupScreen settings={settings.data} />;

  return (
    <Shell route={route}>
      <ErrorBoundary resetKey={route.segments.join("/")}>
        <Screen route={route} />
        <RouteSheets route={route} />
      </ErrorBoundary>
      <NavigationPrompt />
    </Shell>
  );
}
