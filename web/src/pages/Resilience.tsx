import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Globe2, HardDriveDownload, Pin, RefreshCw, ShieldAlert, X } from "lucide-react";
import type { TFunction } from "i18next";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ConfirmAction, PageHeader, Panel, PanelHeader, Skeletonish, StatusBanner, StatusDot } from "@/components/common";
import { api, type ApplyResult, type Control } from "@/lib/api";
import { fmtAgo, fmtBytes, fmtDateTime, fmtInt } from "@/lib/format";
import { useLive } from "@/lib/live";
import { cn } from "@/lib/utils";

export function notifyApply(r: ApplyResult, t: TFunction) {
  if (r.restart_required.length) toast.warning(t("common.restartNeeded", { list: r.restart_required.join(", ") }));
  else toast.success(t("common.saved"));
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-4 py-1.5 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="text-right font-medium tabular-nums">{children}</dd>
    </div>
  );
}

export function Resilience() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { tick } = useLive();
  const data = useQuery({ queryKey: ["resilience"], queryFn: api.resilience, refetchInterval: 5000 });
  const [newDomain, setNewDomain] = useState("");

  const onError = (e: Error) => toast.error(e.message);
  const setMode = useMutation({
    mutationFn: (c: Control) => api.setMode(c),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["resilience"] }),
    onError,
  });
  const refreshRoot = useMutation({
    mutationFn: api.refreshRoot,
    onSuccess: (r) => {
      toast.success(`Root zone serial ${r.serial} downloaded and verified`);
      qc.invalidateQueries({ queryKey: ["resilience"] });
    },
    onError,
  });
  const snapshot = useMutation({
    mutationFn: api.saveSnapshot,
    onSuccess: (r) => {
      toast.success(`Saved ${fmtInt(r.entries)} cache entries`);
      qc.invalidateQueries({ queryKey: ["resilience"] });
    },
    onError,
  });
  const pinned = useMutation({
    mutationFn: (domains: string[]) => api.setPinned(domains),
    onSuccess: (r) => {
      notifyApply(r, t);
      qc.invalidateQueries({ queryKey: ["resilience"] });
    },
    onError,
  });

  if (!data.data) return <Skeletonish />;
  const d = data.data;
  const mode = tick?.mode ?? d.mode;
  const root = d.root_zone;

  const controls: { value: Control; label: string; desc: string }[] = [
    { value: "auto", label: t("mode.auto"), desc: `Isolate after ${d.isolation.isolation_after_secs}s of total international loss, recover after ${d.isolation.recover_after_secs}s` },
    { value: "normal", label: `Force ${t("mode.normal").toLowerCase()}`, desc: "Ignore health checks and always use international upstreams" },
    { value: "isolation", label: `Force ${t("mode.isolation").toLowerCase()}`, desc: "Local upstreams, recursion from the local root, extended serve-stale, overrides" },
  ];

  const addDomain = (e: React.FormEvent) => {
    e.preventDefault();
    const v = newDomain.trim().toLowerCase().replace(/\.$/, "");
    if (!v || d.pinned_domains.includes(v)) return;
    pinned.mutate([...d.pinned_domains, v]);
    setNewDomain("");
  };

  return (
    <div>
      <PageHeader title={t("nav.resilience")} subtitle="Keep local resolution working when international links fail." />
      <StatusBanner mode={mode} />

      <Panel className="mb-4">
        <PanelHeader title="Isolation mode control" subtitle="Changes are written to the audit log." />
        <div role="radiogroup" aria-label="Isolation mode control" className="grid gap-3 px-5 pb-5 md:grid-cols-3">
          {controls.map((c) => {
            const active = mode.control === c.value;
            const button = (
              <button
                type="button"
                role="radio"
                aria-checked={active}
                disabled={setMode.isPending}
                onClick={c.value === "auto" ? () => setMode.mutate("auto") : undefined}
                className={cn(
                  "w-full rounded-xl border p-4 text-left transition-colors",
                  active ? "border-primary/60 bg-primary/10 ring-primary/30 ring-1" : "hover:bg-accent/40",
                )}
              >
                <div className="flex items-center gap-2 text-sm font-semibold">
                  {c.value === "isolation" && <ShieldAlert className="text-down size-4" aria-hidden />}
                  {c.label}
                </div>
                <p className="text-muted-foreground mt-1 text-xs">{c.desc}</p>
              </button>
            );
            return c.value === "auto" || active ? (
              <div key={c.value}>{button}</div>
            ) : (
              <ConfirmAction
                key={c.value}
                trigger={button}
                title={`${c.label}?`}
                description={
                  c.value === "isolation"
                    ? "International upstreams will be skipped until you switch back. Use this when you know links are down or for drills."
                    : "Automatic isolation is disabled until you switch back to automatic. If international links fail, clients will see timeouts."
                }
                confirm={c.label}
                destructive={c.value === "isolation"}
                onConfirm={() => setMode.mutate(c.value)}
              />
            );
          })}
        </div>
      </Panel>

      <div className="grid gap-4 lg:grid-cols-2">
        <Panel>
          <PanelHeader
            title={
              <span className="flex items-center gap-2">
                <Globe2 className="text-primary size-4" aria-hidden /> Local root zone (RFC 8806)
              </span>
            }
            subtitle="Downloaded from IANA, verified with DNSSEC and ZONEMD before use."
            action={
              <Button size="sm" variant="outline" onClick={() => refreshRoot.mutate()} disabled={refreshRoot.isPending || !root.enabled}>
                <RefreshCw className={cn("size-3.5", refreshRoot.isPending && "animate-spin")} />
                {t("common.refresh")}
              </Button>
            }
          />
          <dl className="px-5 pb-5">
            <Row label="Status">
              <span className="inline-flex items-center gap-2">
                <StatusDot state={!root.enabled ? "idle" : root.active ? "good" : "bad"} />
                {!root.enabled ? "Disabled" : root.active ? "Active" : "Not available"}
              </span>
            </Row>
            <Row label="SOA serial">{root.serial ?? "—"}</Row>
            <Row label="Records / TLDs">
              {fmtInt(root.records)} / {fmtInt(root.tlds)}
            </Row>
            <Row label="Last verified">{fmtAgo(root.last_success)}</Row>
            <Row label="Stops being used">{root.expires_at ? fmtDateTime(root.expires_at) : "—"}</Row>
            {root.last_error && (
              <p role="alert" className="text-down bg-down/10 mt-2 rounded-lg px-3 py-2 text-xs">
                Last attempt failed: {root.last_error}
              </p>
            )}
          </dl>
        </Panel>

        <Panel>
          <PanelHeader
            title={
              <span className="flex items-center gap-2">
                <HardDriveDownload className="text-primary size-4" aria-hidden /> Persistent cache
              </span>
            }
            subtitle={`Saved every ${Math.round(d.snapshot.interval_secs / 60)} min and on shutdown, reloaded on start.`}
            action={
              <Button size="sm" variant="outline" onClick={() => snapshot.mutate()} disabled={snapshot.isPending}>
                Save now
              </Button>
            }
          />
          <dl className="px-5 pb-5">
            <Row label="Answers in memory">{fmtInt(tick?.cache_entries ?? d.cache_entries)}</Row>
            <Row label="Snapshot size">{fmtBytes(d.snapshot.bytes)}</Row>
            <Row label="Last saved">{fmtAgo(d.snapshot.saved_at)}</Row>
            <Row label="File">
              <span className="font-mono text-xs">{d.snapshot.path}</span>
            </Row>
          </dl>
        </Panel>
      </div>

      <Panel className="mt-4">
        <PanelHeader
          title={
            <span className="flex items-center gap-2">
              <Pin className="text-primary size-4" aria-hidden /> Pinned domains
            </span>
          }
          subtitle="Kept warm (A and AAAA refreshed before expiry) and served stale for up to 30 days. Add the local sites your subscribers rely on."
        />
        <div className="px-5 pb-5">
          <form onSubmit={addDomain} className="mb-4 flex max-w-md gap-2">
            <Input value={newDomain} onChange={(e) => setNewDomain(e.target.value)} placeholder="example.com.bd" aria-label="Domain to pin" />
            <Button type="submit" disabled={!newDomain.trim() || pinned.isPending}>
              {t("common.add")}
            </Button>
          </form>
          <ul className="flex flex-wrap gap-2" aria-label="Pinned domains">
            {d.pinned_domains.map((name) => (
              <li key={name} className="bg-muted/50 inline-flex items-center gap-1 rounded-lg border py-1 pr-1 pl-2.5 font-mono text-xs">
                {name}
                <button
                  type="button"
                  className="hover:bg-down/20 hover:text-down rounded p-0.5"
                  aria-label={`Unpin ${name}`}
                  onClick={() => pinned.mutate(d.pinned_domains.filter((x) => x !== name))}
                >
                  <X className="size-3.5" />
                </button>
              </li>
            ))}
            {!d.pinned_domains.length && <li className="text-muted-foreground text-sm">No pinned domains yet.</li>}
          </ul>
        </div>
      </Panel>

      {d.isolation.overrides.length > 0 && (
        <Panel className="mt-4">
          <PanelHeader title="Isolation overrides" subtitle="Answered only during isolation, e.g. OS connectivity checks pointing at the local responder." />
          <ul className="space-y-1 px-5 pb-5 font-mono text-xs">
            {d.isolation.overrides.map((o) => (
              <li key={o.name}>
                <span className="font-semibold">{o.name}</span> → {o.records.join(", ")}
              </li>
            ))}
          </ul>
        </Panel>
      )}
    </div>
  );
}
