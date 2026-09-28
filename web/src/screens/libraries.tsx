"use client";

/** All libraries (the phone tab bar lands here; desktop lists them in the sidebar). */

import { FolderPlus } from "lucide-react";
import { PageHeader } from "@/components/shell";
import { buttonVariants } from "@/components/ui/button";
import { EmptyState, Skeleton } from "@/components/ui/display";
import { useLibraries } from "@/lib/queries";
import { href } from "@/lib/router";
import { LibraryRow } from "./overview";

export function LibrariesScreen() {
  const libraries = useLibraries();
  const list = libraries.data ?? [];
  return (
    <div>
      <PageHeader
        title="Libraries"
        description="Each library is a folder Chrysopoeia watches and converts toward its own goal."
        actions={
          <a href={href("/libraries/new")} className={buttonVariants({ variant: "primary" })}>
            <FolderPlus aria-hidden />
            Add library
          </a>
        }
      />
      {libraries.isPending ? (
        <div className="space-y-3">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-20 w-full" />
        </div>
      ) : list.length === 0 ? (
        <EmptyState
          icon={<FolderPlus aria-hidden />}
          title="No libraries yet"
          action={
            <a href={href("/libraries/new")} className={buttonVariants({ variant: "primary" })}>
              Choose a folder
            </a>
          }
        >
          Add the folder that holds your movies or shows. Chrysopoeia scans it and converts what&apos;s worth
          converting.
        </EmptyState>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
          {list.map((library) => (
            <LibraryRow key={library.id} library={library} />
          ))}
        </ul>
      )}
    </div>
  );
}
