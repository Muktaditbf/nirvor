import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { CartesianGrid, Legend, Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { ConfirmAction, PageHeader, Panel, PanelHeader, Skeletonish, StatusDot, chartTooltip } from "@/components/common";
import { api, type UpstreamSpec, type UpstreamStatus } from "@/lib/api";
import { fmtCompact, fmtMs, fmtTime } from "@/lib/format";
import { notifyApply } from "./Resilience";

const COLORS = ["var(--chart-1)", "var(--chart-2)", "var(--chart-3)", "var(--chart-4)", "var(--chart-5)"];

function UpstreamDialog({
  open,
  initial,
  onClose,
  onSave,
}: {
  open: boolean;
  initial: UpstreamSpec | null;
  onClose: () => void;
  onSave: (u: UpstreamSpec) => void;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState<UpstreamSpec>(initial ?? { name: "", addr: "", international: true, enabled: true });
  const [error, setError] = useState("");
  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const addr = form.addr.includes(":") && !form.addr.startsWith("[") && form.addr.split(":").length > 2 ? `[${form.addr}]:53` : form.addr;
    const withPort = /:\d+$/.test(addr) ? addr : `${addr}:53`;
    if (!form.name.trim()) return setError("Give it a name.");
    if (!/^(\d{1,3}(\.\d{1,3}){3}|\[[0-9a-fA-F:]+\]):\d+$/.test(withPort)) return setError("Address must be an IP, optionally with :port.");
    onSave({ ...form, name: form.name.trim(), addr: withPort });
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <form onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{initial ? "Edit upstream" : "Add upstream"}</DialogTitle>
            <DialogDescription>Plain DNS over UDP/TCP. Healthy upstreams are tried fastest first.</DialogDescription>
          </DialogHeader>
          <div className="my-5 grid gap-4">
            <div className="grid gap-2">
              <Label htmlFor="up-name">Name</Label>
              <Input id="up-name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="BDIX resolver" />
            </div>
            <div className="grid gap-2">
              <Label htmlFor="up-addr">Address</Label>
              <Input id="up-addr" className="font-mono" value={form.addr} onChange={(e) => setForm({ ...form, addr: e.target.value })} placeholder="103.0.0.53:53" />
            </div>
            <div className="flex items-center justify-between rounded-xl border p-3">
              <div>
                <Label htmlFor="up-intl">International</Label>
                <p className="text-muted-foreground text-xs">Reached over international links. Skipped during isolation; used to detect it.</p>
              </div>
              <Switch id="up-intl" checked={form.international} onCheckedChange={(v) => setForm({ ...form, international: v })} />
            </div>
            <div className="flex items-center justify-between rounded-xl border p-3">
              <Label htmlFor="up-enabled">Enabled</Label>
              <Switch id="up-enabled" checked={form.enabled} onCheckedChange={(v) => setForm({ ...form, enabled: v })} />
            </div>
            {error && (
              <p role="alert" className="text-down text-sm">
                {error}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button type="submit">{t("common.save")}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function HealthCard({ u, index, onEdit, onDelete }: { u: UpstreamStatus; index: number; onEdit: () => void; onDelete: () => void }) {
  const state = !u.enabled ? "idle" : u.healthy ? "good" : u.srtt_ms === null && !u.failures ? "idle" : "bad";
  const okChecks = u.history.filter((h) => h.ok).length;
  return (
    <Panel className="p-5">
      <div className="flex items-start gap-3">
        <div className="mt-1.5">
          <StatusDot state={state} pulse={u.healthy} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="font-semibold">{u.name}</h3>
            <span className="text-muted-foreground rounded-md border px-1.5 text-[11px]">{u.international ? "international" : "local"}</span>
            {!u.enabled && <span className="text-muted-foreground rounded-md border px-1.5 text-[11px]">disabled</span>}
          </div>
          <div className="text-muted-foreground font-mono text-xs">{u.addr}</div>
        </div>
        <div className="flex gap-1">
          <Button variant="ghost" size="icon" onClick={onEdit} aria-label={`Edit ${u.name}`}>
            <Pencil className="size-4" />
          </Button>
          <ConfirmAction
            trigger={
              <Button variant="ghost" size="icon" aria-label={`Remove ${u.name}`}>
                <Trash2 className="size-4" />
              </Button>
            }
            title={`Remove ${u.name}?`}
            description="It is removed from the running resolver and from the config file."
            confirm="Remove"
            destructive
            onConfirm={onDelete}
          />
        </div>
      </div>
      <dl className="mt-4 grid grid-cols-2 gap-2 text-center sm:grid-cols-4">
        {[
          ["Latency", u.srtt_ms === null ? "—" : fmtMs(u.srtt_ms)],
          ["Timeout", fmtMs(u.timeout_ms)],
          ["Queries", fmtCompact(u.queries)],
          ["Checks ok", u.history.length ? `${Math.round((okChecks / u.history.length) * 100)}%` : "—"],
        ].map(([k, v]) => (
          <div key={k} className="bg-muted/40 rounded-xl py-2">
            <dt className="text-muted-foreground text-[11px] uppercase">{k}</dt>
            <dd className="text-sm font-semibold tabular-nums">{v}</dd>
          </div>
        ))}
      </dl>
      {u.last_error && !u.healthy && (
        <p className="text-down mt-3 truncate text-xs" title={u.last_error}>
          Last error: {u.last_error}
        </p>
      )}
      <span className="sr-only">Upstream number {index + 1}</span>
    </Panel>
  );
}

export function Upstreams() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const data = useQuery({ queryKey: ["upstreams"], queryFn: api.upstreams, refetchInterval: 5000 });
  const [dialog, setDialog] = useState<{ index: number | null } | null>(null);
  const done = {
    onSuccess: (r: { restart_required: string[] }) => {
      notifyApply(r, t);
      setDialog(null);
      qc.invalidateQueries({ queryKey: ["upstreams"] });
    },
    onError: (e: Error) => toast.error(e.message),
  };
  const save = useMutation({
    mutationFn: ({ index, spec }: { index: number | null; spec: UpstreamSpec }) =>
      index === null ? api.addUpstream(spec) : api.editUpstream(index, spec),
    ...done,
  });
  const remove = useMutation({ mutationFn: (i: number) => api.deleteUpstream(i), ...done });

  if (!data.data) return <Skeletonish />;
  const ups = data.data;

  // Merge each upstream's health-check history into one time series.
  const byTs = new Map<number, Record<string, number | null>>();
  ups.forEach((u) =>
    u.history.forEach((h) => {
      const row = byTs.get(h.ts) ?? {};
      row[u.name] = h.ok ? h.rtt_ms : null;
      byTs.set(h.ts, row);
    }),
  );
  const history = [...byTs.entries()].sort((a, b) => a[0] - b[0]).map(([ts, row]) => ({ time: fmtTime(ts), ...row }));
  const editing = dialog && dialog.index !== null ? ups[dialog.index] : null;

  return (
    <div>
      <PageHeader
        title={t("nav.upstreams")}
        subtitle="Forwarders with active health checks, adaptive timeouts (RFC 6298) and automatic failover."
        action={
          <Button onClick={() => setDialog({ index: null })}>
            <Plus className="size-4" /> {t("common.add")}
          </Button>
        }
      />
      <div className="grid gap-4 md:grid-cols-2 2xl:grid-cols-3">
        {ups.map((u, i) => (
          <HealthCard key={u.addr} u={u} index={i} onEdit={() => setDialog({ index: i })} onDelete={() => remove.mutate(i)} />
        ))}
      </div>
      <Panel className="mt-4">
        <PanelHeader title="Health check latency" subtitle="Round-trip time of each probe, ms. Gaps are failed checks." />
        <div className="h-72 px-2 pb-3" role="img" aria-label="Upstream latency history chart">
          <ResponsiveContainer>
            <LineChart data={history} margin={{ top: 8, right: 12, left: 0, bottom: 0 }}>
              <CartesianGrid stroke="var(--grid)" vertical={false} />
              <XAxis dataKey="time" tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} tickLine={false} axisLine={false} minTickGap={50} />
              <YAxis tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} tickLine={false} axisLine={false} width={44} />
              <Tooltip {...chartTooltip} formatter={(v) => (v === null ? "failed" : fmtMs(Number(v)))} />
              <Legend iconType="circle" iconSize={8} wrapperStyle={{ fontSize: 12, color: "var(--muted-foreground)" }} />
              {ups.map((u, i) => (
                <Line key={u.addr} dataKey={u.name} stroke={COLORS[i % COLORS.length]} dot={false} strokeWidth={1.6} connectNulls={false} isAnimationActive={false} />
              ))}
            </LineChart>
          </ResponsiveContainer>
        </div>
      </Panel>
      {dialog && (
        <UpstreamDialog
          key={dialog.index ?? "new"}
          open
          initial={editing ? { name: editing.name, addr: editing.addr, international: editing.international, enabled: editing.enabled } : null}
          onClose={() => setDialog(null)}
          onSave={(spec) => save.mutate({ index: dialog.index, spec })}
        />
      )}
    </div>
  );
}
