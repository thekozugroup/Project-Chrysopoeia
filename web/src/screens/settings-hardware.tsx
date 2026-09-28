"use client";

/**
 * Settings › Hardware: what was detected, which encoders really work (each
 * hardware encoder is proven by a test encode), setup fixes, and the
 * preference for which to use.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  ChevronRight,
  CircleCheck,
  CircleDashed,
  CircleMinus,
  CircleX,
  Cpu,
  MemoryStick,
  MonitorPlay,
  RefreshCw,
  Server,
} from "lucide-react";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { SetupHints } from "@/components/hints";
import { Button } from "@/components/ui/button";
import { Field, Select, SwitchRow } from "@/components/ui/controls";
import { Callout, CodeBlock, SectionHeading, Skeleton } from "@/components/ui/display";
import { ApiError, api, errorMessage } from "@/lib/api";
import { ffmpegVersionLabel, formatBytes, formatRelative } from "@/lib/format";
import { encoderCell, isDetecting, matrixApis, preferenceChoices } from "@/lib/hardware";
import {
  GPU_VENDOR_LABEL,
  HW_API_LABEL,
  HW_API_TECH,
  HW_PREFERENCE_API,
  HW_PREFERENCE_LABEL,
  VIDEO_CODEC_LABEL,
} from "@/lib/labels";
import { keys, useHardware } from "@/lib/queries";
import type { EncoderStatus, HardwareInfo, HwApi, HwPreference, Settings } from "@/lib/types";
import { VIDEO_CODECS } from "@/lib/types";
import { cn } from "@/lib/utils";

function DeviceCard({ icon, title, children }: { icon: ReactNode; title: string; children: ReactNode }) {
  return (
    <div className="rounded-lg border border-line bg-surface p-4">
      <div className="flex items-center gap-2 text-muted [&_svg]:size-4">
        {icon}
        <h3 className="text-[0.8125rem] font-medium">{title}</h3>
      </div>
      <div className="mt-2 text-sm text-fg">{children}</div>
    </div>
  );
}

function Devices({ hw }: { hw: HardwareInfo }) {
  const cores = hw.cpu.cgroup_limit
    ? `${hw.cpu.cgroup_limit.toLocaleString(undefined, { maximumFractionDigits: 1 })} of ${hw.cpu.logical_cores} cores allowed by the container`
    : `${hw.cpu.logical_cores} cores${hw.cpu.physical_cores ? ` (${hw.cpu.physical_cores} physical)` : ""}`;
  const memoryLimit = hw.memory.cgroup_limit_bytes;
  return (
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      <DeviceCard icon={<Cpu aria-hidden />} title="Processor">
        <p className="font-medium">{hw.cpu.model}</p>
        <p className="mt-0.5 text-[0.8125rem] text-muted">{cores}</p>
      </DeviceCard>
      <DeviceCard icon={<MemoryStick aria-hidden />} title="Memory">
        <p className="font-medium">{formatBytes(memoryLimit ?? hw.memory.total_bytes)}</p>
        <p className="mt-0.5 text-[0.8125rem] text-muted">
          {memoryLimit ? `Container limit · ${formatBytes(hw.memory.total_bytes)} in the machine` : `${formatBytes(hw.memory.available_bytes)} free`}
        </p>
      </DeviceCard>
      {hw.gpus.length ? (
        hw.gpus.map((gpu) => (
          <DeviceCard key={`${gpu.name}-${gpu.render_node ?? ""}`} icon={<MonitorPlay aria-hidden />} title={`${GPU_VENDOR_LABEL[gpu.vendor]} graphics`}>
            <p className="font-medium">{gpu.name}</p>
            <p className="mt-0.5 font-mono text-xs text-muted">
              {[gpu.driver, gpu.render_node].filter(Boolean).join(" · ") || "No render node"}
            </p>
          </DeviceCard>
        ))
      ) : (
        <DeviceCard icon={<MonitorPlay aria-hidden />} title="Graphics">
          <p className="font-medium">No GPU found</p>
          <p className="mt-0.5 text-[0.8125rem] text-muted">Converting on the CPU works; it&apos;s just slower.</p>
        </DeviceCard>
      )}
    </div>
  );
}

function cellFor(encoders: EncoderStatus[], codec: string, api: HwApi) {
  return encoders.find((e) => e.codec === codec && e.api === api);
}

/** A disclosure listing encoders with ffmpeg's reason for each. */
function EncoderReasons({ title, encoders }: { title: string; encoders: EncoderStatus[] }) {
  if (!encoders.length) return null;
  return (
    <details className="group mt-4 rounded-lg border border-line bg-surface">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-sm font-medium text-fg [&::-webkit-details-marker]:hidden">
        <ChevronRight className="size-4 text-muted transition-transform duration-200 group-open:rotate-90" aria-hidden />
        {title}
      </summary>
      <ul className="divide-y divide-line border-t border-line">
        {encoders.map((e) => (
          <li key={e.name} className="px-4 py-3">
            <p className="font-mono text-[0.8125rem] text-fg">{e.name}</p>
            <p className="mt-1 text-[0.8125rem] leading-relaxed break-words whitespace-pre-wrap text-muted">{e.error}</p>
          </li>
        ))}
      </ul>
    </details>
  );
}

function EncoderMatrix({ hw }: { hw: HardwareInfo }) {
  // Only devices that are actually here get a column: ffmpeg lists NVENC,
  // QSV and VA-API encoders even on a machine without those GPUs.
  const apis: HwApi[] = ["software", ...matrixApis(hw)];
  const shown = hw.encoders.filter((e) => apis.includes(e.api));
  const failures = shown.filter((e) => e.error && encoderCell(hw, e) === "failed");
  const unsupported = shown.filter((e) => e.error && encoderCell(hw, e) === "unsupported");
  const notSetUp = shown.some((e) => encoderCell(hw, e) === "not_set_up");
  return (
    <div>
      <div className="overflow-x-auto rounded-lg border border-line bg-surface">
        <table className={cn("w-full text-sm", apis.length > 2 && "min-w-[32rem]")}>
          <caption className="sr-only">Which video formats each device can encode</caption>
          <thead>
            <tr className="border-b border-line text-left text-xs text-muted">
              <th scope="col" className="py-2.5 pl-4 font-medium">
                Format
              </th>
              {apis.map((api) => (
                <th key={api} scope="col" className="px-3 py-2.5 font-medium">
                  {HW_API_LABEL[api]}
                  {api !== "software" ? <span className="ml-1 font-mono text-xs">{HW_API_TECH[api]}</span> : null}
                </th>
              ))}
            </tr>
          </thead>
          <tbody className="divide-y divide-line">
            {VIDEO_CODECS.map((codec) => (
              <tr key={codec}>
                <th scope="row" className="py-3 pl-4 text-left font-medium text-fg">
                  {VIDEO_CODEC_LABEL[codec]}
                </th>
                {apis.map((api) => {
                  const e = cellFor(hw.encoders, codec, api);
                  const cell = encoderCell(hw, e);
                  let content: ReactNode;
                  if (cell === "unavailable") {
                    content = (
                      <span className="inline-flex items-center gap-1.5 text-muted">
                        <CircleMinus className="size-4" aria-hidden />
                        <span>Not available</span>
                      </span>
                    );
                  } else if (cell === "works") {
                    content = (
                      <span className="inline-flex items-center gap-1.5 text-success">
                        <CircleCheck className="size-4" aria-hidden />
                        <span className="text-fg">Works</span>
                      </span>
                    );
                  } else if (cell === "unsupported") {
                    content = (
                      <span className="inline-flex items-center gap-1.5 text-muted">
                        <CircleMinus className="size-4" aria-hidden />
                        <span>Not supported</span>
                      </span>
                    );
                  } else if (cell === "not_set_up") {
                    content = (
                      <span className="inline-flex items-center gap-1.5 text-muted">
                        <CircleDashed className="size-4" aria-hidden />
                        <span>Not set up</span>
                      </span>
                    );
                  } else {
                    content = (
                      <span className="inline-flex items-center gap-1.5 text-danger">
                        <CircleX className="size-4" aria-hidden />
                        <span>Failed test</span>
                      </span>
                    );
                  }
                  return (
                    <td key={api} className="px-3 py-3 text-[0.8125rem] whitespace-nowrap">
                      {content}
                      {e && cell !== "unavailable" ? (
                        <span className="mt-0.5 block font-mono text-[0.6875rem] text-muted">{e.name}</span>
                      ) : null}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <p className="mt-2 text-[0.8125rem] text-muted">
        Hardware encoders are listed as working only after a short test encode succeeds on this machine.
        {notSetUp ? " “Not set up” means the device isn't passed to the container; the setup tips above show how." : ""}
      </p>
      <EncoderReasons
        title={`Why ${failures.length === 1 ? "1 encoder" : `${failures.length} encoders`} failed the test`}
        encoders={failures}
      />
      <EncoderReasons
        title={`Why ${unsupported.length === 1 ? "1 format isn't" : `${unsupported.length} formats aren't`} supported by the GPU`}
        encoders={unsupported}
      />
    </div>
  );
}

export function HardwareSection({
  draft,
  onChange,
}: {
  draft: Settings;
  onChange: (patch: Partial<Settings>) => void;
}) {
  const client = useQueryClient();
  const hardware = useHardware();
  // The server's "Checking your hardware…" stand-in is not a result.
  const detectingFirst = isDetecting(hardware.data);
  const hw = detectingFirst ? undefined : hardware.data;
  const detect = useMutation({
    mutationFn: () => api.detectHardware(),
    onSuccess: (info) => {
      client.setQueryData(keys.hardware, info);
      void client.invalidateQueries({ queryKey: keys.queue });
      toast.success("Hardware checked", { description: info.recommended_jobs.reason });
    },
    onError: (err) => toast.error("Couldn't check the hardware", { description: errorMessage(err) }),
  });
  const detecting = detect.isPending;
  const stillStarting =
    detectingFirst || (hardware.error instanceof ApiError && hardware.error.status === 503);

  return (
    <div className="flex flex-col gap-10">
      <section>
        <SectionHeading
          title="This machine"
          description={
            hw ? `Checked ${formatRelative(hw.detected_at)}${hw.in_container ? " · running in a container" : ""}.` : undefined
          }
          action={
            <Button variant="secondary" size="sm" onClick={() => detect.mutate()} loading={detecting}>
              {detecting ? null : <RefreshCw aria-hidden />}
              {detecting ? "Checking…" : "Check again"}
            </Button>
          }
        />
        {detecting ? (
          <p role="status" className="mb-4 text-[0.8125rem] text-muted">
            Testing each encoder with a short encode. This takes up to 15 seconds.
          </p>
        ) : null}
        {hw ? (
          <div className={cn(detecting && "opacity-60 transition-opacity")}>
            <Devices hw={hw} />
          </div>
        ) : hardware.error && !stillStarting ? (
          <Callout tone="danger" title="Hardware information isn't available">
            {errorMessage(hardware.error)}
          </Callout>
        ) : (
          <div role="status" aria-label="Checking your hardware">
            <p className="mb-3 text-[0.8125rem] text-muted">Checking your hardware… this takes a few seconds.</p>
            <div className="grid gap-3 sm:grid-cols-3">
              <Skeleton className="h-24" />
              <Skeleton className="h-24" />
              <Skeleton className="h-24" />
            </div>
          </div>
        )}
      </section>

      {hw && hw.hints.length ? (
        <section>
          <SectionHeading title="Setup tips" description="Plain-language fixes for anything that's slowing things down." />
          <SetupHints hints={hw.hints} />
        </section>
      ) : null}

      {hw ? (
        <section>
          <SectionHeading title="What can encode what" />
          <EncoderMatrix hw={hw} />
        </section>
      ) : null}

      <section>
        <SectionHeading title="Preferences" />
        <div className="flex max-w-xl flex-col gap-6">
          <Field
            label="Use for converting"
            description={
              draft.hardware === "auto"
                ? "Uses the fastest encoder that passed its test, and the CPU when none did."
                : draft.hardware === "cpu"
                  ? "Always uses the CPU, even when a GPU could do it faster."
                  : "Only uses this hardware. Formats it can't encode fall back to the CPU if allowed below."
            }
          >
            <Select value={draft.hardware} onChange={(e) => onChange({ hardware: e.target.value as HwPreference })}>
              {preferenceChoices(hw, draft.hardware, HW_PREFERENCE_API).map((choice) => (
                <option key={choice.value} value={choice.value} disabled={choice.disabled}>
                  {HW_PREFERENCE_LABEL[choice.value]}
                  {choice.disabled ? " (not working on this machine)" : ""}
                </option>
              ))}
            </Select>
          </Field>
          <SwitchRow
            label="If the GPU fails, try the CPU"
            description="A file that the GPU can't convert is retried on the CPU instead of being marked failed."
            checked={draft.cpu_fallback}
            onCheckedChange={(cpu_fallback) => onChange({ cpu_fallback })}
          />
        </div>
      </section>

      {hw ? (
        <section>
          <SectionHeading title="ffmpeg" />
          {hw.ffmpeg.found ? (
            <div className="flex flex-col gap-3">
              <p className="flex items-center gap-2 text-sm text-fg">
                <Server className="size-4 text-muted" aria-hidden />
                {ffmpegVersionLabel(hw.ffmpeg.version)}
              </p>
              <p className="font-mono text-xs text-muted">
                {hw.ffmpeg.ffmpeg_path} · {hw.ffmpeg.ffprobe_path}
                {hw.ffmpeg.ffprobe_found ? "" : " (ffprobe not found)"}
              </p>
              {hw.filters.length ? (
                <p className="text-[0.8125rem] text-muted">
                  Verification filters available: <span className="font-mono text-xs">{hw.filters.join(", ")}</span>
                </p>
              ) : null}
            </div>
          ) : (
            <Callout tone="danger" title="ffmpeg wasn't found">
              <p>Nothing can be converted until ffmpeg is available. The official image includes it.</p>
              <CodeBlock className="mt-3" code={`FFMPEG_PATH=${hw.ffmpeg.ffmpeg_path}`} label="Path Chrysopoeia tried" />
            </Callout>
          )}
        </section>
      ) : null}
    </div>
  );
}
