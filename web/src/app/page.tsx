"use client";

import { SidebarProvider, SidebarInset } from "@/components/ui/sidebar";
import { AppSidebar } from "@/components/app-sidebar";
import { Dashboard } from "@/components/dashboard";
import { ErrorBoundary } from "@/components/error-boundary";
import { StatusBar } from "@/components/status-bar";
import { ThemeToggle } from "@/components/theme-toggle";

export default function Home() {
  return (
    <ErrorBoundary>
      <SidebarProvider>
        <AppSidebar />
        <SidebarInset className="flex h-screen flex-col">
          <div className="flex-1 min-h-0">
            <Dashboard />
          </div>
          <StatusBar />
        </SidebarInset>
        <ThemeToggle />
      </SidebarProvider>
    </ErrorBoundary>
  );
}
