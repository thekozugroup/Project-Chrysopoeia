"use client";

/** "Add library": the folder and goal steps from setup, inside the app frame. */

import { ArrowLeft } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { PageHeader } from "@/components/shell";
import { Button, buttonVariants } from "@/components/ui/button";
import { href, navigate } from "@/lib/router";
import { FolderStep, GoalStep } from "./library-steps";

export function AddLibraryScreen() {
  const [path, setPath] = useState<string | null>(null);
  const [step, setStep] = useState<"folder" | "goal">("folder");
  const [folderError, setFolderError] = useState<string | null>(null);

  return (
    <div className="max-w-3xl">
      <PageHeader
        title="Add a library"
        description="A library is a folder Chrysopoeia watches. Each library has its own goal."
      />
      {step === "folder" ? (
        <>
          <FolderStep
            initialPath={path ?? undefined}
            error={folderError}
            onNavigate={() => setFolderError(null)}
            onPicked={(picked) => {
              setPath(picked);
              setFolderError(null);
              setStep("goal");
            }}
          />
          <div className="mt-6">
            <a href={href("/")} className={buttonVariants({ variant: "quiet" })}>
              Cancel
            </a>
          </div>
        </>
      ) : path ? (
        <GoalStep
          path={path}
          submitLabel="Add library"
          onChangeFolder={() => setStep("folder")}
          onFolderError={(message) => {
            setFolderError(message);
            setStep("folder");
          }}
          onCreated={(library) => {
            toast.success(`Scanning ${library.name}`, { description: "Files appear here as they're found." });
            navigate(`/library/${library.id}`, { replace: true });
          }}
          secondary={
            <Button variant="quiet" onClick={() => setStep("folder")}>
              <ArrowLeft aria-hidden />
              Back
            </Button>
          }
        />
      ) : null}
    </div>
  );
}
