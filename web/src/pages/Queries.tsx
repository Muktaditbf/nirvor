import { useQuery } from "@tanstack/react-query";
import { Pause, Play, ShieldOff } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { EmptyState, PageHeader, Panel } from "@/components/common";
import { api, type QueryEvent, type Source } from "@/lib/api";
import { fmtMs } from "@/lib/format";
import { useLive } from "@/lib/live";
import { cn } from "@/lib/utils";

const MAX_ROWS = 1000;
const SOURCES: Source[] = ["cache", "upstream", "stale", "local", "blocked", "error"];
const sourceStyle: Record<Source, string> = {
  cache: "bg-healthy/15 text-healthy",
  upstream: "bg-primary/15 text-primary",
  stale: "bg-degraded/15 text-degraded",
  local: "bg-chart-3/15 text-chart-3",
  blocked: "bg-down/15 text-down",
  error: "bg-down/15 text-down",
};

export function Queries() {
  const { t } = useTranslation();
  const { subscribe } = useLive();
  const initial = useQuery({ queryKey: ["queries"], queryFn: () => api.queries(500) });
  const [rows, setRows] = useState<QueryEvent[]>([]);
  const [paused, setPaused] = useState(false);
  const [text, setText] = useState("");
  const [qtype, setQtype] = useState("all");
  const [source, setSource] = useState("all");
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const buffer = useRef<QueryEvent[]>([]);

  useEffect(() => {
    if (initial.data) setRows(initial.data.queries);
  }, [initial.data]);

  // Batch live events and flush a few times per second so heavy traffic stays smooth.
  useEffect(() => {
    const unsub = subscribe((q) => {
      if (!pausedRef.current) buffer.current.push(q);
    });
    const timer = setInterval(() => {
      if (!buffer.current.length) return;
      const fresh = buffer.current.reverse();
      buffer.current = [];
      setRows((prev) => [...fresh, ...prev].slice(0, MAX_ROWS));
    }, 300);
    return () => {
      unsub();
      clearInterval(timer);
    };
  }, [subscribe]);

  const types = useMemo(() => Array.from(new Set(rows.map((r) => r.qtype))).sort(), [rows]);
  const filtered = useMemo(() => {
    const needle = text.trim().toLowerCase();
    return rows.filter(
      (r) =>
        (qtype === "all" || r.qtype === qtype) &&
        (source === "all" || r.source === source) &&
        (!needle || r.name.includes(needle) || r.client.includes(needle) || r.rcode.toLowerCase().includes(needle)),
    );
  }, [rows, text, qtype, source]);

  const privacy = initial.data?.privacy;

  return (
    <div>
      <PageHeader
        title={t("nav.queries")}
        subtitle={
          privacy === "anonymized"
            ? "Client addresses are anonymized (IPv4 /24, IPv6 /48) per the privacy setting."
            : privacy === "full"
              ? "Full client addresses are logged. Change logging.query_log in Settings to anonymize."
              : undefined
        }
        action={
          <Button variant={paused ? "default" : "outline"} onClick={() => setPaused((p) => !p)} aria-pressed={paused}>
            {paused ? <Play className="size-4" /> : <Pause className="size-4" />}
            {paused ? t("common.resume") : t("common.pause")}
          </Button>
        }
      />
      {privacy === "off" ? (
        <Panel>
          <EmptyState>
            <ShieldOff className="mx-auto mb-3 size-8 opacity-60" aria-hidden />
            Query logging is turned off (logging.query_log = "off"). No per-query data is kept.
          </EmptyState>
        </Panel>
      ) : (
        <Panel>
          <div className="flex flex-wrap items-center gap-2 border-b px-4 py-3">
            <Input
              className="h-9 max-w-xs"
              placeholder={`${t("common.search")}: domain, client, rcode`}
              aria-label={t("common.search")}
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
            <Select value={qtype} onValueChange={setQtype}>
              <SelectTrigger className="h-9 w-32" aria-label="Record type">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All types</SelectItem>
                {types.map((ty) => (
                  <SelectItem key={ty} value={ty}>
                    {ty}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={source} onValueChange={setSource}>
              <SelectTrigger className="h-9 w-36" aria-label="Answer source">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All sources</SelectItem>
                {SOURCES.map((s) => (
                  <SelectItem key={s} value={s}>
                    {s}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <span className="text-muted-foreground ml-auto text-xs tabular-nums">
              {filtered.length} of {rows.length}
            </span>
          </div>
          <div className="max-h-[70vh] overflow-auto">
            <Table>
              <TableHeader className="bg-card/95 sticky top-0 backdrop-blur">
                <TableRow>
                  <TableHead className="w-24">Time</TableHead>
                  <TableHead>Domain</TableHead>
                  <TableHead className="w-20">Type</TableHead>
                  <TableHead className="hidden w-40 md:table-cell">Client</TableHead>
                  <TableHead className="w-28">Response</TableHead>
                  <TableHead className="w-24">Source</TableHead>
                  <TableHead className="w-24 text-right">Latency</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {filtered.slice(0, 400).map((r, i) => (
                  <TableRow key={`${r.ts_ms}-${i}-${r.name}`} className="font-mono text-xs">
                    <TableCell className="text-muted-foreground tabular-nums">
                      {new Date(r.ts_ms).toLocaleTimeString([], { hour12: false })}
                    </TableCell>
                    <TableCell className="max-w-[28ch] truncate font-medium" title={r.name}>
                      {r.name || "."}
                    </TableCell>
                    <TableCell>{r.qtype}</TableCell>
                    <TableCell className="text-muted-foreground hidden md:table-cell">{r.client}</TableCell>
                    <TableCell className={cn(r.rcode !== "No Error" && "text-degraded")}>{r.rcode}</TableCell>
                    <TableCell>
                      <span className={cn("rounded-md px-1.5 py-0.5 font-sans text-[11px] font-medium", sourceStyle[r.source])}>{r.source}</span>
                    </TableCell>
                    <TableCell className="text-right tabular-nums">{fmtMs(r.latency_ms)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            {!filtered.length && <EmptyState>No queries match. Send some DNS traffic and they appear here live.</EmptyState>}
          </div>
        </Panel>
      )}
    </div>
  );
}
