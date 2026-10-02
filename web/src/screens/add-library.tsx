"use client";

/**
 * "Add library": the folder and goal steps from setup, inside the app frame.
 * Each step's question is the page's one heading.
 */

import { ArrowLeft } from "lucide-react";
import { useState } from "react";
import { Button, buttonVariants } from "@/components/ui/button";
import { href, navigate } from "@/lib/router";
import { FolderStep, GoalStep } from "./library-steps";

export function AddLibraryScreen() {
  const [path, setPath] = useState<string | null>(null);
  const [step, setStep] = useState<"folder" | "goal">("folder");
  const [folderError, setFolderError] = useState<string | null>(null);

  return (
    <div className="max-w-3xl">
      {step === "folder" ? (
        <>
          <FolderStep
            level={1}
            step="Add a library, step 1 of 2"
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
          level={1}
          step="Add a library, step 2 of 2"
          path={path}
          submitLabel="Add library"
          onChangeFolder={() => setStep("folder")}
          onFolderError={(message) => {
            setFolderError(message);
            setStep("folder");
          }}
          onCreated={(library) => {
            // The library page says it's scanning; no toast needed.
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
