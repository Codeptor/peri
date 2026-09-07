"use client";
import { useEffect, useInsertionEffect, useRef, useState } from "react";
import type { Position, Decision, NewsItem, EquityPoint } from "./api";

export type LiveHandlers = Partial<{
  mids: (m: Record<string, number>) => void;
  position: (p: Position) => void;
  decision: (d: Decision) => void;
  news: (n: NewsItem) => void;
  equity: (e: EquityPoint) => void;
}>;

const WS_URL = "ws://127.0.0.1:7411/ws";

export function useLiveFeed(handlers: LiveHandlers): { connected: boolean } {
  const [connected, setConnected] = useState(false);
  const handlersRef = useRef<LiveHandlers>(handlers);
  // Latest-ref, published in the *insertion* phase: it commits before layout
  // effects and before the passive effect below that opens the socket, so the
  // ref already holds this render's handlers by the time any ws callback can
  // fire (socket events are macrotasks — they cannot interleave with a commit).
  // Writing it during render is what `react-hooks/refs` forbids.
  useInsertionEffect(() => {
    handlersRef.current = handlers;
  }, [handlers]);
  const retryRef = useRef(0);
  const wsRef = useRef<WebSocket | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let closed = false;

    function schedule() {
      if (closed) return;
      const delay = Math.min(1000 * Math.pow(2, retryRef.current), 10000);
      // Use 1s first retry, capped 10s as spec
      const actual = retryRef.current === 0 ? 1000 : delay;
      timerRef.current = setTimeout(connect, actual);
    }

    function connect() {
      if (closed) return;
      try {
        const ws = new WebSocket(WS_URL);
        wsRef.current = ws;
        ws.onopen = () => {
          retryRef.current = 0;
          setConnected(true);
        };
        ws.onclose = () => {
          setConnected(false);
          if (!closed) {
            retryRef.current += 1;
            schedule();
          }
        };
        ws.onerror = () => {
          try { ws.close(); } catch {}
        };
        ws.onmessage = (ev) => {
          try {
            const msg = JSON.parse(ev.data as string) as { type: string; data: unknown };
            const h = handlersRef.current;
            switch (msg.type) {
              case "mids":
                h.mids?.(msg.data as Record<string, number>);
                break;
              case "position":
                h.position?.(msg.data as Position);
                break;
              case "decision":
                h.decision?.(msg.data as Decision);
                break;
              case "news":
                h.news?.(msg.data as NewsItem);
                break;
              case "equity":
                h.equity?.(msg.data as EquityPoint);
                break;
              default:
                break;
            }
          } catch {
            // ignore parse errors
          }
        };
      } catch {
        retryRef.current += 1;
        schedule();
      }
    }

    connect();

    return () => {
      closed = true;
      if (timerRef.current) clearTimeout(timerRef.current);
      try { wsRef.current?.close(); } catch {}
    };
  }, []);

  return { connected };
}
