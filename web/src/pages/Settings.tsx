import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Eraser, FileCog, KeyRound, ScrollText } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { ConfirmAction, PageHeader, Panel, PanelHeader } from "@/components/common";
import { api } from "@/lib/api";
import { fmtDateTime } from "@/lib/format";
import { notifyApply } from "./Resilience";

function ConfigEditor() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const cfg = useQuery({ queryKey: ["config"], queryFn: api.config });
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  useEffect(() => {
    if (cfg.data) setText(cfg.data.toml);
  }, [cfg.data]);
  const save = useMutation({
    mutationFn: (toml: string) => api.setConfig(toml),
    onSuccess: (r) => {
      setError("");
      notifyApply(r, t);
      qc.invalidateQueries();
    },
    onError: (e: Error) => setError(e.message),
  });
  const dirty = cfg.data && text !== cfg.data.toml;
  return (
    <Panel>
      <PanelHeader
        title={
          <span className="flex items-center gap-2">
            <FileCog className="text-primary size-4" aria-hidden /> Configuration
          </span>
        }
        subtitle={cfg.data ? `Validated before it is applied, then saved to ${cfg.data.path} (comments in the file are not kept).` : undefined}
        action={
          <div className="flex gap-2">
            <Button size="sm" variant="ghost" disabled={!dirty} onClick={() => cfg.data && setText(cfg.data.toml)}>
              {t("common.cancel")}
            </Button>
            <Button size="sm" disabled={!dirty || save.isPending} onClick={() => save.mutate(text)}>
              Validate & {t("common.save").toLowerCase()}
            </Button>
          </div>
        }
      />
      <div className="px-5 pb-5">
        <Textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          spellCheck={false}
          aria-label="Configuration TOML"
          aria-invalid={!!error}
          className="h-[480px] font-mono text-xs leading-relaxed"
        />
        {error && (
          <p role="alert" className="text-down bg-down/10 mt-3 rounded-lg px-3 py-2 font-mono text-xs whitespace-pre-wrap">
            {error}
          </p>
        )}
      </div>
    </Panel>
  );
}

function PasswordForm() {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const change = useMutation({
    mutationFn: () => api.changePassword(current, next),
    onSuccess: () => {
      toast.success("Password changed. Other sessions were signed out.");
      setCurrent("");
      setNext("");
      setConfirm("");
    },
    onError: (e: Error) => toast.error(e.message),
  });
  const mismatch = confirm.length > 0 && next !== confirm;
  return (
    <Panel>
      <PanelHeader
        title={
          <span className="flex items-center gap-2">
            <KeyRound className="text-primary size-4" aria-hidden /> Admin password
          </span>
        }
        subtitle="Stored as an argon2id hash. At least 10 characters."
      />
      <form
        className="grid gap-3 px-5 pb-5"
        onSubmit={(e) => {
          e.preventDefault();
          if (!mismatch) change.mutate();
        }}
      >
        <div className="grid gap-1.5">
          <Label htmlFor="pw-current">Current password</Label>
          <Input id="pw-current" type="password" autoComplete="current-password" value={current} onChange={(e) => setCurrent(e.target.value)} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="pw-new">New password</Label>
          <Input id="pw-new" type="password" autoComplete="new-password" minLength={10} value={next} onChange={(e) => setNext(e.target.value)} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="pw-confirm">Repeat new password</Label>
          <Input
            id="pw-confirm"
            type="password"
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            aria-invalid={mismatch}
          />
          {mismatch && <p className="text-down text-xs">Passwords do not match.</p>}
        </div>
        <Button type="submit" className="justify-self-start" disabled={!current || next.length < 10 || mismatch || change.isPending}>
          Change password
        </Button>
      </form>
    </Panel>
  );
}

function AuditLog() {
  const audit = useQuery({ queryKey: ["audit"], queryFn: () => api.audit(200), refetchInterval: 10000 });
  return (
    <Panel>
      <PanelHeader
        title={
          <span className="flex items-center gap-2">
            <ScrollText className="text-primary size-4" aria-hidden /> Audit log
          </span>
        }
        subtitle="Mode changes, logins and configuration edits."
      />
      <div className="max-h-[420px] overflow-auto">
        <Table>
          <TableHeader className="bg-card/95 sticky top-0 backdrop-blur">
            <TableRow>
              <TableHead className="w-44">When</TableHead>
              <TableHead className="w-28">Who</TableHead>
              <TableHead className="w-36">Action</TableHead>
              <TableHead>Detail</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(audit.data ?? []).map((e) => (
              <TableRow key={e.id} className="text-xs">
                <TableCell className="text-muted-foreground tabular-nums">{fmtDateTime(e.ts)}</TableCell>
                <TableCell className="font-mono">{e.actor}</TableCell>
                <TableCell className="font-medium">{e.action}</TableCell>
                <TableCell className="text-muted-foreground">{e.detail}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </Panel>
  );
}

export function Settings() {
  const { t } = useTranslation();
  const flush = useMutation({
    mutationFn: api.flushCache,
    onSuccess: () => toast.success("Cache cleared"),
    onError: (e: Error) => toast.error(e.message),
  });
  return (
    <div>
      <PageHeader
        title={t("nav.settings")}
        action={
          <ConfirmAction
            trigger={
              <Button variant="outline">
                <Eraser className="size-4" /> Flush cache
              </Button>
            }
            title="Clear the whole cache?"
            description="Every answer will be fetched again. During an outage this throws away answers that could have been served stale."
            confirm="Flush cache"
            destructive
            onConfirm={() => flush.mutate()}
          />
        }
      />
      <div className="grid gap-4 xl:grid-cols-3">
        <div className="xl:col-span-2">
          <ConfigEditor />
        </div>
        <PasswordForm />
      </div>
      <div className="mt-4">
        <AuditLog />
      </div>
    </div>
  );
}
