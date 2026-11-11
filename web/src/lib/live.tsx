// One shared EventSource for the whole app: per-second ticks and the live query feed.

import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import type { ModeState, QueryEvent, Sample, Summary } from "./api";
import { api } from "./api";

interface Tick {
  sample: Sample | null;
  summary: Summary;
  mode: ModeState;
  cache_entries: number;
}

interface LiveState {
  connected: boolean;
  tick: Tick | null;
  samples: Sample[];
  subscribe: (f: (q: QueryEvent) => void) => () => void;
}

const LiveContext = createContext<LiveState | null>(null);
const WINDOW = 300;

export function LiveProvider({ children }: { children: ReactNode }) {
  const [connected, setConnected] = useState(false);
  const [tick, setTick] = useState<Tick | null>(null);
  const [samples, setSamples] = useState<Sample[]>([]);
  const listeners = useRef(new Set<(q: QueryEvent) => void>());

  useEffect(() => {
    let closed = false;
    api.series(WINDOW).then((s) => !closed && setSamples(s)).catch(() => {});
    const es = new EventSource("/api/v1/stream", { withCredentials: true });
    es.onopen = () => setConnected(true);
    es.onerror = () => setConnected(false); // EventSource reconnects by itself
    es.addEventListener("tick", (e) => {
      const t = JSON.parse((e as MessageEvent).data) as Tick;
      setTick(t);
      const sample = t.sample;
      if (sample) {
        setSamples((prev) => {
          if (prev.length && prev[prev.length - 1].ts >= sample.ts) return prev;
          const next = [...prev, sample];
          return next.length > WINDOW ? next.slice(next.length - WINDOW) : next;
        });
      }
    });
    es.addEventListener("query", (e) => {
      const q = JSON.parse((e as MessageEvent).data) as QueryEvent;
      listeners.current.forEach((f) => f(q));
    });
    return () => {
      closed = true;
      es.close();
    };
  }, []);

  const subscribe = (f: (q: QueryEvent) => void) => {
    listeners.current.add(f);
    return () => listeners.current.delete(f);
  };

  return <LiveContext.Provider value={{ connected, tick, samples, subscribe }}>{children}</LiveContext.Provider>;
}

export function useLive(): LiveState {
  const v = useContext(LiveContext);
  if (!v) throw new Error("useLive outside LiveProvider");
  return v;
}
