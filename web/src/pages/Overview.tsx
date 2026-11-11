import { useQuery } from "@tanstack/react-query";
import { Ban, Database, Hourglass, Timer, Zap } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Area, AreaChart, CartesianGrid, Legend, Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { Gauge, Panel, PanelHeader, Skeletonish, StatCard, StatusBanner, StatusDot, chartTooltip } from "@/components/common";
import { api } from "@/lib/api";
import { fmtCompact, fmtInt, fmtMs, fmtPct, fmtTime } from "@/lib/format";
import { useLive } from "@/lib/live";

export function Overview() {
  const { t } = useTranslation();
  const { tick, samples } = useLive();
  const overview = useQuery({ queryKey: ["overview"], queryFn: api.overview, refetchInterval: 5000 });
  const summary = tick?.summary ?? overview.data?.summary;
  const mode = tick?.mode ?? overview.data?.mode;
  const cacheEntries = tick?.cache_entries ?? overview.data?.cache_entries ?? 0;

  if (!summary || !mode) return <Skeletonish />;

  const chartData = samples.map((s) => ({ ...s, time: fmtTime(s.ts) }));
  const ups = overview.data?.upstreams ?? [];

  return (
    <div>
      <StatusBanner mode={mode} />

      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard label={t("overview.qps")} value={summary.qps} format={(n) => n.toFixed(n < 10 ? 1 : 0)} icon={<Zap className="size-4" />} hint={`1-minute average · ${t("overview.totalQueries").toLowerCase()} ${fmtCompact(summary.totals.queries)}`} />
        <StatCard label={t("overview.cacheEntries")} value={cacheEntries} format={fmtInt} icon={<Database className="size-4" />} hint={`${fmtCompact(summary.totals.prefetches)} prefetched`} />
        <StatCard
          label={t("overview.blocked")}
          value={summary.totals.blocked}
          format={fmtInt}
          icon={<Ban className="size-4" />}
          tone="warn"
          hint={`${fmtCompact(summary.totals.rate_limited)} rate-limited · ${fmtCompact(summary.totals.refused)} refused`}
        />
        <StatCard
          label={t("overview.stale")}
          value={summary.totals.stale_served}
          format={fmtInt}
          icon={<Hourglass className="size-4" />}
          tone={summary.totals.stale_served > 0 ? "warn" : "good"}
          hint={`${fmtCompact(summary.totals.local_answers)} local · ${fmtCompact(summary.totals.recursive_answers)} recursive`}
        />
      </div>

      <div className="mt-4 grid gap-4 xl:grid-cols-3">
        <Panel className="xl:col-span-2">
          <PanelHeader title={t("overview.traffic")} subtitle={t("overview.trafficSub")} />
          <div className="h-64 px-2 pb-3" role="img" aria-label="Queries per second chart">
            <ResponsiveContainer>
              <AreaChart data={chartData} margin={{ top: 8, right: 12, left: 0, bottom: 0 }}>
                <defs>
                  <linearGradient id="qps" x1="0" x2="0" y1="0" y2="1">
                    <stop offset="0%" stopColor="var(--chart-1)" stopOpacity={0.45} />
                    <stop offset="100%" stopColor="var(--chart-1)" stopOpacity={0} />
                  </linearGradient>
                  <linearGradient id="hits" x1="0" x2="0" y1="0" y2="1">
                    <stop offset="0%" stopColor="var(--chart-2)" stopOpacity={0.3} />
                    <stop offset="100%" stopColor="var(--chart-2)" stopOpacity={0} />
                  </linearGradient>
                </defs>
                <CartesianGrid stroke="var(--grid)" vertical={false} />
                <XAxis dataKey="time" tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} tickLine={false} axisLine={false} minTickGap={40} />
                <YAxis tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} tickLine={false} axisLine={false} width={44} allowDecimals={false} />
                <Tooltip {...chartTooltip} />
                <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12, color: "var(--muted-foreground)" }} />
                <Area type="monotone" dataKey="qps" name="queries/s" stroke="var(--chart-1)" fill="url(#qps)" strokeWidth={2} isAnimationActive={false} />
                <Area type="monotone" dataKey="hits" name="cache hits/s" stroke="var(--chart-2)" fill="url(#hits)" strokeWidth={1.5} isAnimationActive={false} />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        </Panel>

        <Panel>
          <PanelHeader title={t("overview.hitRatio")} subtitle="Fresh cache hits, last minute" />
          <div className="px-5 pt-2 pb-5">
            <Gauge value={summary.hit_ratio} label={t("overview.hitRatio")} />
            <dl className="mt-6 grid grid-cols-3 gap-2 text-center">
              {(
                [
                  ["p50", summary.p50],
                  ["p95", summary.p95],
                  ["p99", summary.p99],
                ] as const
              ).map(([k, v]) => (
                <div key={k} className="bg-muted/40 rounded-xl py-2.5">
                  <dt className="text-muted-foreground text-[11px] font-medium uppercase">{k}</dt>
                  <dd className="mt-0.5 text-sm font-semibold tabular-nums">{fmtMs(v)}</dd>
                </div>
              ))}
            </dl>
            <p className="text-muted-foreground mt-3 flex items-center gap-1.5 text-xs">
              <Timer className="size-3.5" aria-hidden /> {t("overview.latency")} · {fmtPct(summary.hit_ratio)} {t("overview.hitRatio").toLowerCase()}
            </p>
          </div>
        </Panel>
      </div>

      <div className="mt-4 grid gap-4 xl:grid-cols-3">
        <Panel className="xl:col-span-2">
          <PanelHeader title={t("overview.latencyChart")} subtitle="Per second, milliseconds (log scale)" />
          <div className="h-56 px-2 pb-3" role="img" aria-label="Latency percentiles chart">
            <ResponsiveContainer>
              <LineChart data={chartData} margin={{ top: 8, right: 12, left: 0, bottom: 0 }}>
                <CartesianGrid stroke="var(--grid)" vertical={false} />
                <XAxis dataKey="time" tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} tickLine={false} axisLine={false} minTickGap={40} />
                <YAxis
                  scale="log"
                  domain={[0.01, "auto"]}
                  allowDataOverflow
                  tick={{ fill: "var(--muted-foreground)", fontSize: 11 }}
                  tickLine={false}
                  axisLine={false}
                  width={44}
                  tickFormatter={(v: number) => (v < 1 ? v.toFixed(2) : v.toFixed(0))}
                />
                <Tooltip {...chartTooltip} formatter={(v) => fmtMs(Number(v))} />
                <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12, color: "var(--muted-foreground)" }} />
                <Line dataKey={(d: { p50: number }) => Math.max(d.p50, 0.01)} name="p50" stroke="var(--chart-2)" dot={false} strokeWidth={1.8} isAnimationActive={false} />
                <Line dataKey={(d: { p95: number }) => Math.max(d.p95, 0.01)} name="p95" stroke="var(--chart-4)" dot={false} strokeWidth={1.5} isAnimationActive={false} />
                <Line dataKey={(d: { p99: number }) => Math.max(d.p99, 0.01)} name="p99" stroke="var(--chart-5)" dot={false} strokeWidth={1.5} isAnimationActive={false} />
              </LineChart>
            </ResponsiveContainer>
          </div>
        </Panel>

        <Panel>
          <PanelHeader title={t("overview.upstreamHealth")} subtitle={`${ups.filter((u) => u.healthy).length} of ${ups.length} healthy`} />
          <ul className="space-y-2 px-5 pb-5">
            {ups.map((u) => (
              <li key={u.addr} className="bg-muted/30 flex items-center gap-3 rounded-xl px-3 py-2.5">
                <StatusDot state={!u.enabled ? "idle" : u.healthy ? "good" : u.srtt_ms === null ? "idle" : "bad"} pulse={u.healthy} />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium">{u.name}</div>
                  <div className="text-muted-foreground truncate font-mono text-[11px]">
                    {u.addr} · {u.international ? "international" : "local"}
                  </div>
                </div>
                <div className="text-right text-xs tabular-nums">
                  <div className="font-semibold">{u.srtt_ms === null ? "—" : fmtMs(u.srtt_ms)}</div>
                  <div className="text-muted-foreground">{fmtCompact(u.queries)} q</div>
                </div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
    </div>
  );
}
