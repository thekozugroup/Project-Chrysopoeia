"use client";

import { Component, type ReactNode } from "react";
import { AlertTriangle, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  handleRetry = () => {
    window.location.reload();
  };

  render() {
    if (this.state.hasError) {
      return (
        <div className="flex h-screen w-full items-center justify-center bg-background">
          <div className="animate-fade-up flex flex-col items-center gap-6 text-center px-6 max-w-md">
            <div className="flex h-16 w-16 items-center justify-center rounded-full bg-destructive/10">
              <AlertTriangle className="h-8 w-8 text-destructive" />
            </div>
            <div className="space-y-2">
              <h2 className="text-lg font-heading tracking-tight text-foreground">
                Something went wrong
              </h2>
              <p className="text-sm text-muted-foreground leading-relaxed">
                An unexpected error occurred. This has been noted and will be
                investigated. You can try reloading the page.
              </p>
              {this.state.error?.message && (
                <p className="mt-3 rounded-md bg-secondary/60 px-3 py-2 font-mono text-[11px] text-muted-foreground/70 break-all">
                  {this.state.error.message}
                </p>
              )}
            </div>
            <Button
              type="button"
              onClick={this.handleRetry}
              className="gap-2 bg-gold text-gold-foreground hover:bg-gold/90 shadow-[0_0_12px_oklch(0.78_0.12_75_/_0.2)]"
            >
              <RotateCw className="h-4 w-4" />
              Reload page
            </Button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
