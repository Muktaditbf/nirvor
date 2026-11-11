import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, RefreshCw, Trash2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { EmptyState, PageHeader, Panel, PanelHeader, Skeletonish, StatusDot } from "@/components/common";
import { api, type BlocklistSource, type PolicyConfig } from "@/lib/api";
import { fmtAgo, fmtInt } from "@/lib/format";
import { cn } from "@/lib/utils";
import { notifyApply } from "./Resilience";

function NumberField({ id, label, hint, value, onChange }: { id: string; label: string; hint?: string; value: number; onChange: (n: number) => void }) {
  return (
    <div className="grid gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <Input id={id} type="number" min={0} value={value} onChange={(e) => onChange(Math.max(0, Number(e.target.value) || 0))} className="tabular-nums" />
      {hint && <p className="text-muted-foreground text-xs">{hint}</p>}
    </div>
  );
}

export function Policy() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const data = useQuery({ queryKey: ["policy"], queryFn: api.policy, refetchInterval: 10000 });
  const [draft, setDraft] = useState<PolicyConfig | null>(null);
  const [cidr, setCidr] = useState("");
  const [list, setList] = useState<{ name: string; where: string }>({ name: "", where: "" });

  useEffect(() => {
    if (data.data && !draft) setDraft(structuredClone(data.data.policy));
  }, [data.data, draft]);

  const save = useMutation({
    mutationFn: (p: PolicyConfig) => api.setPolicy(p),
    onSuccess: (r) => {
      notifyApply(r, t);
      setDraft(null);
      qc.invalidateQueries({ queryKey: ["policy"] });
    },
    onError: (e: Error) => toast.error(e.message),
  });
  const reload = useMutation({
    mutationFn: api.reloadBlocklists,
    onSuccess: () => {
      toast.success("Blocklists reloaded");
      qc.invalidateQueries({ queryKey: ["policy"] });
    },
    onError: (e: Error) => toast.error(e.message),
  });

  if (!data.data || !draft) return <Skeletonish />;
  const view = data.data;
  const dirty = JSON.stringify(draft) !== JSON.stringify(view.policy);
  const set = (f: (p: PolicyConfig) => void) => {
    const next = structuredClone(draft);
    f(next);
    setDraft(next);
  };

  const addCidr = (e: React.FormEvent) => {
    e.preventDefault();
    const v = cidr.trim();
    if (!/^[0-9a-fA-F:.]+\/\d{1,3}$/.test(v)) return toast.error("Use CIDR notation, e.g. 103.0.0.0/22 or 2401:db00::/32");
    if (/\/0$/.test(v)) return toast.error("A /0 range would make an open resolver.");
    set((p) => {
      if (!p.allow.includes(v)) p.allow.push(v);
    });
    setCidr("");
  };

  const addList = (e: React.FormEvent) => {
    e.preventDefault();
    const where = list.where.trim();
    if (!list.name.trim() || !where) return;
    const src: BlocklistSource = /^https?:\/\//.test(where)
      ? { name: list.name.trim(), url: where, path: null, enabled: true, refresh_secs: 21600 }
      : { name: list.name.trim(), path: where, url: null, enabled: true, refresh_secs: 21600 };
    set((p) => p.blocklists.push(src));
    setList({ name: "", where: "" });
  };

  return (
    <div>
      <PageHeader
        title={t("nav.policy")}
        subtitle="Who may query, how fast, and what is blocked. Edits apply immediately and are saved to the config file."
        action={
          <div className="flex gap-2">
            <Button variant="ghost" disabled={!dirty} onClick={() => setDraft(structuredClone(view.policy))}>
              {t("common.cancel")}
            </Button>
            <Button disabled={!dirty || save.isPending} onClick={() => save.mutate(draft)}>
              {t("common.save")}
            </Button>
          </div>
        }
      />

      <div className="grid gap-4 xl:grid-cols-2">
        <Panel>
          <PanelHeader title="Access control (ACL)" subtitle="Only these subscriber ranges may query. Everything else is denied." />
          <div className="px-5 pb-5">
            <form onSubmit={addCidr} className="mb-3 flex gap-2">
              <Input value={cidr} onChange={(e) => setCidr(e.target.value)} placeholder="103.0.0.0/22" className="font-mono" aria-label="Range to allow" />
              <Button type="submit" variant="outline">
                <Plus className="size-4" /> {t("common.add")}
              </Button>
            </form>
            <ul className="flex flex-wrap gap-2" aria-label="Allowed ranges">
              {draft.allow.map((n) => (
                <li key={n} className="bg-muted/50 inline-flex items-center gap-1 rounded-lg border py-1 pr-1 pl-2.5 font-mono text-xs">
                  {n}
                  <button
                    type="button"
                    aria-label={`Remove ${n}`}
                    className="hover:bg-down/20 hover:text-down rounded p-0.5"
                    onClick={() => set((p) => (p.allow = p.allow.filter((x) => x !== n)))}
                  >
                    <X className="size-3.5" />
                  </button>
                </li>
              ))}
            </ul>
            <div className="mt-4 grid gap-1.5">
              <Label>Denied queries</Label>
              <Select value={draft.deny_action} onValueChange={(v) => set((p) => (p.deny_action = v as PolicyConfig["deny_action"]))}>
                <SelectTrigger className="w-60">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="drop">Drop silently (recommended)</SelectItem>
                  <SelectItem value="refuse">Answer REFUSED</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        </Panel>

        <Panel>
          <PanelHeader title="Rate limits" subtitle="Per client, plus response rate limiting (RRL) against amplification abuse. 0 turns a limit off." />
          <div className="grid gap-4 px-5 pb-5 sm:grid-cols-2">
            <NumberField id="qps" label="Queries/s per client" value={draft.rate_limit.per_client_qps} onChange={(n) => set((p) => (p.rate_limit.per_client_qps = n))} />
            <NumberField id="burst" label="Burst" hint="Short bursts allowed above the rate" value={draft.rate_limit.burst} onChange={(n) => set((p) => (p.rate_limit.burst = n))} />
            <NumberField id="rrl" label="RRL responses/s" hint="Identical answers per client network" value={draft.rrl.responses_per_second} onChange={(n) => set((p) => (p.rrl.responses_per_second = n))} />
            <NumberField id="slip" label="RRL slip" hint="Every Nth limited answer is sent truncated" value={draft.rrl.slip} onChange={(n) => set((p) => (p.rrl.slip = n))} />
          </div>
        </Panel>
      </div>

      <Panel className="mt-4">
        <PanelHeader
          title="Blocklists"
          subtitle={`${fmtInt(view.blocked_entries)} entries in use. Files are re-read when they change; URLs refresh on their schedule.`}
          action={
            <Button size="sm" variant="outline" onClick={() => reload.mutate()} disabled={reload.isPending}>
              <RefreshCw className={cn("size-3.5", reload.isPending && "animate-spin")} /> {t("common.reload")}
            </Button>
          }
        />
        <div className="grid gap-4 px-5 pb-4 md:grid-cols-3">
          <div className="grid gap-1.5">
            <Label>Blocked names get</Label>
            <Select value={draft.block_response} onValueChange={(v) => set((p) => (p.block_response = v as PolicyConfig["block_response"]))}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="nxdomain">NXDOMAIN</SelectItem>
                <SelectItem value="refused">REFUSED</SelectItem>
                <SelectItem value="null">0.0.0.0 / ::</SelectItem>
                <SelectItem value="redirect">Redirect to block page</SelectItem>
              </SelectContent>
            </Select>
          </div>
          {draft.block_response === "redirect" && (
            <>
              <div className="grid gap-1.5">
                <Label htmlFor="r4">Block page IPv4</Label>
                <Input id="r4" className="font-mono" value={draft.redirect_ipv4 ?? ""} onChange={(e) => set((p) => (p.redirect_ipv4 = e.target.value || null))} />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="r6">Block page IPv6</Label>
                <Input id="r6" className="font-mono" value={draft.redirect_ipv6 ?? ""} onChange={(e) => set((p) => (p.redirect_ipv6 = e.target.value || null))} />
              </div>
            </>
          )}
        </div>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>List</TableHead>
              <TableHead className="hidden md:table-cell">Source</TableHead>
              <TableHead className="w-24 text-right">Entries</TableHead>
              <TableHead className="w-32">Loaded</TableHead>
              <TableHead className="w-20">On</TableHead>
              <TableHead className="w-12" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {draft.blocklists.map((b, i) => {
              const st = view.blocklists.find((s) => s.name === b.name);
              return (
                <TableRow key={`${b.name}-${i}`}>
                  <TableCell>
                    <div className="flex items-center gap-2 font-medium">
                      <StatusDot state={!b.enabled ? "idle" : st?.error ? "bad" : st?.loaded_at ? "good" : "idle"} />
                      {b.name}
                    </div>
                    {st?.error && <div className="text-down mt-0.5 text-xs">{st.error}</div>}
                  </TableCell>
                  <TableCell className="text-muted-foreground hidden max-w-[40ch] truncate font-mono text-xs md:table-cell">{b.url ?? b.path}</TableCell>
                  <TableCell className="text-right tabular-nums">{st ? fmtInt(st.entries) : "—"}</TableCell>
                  <TableCell className="text-muted-foreground text-xs">{fmtAgo(st?.loaded_at)}</TableCell>
                  <TableCell>
                    <Switch checked={b.enabled} aria-label={`Enable ${b.name}`} onCheckedChange={(v) => set((p) => (p.blocklists[i].enabled = v))} />
                  </TableCell>
                  <TableCell>
                    <Button variant="ghost" size="icon" aria-label={`Remove ${b.name}`} onClick={() => set((p) => p.blocklists.splice(i, 1))}>
                      <Trash2 className="size-4" />
                    </Button>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
        {!draft.blocklists.length && <EmptyState>No blocklists. Add operator or regulatory lists below.</EmptyState>}
        <form onSubmit={addList} className="flex flex-wrap gap-2 border-t px-5 py-4">
          <Input className="w-48" placeholder="Name" aria-label="List name" value={list.name} onChange={(e) => setList({ ...list, name: e.target.value })} />
          <Input
            className="min-w-64 flex-1 font-mono"
            placeholder="/etc/nirvor/blocklist.txt or https://…"
            aria-label="File path or URL"
            value={list.where}
            onChange={(e) => setList({ ...list, where: e.target.value })}
          />
          <Button type="submit" variant="outline">
            <Plus className="size-4" /> Add list
          </Button>
        </form>
      </Panel>
    </div>
  );
}
