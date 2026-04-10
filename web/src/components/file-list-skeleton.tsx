"use client";

import { Skeleton } from "@/components/ui/skeleton";

const ROWS = 6;

function SkeletonRow({ index }: { index: number }) {
  const staggerClass = `stagger-${Math.min(index + 1, 5)}`;

  return (
    <div
      className={`animate-fade-up ${staggerClass} flex items-center gap-2 md:gap-4 border-b border-border/40 px-3 md:px-6 py-3`}
    >
      {/* Checkbox */}
      <div className="w-4">
        <Skeleton className="h-3.5 w-3.5 rounded-sm" />
      </div>
      {/* Status */}
      <div className="w-5">
        <Skeleton className="h-4 w-4 rounded-full" />
      </div>
      {/* File info */}
      <div className="flex-1 min-w-0 space-y-2">
        <Skeleton
          className="h-3.5 rounded"
          style={{ width: `${55 + index * 6}%`, maxWidth: "280px" }}
        />
        <div className="flex gap-1.5">
          <Skeleton className="h-4 w-10 rounded" />
          <Skeleton className="h-4 w-8 rounded" />
        </div>
      </div>
      {/* Res */}
      <div className="hidden md:block w-20">
        <Skeleton className="ml-auto h-3 w-14 rounded" />
      </div>
      {/* Duration */}
      <div className="hidden md:block w-14">
        <Skeleton className="ml-auto h-3 w-10 rounded" />
      </div>
      {/* Size */}
      <div className="w-20">
        <Skeleton className="ml-auto h-3 w-12 rounded" />
      </div>
      {/* Progress */}
      <div className="w-16 md:w-32">
        <Skeleton className="ml-auto h-3 w-14 md:w-24 rounded" />
      </div>
    </div>
  );
}

export function FileListSkeleton() {
  return (
    <div role="table" aria-label="Loading media files" className="min-w-0">
      {/* Column headers skeleton */}
      <div className="sticky top-0 z-10 flex items-center gap-2 md:gap-4 border-b border-border bg-card/85 backdrop-blur-md px-3 md:px-6 py-2">
        <div className="w-4" />
        <div className="w-5" />
        <div className="flex-1 text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          File
        </div>
        <div className="hidden md:block w-20 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Res
        </div>
        <div className="hidden md:block w-14 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Dur
        </div>
        <div className="w-20 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Size
        </div>
        <div className="w-16 md:w-32 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Progress
        </div>
      </div>

      {Array.from({ length: ROWS }, (_, i) => (
        <SkeletonRow key={i} index={i} />
      ))}
    </div>
  );
}
