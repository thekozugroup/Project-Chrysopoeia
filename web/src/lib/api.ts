import type {
  MediaFile,
  LibraryStats,
  HardwareInfo,
  LibraryPath,
  LibraryTranscodeConfig,
  GlobalSettings,
} from "./types";

const API_BASE = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080/api";
const WS_BASE = process.env.NEXT_PUBLIC_WS_URL ?? "ws://localhost:8080/ws";

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

// ---------------------------------------------------------------------------
// Fetch with single retry
// ---------------------------------------------------------------------------

async function fetchWithRetry(
  input: string,
  init?: RequestInit,
  retries = 1,
): Promise<Response> {
  let lastError: unknown;
  for (let attempt = 0; attempt <= retries; attempt++) {
    try {
      const res = await fetch(input, init);
      if (res.ok || attempt === retries) return res;
      lastError = new ApiError(res.status, await res.text());
    } catch (err) {
      lastError = err;
      if (attempt === retries) break;
    }
  }
  throw lastError;
}

// ---------------------------------------------------------------------------
// Base request helper
// ---------------------------------------------------------------------------

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetchWithRetry(`${API_BASE}${path}`, {
    headers: { "Content-Type": "application/json" },
    ...init,
  });
  if (!res.ok) {
    const body = await res.text();
    throw new ApiError(res.status, body || `API ${res.status}`);
  }
  return res.json() as Promise<T>;
}

export async function getFiles(): Promise<MediaFile[]> {
  return request("/files");
}

export async function scanLibrary(): Promise<{ queued: number }> {
  return request("/scan", { method: "POST" });
}

export async function getStats(): Promise<LibraryStats> {
  return request("/stats");
}

export async function startProcessing(): Promise<void> {
  await request("/process/start", { method: "POST" });
}

export async function stopProcessing(): Promise<void> {
  await request("/process/stop", { method: "POST" });
}

export async function getHardware(): Promise<HardwareInfo> {
  return request("/hardware");
}

export async function getConfig(): Promise<GlobalSettings> {
  return request("/config");
}

export async function updateConfig(config: Partial<GlobalSettings>): Promise<GlobalSettings> {
  return request("/config", {
    method: "PATCH",
    body: JSON.stringify(config),
  });
}

export async function getLibraries(): Promise<LibraryPath[]> {
  return request("/libraries");
}

export async function addLibrary(path: string): Promise<LibraryPath> {
  return request("/libraries", {
    method: "POST",
    body: JSON.stringify({ path }),
  });
}

export async function deleteLibrary(id: string): Promise<void> {
  await request(`/libraries/${id}`, { method: "DELETE" });
}

export async function updateLibrary(
  id: string,
  config: Partial<LibraryTranscodeConfig>,
): Promise<LibraryPath> {
  return request(`/libraries/${id}`, {
    method: "PUT",
    body: JSON.stringify(config),
  });
}

export async function checkHealth(): Promise<{ ok: boolean }> {
  return request("/health");
}

export type WSMessage =
  | { type: "file_progress"; file: MediaFile }
  | { type: "file_complete"; file: MediaFile }
  | { type: "file_error"; file: MediaFile }
  | { type: "scan_progress"; scanned: number; total: number }
  | { type: "stats_update"; stats: LibraryStats };

export function connectWS(onMessage: (msg: WSMessage) => void): () => void {
  let ws: WebSocket | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout>;

  function connect() {
    ws = new WebSocket(WS_BASE);

    ws.onmessage = (ev) => {
      try {
        const data = JSON.parse(ev.data) as WSMessage;
        onMessage(data);
      } catch {
        // ignore malformed messages
      }
    };

    ws.onclose = () => {
      reconnectTimer = setTimeout(connect, 3000);
    };
  }

  connect();

  return () => {
    clearTimeout(reconnectTimer);
    ws?.close();
  };
}
