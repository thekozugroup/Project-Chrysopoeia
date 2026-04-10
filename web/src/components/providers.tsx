"use client";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState, useCallback } from "react";
import { Toaster } from "sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useWebSocket } from "@/lib/use-websocket";
import { useDataInit } from "@/lib/use-data-init";
import { useKeyboardShortcuts } from "@/lib/use-keyboard-shortcuts";
import { ShortcutsDialog } from "@/components/shortcuts-dialog";

function DataInitProvider({ children }: { children: React.ReactNode }) {
  useDataInit();
  useWebSocket();
  return <>{children}</>;
}

function KeyboardShortcutsManager() {
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const showHelp = useCallback(() => setShortcutsOpen(true), []);

  useKeyboardShortcuts(showHelp);

  return (
    <ShortcutsDialog open={shortcutsOpen} onOpenChange={setShortcutsOpen} />
  );
}

export function Providers({ children }: { children: React.ReactNode }) {
  const [queryClient] = useState(
    () =>
      new QueryClient({
        defaultOptions: { queries: { staleTime: 30_000, retry: 1 } },
      })
  );

  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider delay={300}>
        <DataInitProvider>{children}</DataInitProvider>
        <KeyboardShortcutsManager />
        <Toaster
          position="bottom-right"
          toastOptions={{
            className: "!bg-card !text-foreground !border-border/60 !shadow-lg",
            descriptionClassName: "!text-muted-foreground",
          }}
          theme="system"
          richColors
        />
      </TooltipProvider>
    </QueryClientProvider>
  );
}
