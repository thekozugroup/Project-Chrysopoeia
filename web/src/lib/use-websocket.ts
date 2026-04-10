"use client";

import { useEffect, useRef, useCallback } from "react";
import { useAppStore } from "./store";
import type { WSMessage } from "./api";

const WS_URL =
  process.env.NEXT_PUBLIC_WS_URL ?? "ws://localhost:8080/ws";

const MIN_DELAY = 1_000;
const MAX_DELAY = 30_000;

export function useWebSocket(url: string = WS_URL) {
  const wsRef = useRef<WebSocket | null>(null);
  const retriesRef = useRef(0);
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined);
  const unmountedRef = useRef(false);

  const dispatch = useCallback((msg: WSMessage) => {
    const s = useAppStore.getState();
    switch (msg.type) {
      case "file_progress":
        s.updateFileProgress(
          msg.file.id,
          msg.file.progress,
          msg.file.speed,
          msg.file.eta_secs,
        );
        break;
      case "file_complete":
        s.updateFileStatus(
          msg.file.id,
          "complete",
          msg.file.output_size_bytes,
        );
        break;
      case "file_error":
        s.updateFileStatus(
          msg.file.id,
          "error",
          undefined,
          msg.file.error_message,
        );
        break;
      case "stats_update":
        s.updateStats(msg.stats);
        break;
      case "scan_progress":
        // Could be extended; for now update scanning state
        break;
    }
  }, []);

  const connect = useCallback(() => {
    if (unmountedRef.current) return;

    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      retriesRef.current = 0;
      useAppStore.getState().setWsConnected(true);
    };

    ws.onmessage = (ev) => {
      try {
        const data = JSON.parse(ev.data) as WSMessage;
        dispatch(data);
      } catch {
        // ignore malformed messages
      }
    };

    ws.onclose = () => {
      useAppStore.getState().setWsConnected(false);
      if (unmountedRef.current) return;

      const delay = Math.min(
        MIN_DELAY * Math.pow(2, retriesRef.current),
        MAX_DELAY,
      );
      retriesRef.current += 1;
      timerRef.current = setTimeout(connect, delay);
    };

    ws.onerror = () => {
      // onclose will fire after this; let it handle reconnect
      ws.close();
    };
  }, [url, dispatch]);

  useEffect(() => {
    unmountedRef.current = false;

    // In dev without a backend, don't attempt WS connection
    // Check if the API is reachable first
    fetch(url.replace("ws", "http").replace("/ws", "/api/health"), {
      signal: AbortSignal.timeout(2000),
    })
      .then((res) => {
        if (res.ok) connect();
        // If not ok, stay in mock mode with wsConnected = true (from store default)
      })
      .catch(() => {
        // Backend not running — stay in mock mode, don't touch wsConnected
      });

    return () => {
      unmountedRef.current = true;
      clearTimeout(timerRef.current);
      wsRef.current?.close();
    };
  }, [connect, url]);

  return {
    connected: useAppStore((s) => s.wsConnected),
    lastMessage: null as WSMessage | null,
  };
}
