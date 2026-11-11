import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { EmptyState, PageHeader, Panel, PanelHeader, Sparkline } from "@/components/common";
import { api, type TopEntry } from "@/lib/api";
import { fmtInt } from "@/lib/format";

function Ranked({ title, subtitle, data, mono }: { title: string; subtitle: string; data?: TopEntry[]; mono?: boolean }) {
  const max = Math.max(1, ...(data ?? []).map((d) => d.total));
  return (
    <Panel>
      <PanelHeader title={title} subtitle={subtitle} />
      {!data?.length ? (
        <EmptyState>No data yet.</EmptyState>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="w-10">#</TableHead>
              <TableHead>Name</TableHead>
              <TableHead className="hidden w-[140px] sm:table-cell">Last 30 min</TableHead>
              <TableHead className="w-24 text-right">Queries</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.map((d, i) => (
              <TableRow key={d.key}>
                <TableCell className="text-muted-foreground tabular-nums">{i + 1}</TableCell>
                <TableCell className="max-w-0">
                  <div className={mono ? "truncate font-mono text-xs" : "truncate text-sm font-medium"} title={d.key}>
                    {d.key}
                  </div>
                  <div className="bg-muted mt-1.5 h-1 overflow-hidden rounded-full" aria-hidden>
                    <div className="bg-primary/70 h-full rounded-full" style={{ width: `${(d.total / max) * 100}%` }} />
                  </div>
                </TableCell>
                <TableCell className="hidden sm:table-cell">
                  <Sparkline data={d.spark} />
                </TableCell>
                <TableCell className="text-right font-semibold tabular-nums">{fmtInt(d.total)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </Panel>
  );
}

export function Top() {
  const { t } = useTranslation();
  const domains = useQuery({ queryKey: ["top", "domains"], queryFn: () => api.top("domains", 25), refetchInterval: 5000 });
  const clients = useQuery({ queryKey: ["top", "clients"], queryFn: () => api.top("clients", 25), refetchInterval: 5000 });
  return (
    <div>
      <PageHeader title={t("nav.top")} subtitle="Most queried names and busiest clients over the last 30 minutes. Client data follows the privacy setting." />
      <div className="grid gap-4 xl:grid-cols-2">
        <Ranked title="Top domains" subtitle="By query count" data={domains.data} />
        <Ranked title="Top clients" subtitle="By query count (anonymized unless full logging is on)" data={clients.data} mono />
      </div>
    </div>
  );
}
