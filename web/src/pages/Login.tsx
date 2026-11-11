import { motion } from "framer-motion";
import { KeyRound, Loader2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Panel } from "@/components/common";
import { api } from "@/lib/api";

export function Login({ onDone }: { onDone: () => void }) {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    try {
      await api.login(password);
      onDone();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="grid min-h-dvh place-items-center px-4">
      <motion.div initial={{ opacity: 0, y: 12, scale: 0.98 }} animate={{ opacity: 1, y: 0, scale: 1 }} className="w-full max-w-sm">
        <Panel className="p-7">
          <div className="bg-primary/15 text-primary mb-5 grid size-11 place-items-center rounded-xl">
            <KeyRound className="size-5" aria-hidden />
          </div>
          <h1 className="text-xl font-semibold tracking-tight">{t("login.title")}</h1>
          <p className="text-muted-foreground mt-1 text-sm">{t("login.subtitle")}</p>
          <form onSubmit={submit} className="mt-6 space-y-4">
            <div className="space-y-2">
              <Label htmlFor="password">{t("login.password")}</Label>
              <Input
                id="password"
                type="password"
                autoComplete="current-password"
                autoFocus
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                aria-invalid={!!error}
                aria-describedby={error ? "login-error" : undefined}
              />
            </div>
            {error && (
              <p id="login-error" role="alert" className="text-down text-sm">
                {error}
              </p>
            )}
            <Button type="submit" className="w-full" disabled={busy || !password}>
              {busy && <Loader2 className="size-4 animate-spin" />}
              {t("login.submit")}
            </Button>
          </form>
          <p className="text-muted-foreground mt-5 text-xs">{t("login.firstRun")}</p>
        </Panel>
      </motion.div>
    </div>
  );
}
