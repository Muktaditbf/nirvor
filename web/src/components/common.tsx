import { animate, motion, useMotionValue, useReducedMotion, useTransform } from "framer-motion";
import { AlertTriangle, CheckCircle2, ShieldAlert } from "lucide-react";
import { useEffect, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { ModeState } from "@/lib/api";
import { fmtAgo } from "@/lib/format";
import { cn } from "@/lib/utils";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";

/** Card with a subtle glass surface. */
export function Panel({ className, children, ...rest }: React.HTMLAttributes<HTMLElement>) {
  return (
    <section
      className={cn("glass rounded-2xl border shadow-[0_1px_0_0_oklch(1_0_0/0.04)_inset,0_8px_24px_-12px_oklch(0_0_0/0.35)]", className)}
      {...rest}
    >
      {children}
    </section>
  );
}

export function PanelHeader({ title, subtitle, action }: { title: ReactNode; subtitle?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-3 px-5 pt-4 pb-2">
      <div className="min-w-0">
        <h2 className="text-sm font-semibold tracking-tight">{title}</h2>
        {subtitle && <p className="text-muted-foreground mt-0.5 text-xs">{subtitle}</p>}
      </div>
      {action}
    </div>
  );
}

export function PageHeader({ title, subtitle, action }: { title: string; subtitle?: string; action?: ReactNode }) {
  return (
    <header className="mb-6 flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {subtitle && <p className="text-muted-foreground mt-1 max-w-2xl text-sm">{subtitle}</p>}
      </div>
      {action}
    </header>
  );
}

/** Number that eases to its new value. */
export function AnimatedNumber({ value, format }: { value: number; format: (n: number) => string }) {
  const reduce = useReducedMotion();
  const mv = useMotionValue(value);
  const text = useTransform(mv, (v) => format(v));
  useEffect(() => {
    if (reduce) {
      mv.set(value);
      return;
    }
    const c = animate(mv, value, { duration: 0.6, ease: [0.22, 1, 0.36, 1] });
    return () => c.stop();
  }, [value, reduce, mv]);
  return <motion.span className="tabular-nums">{text}</motion.span>;
}

export function StatCard({
  label,
  value,
  format,
  hint,
  icon,
  tone = "default",
}: {
  label: string;
  value: number;
  format: (n: number) => string;
  hint?: ReactNode;
  icon?: ReactNode;
  tone?: "default" | "good" | "warn" | "bad";
}) {
  const toneClass = { default: "text-primary", good: "text-healthy", warn: "text-degraded", bad: "text-down" }[tone];
  return (
    <Panel className="p-5">
      <div className="text-muted-foreground flex items-center justify-between text-xs font-medium tracking-wide uppercase">
        <span>{label}</span>
        <span className={cn("opacity-80", toneClass)} aria-hidden>
          {icon}
        </span>
      </div>
      <div className="mt-3 text-3xl font-semibold tracking-tight">
        <AnimatedNumber value={value} format={format} />
      </div>
      {hint && <div className="text-muted-foreground mt-1.5 text-xs">{hint}</div>}
    </Panel>
  );
}

/** Half-ring gauge for a 0..1 ratio. */
export function Gauge({ value, label }: { value: number; label: string }) {
  const r = 52;
  const len = Math.PI * r;
  const v = Math.max(0, Math.min(1, value || 0));
  const color = v >= 0.8 ? "var(--healthy)" : v >= 0.5 ? "var(--degraded)" : "var(--down)";
  return (
    <div className="flex flex-col items-center" role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(v * 100)} aria-label={label}>
      <svg viewBox="0 0 128 72" className="w-full max-w-[200px]">
        <path d="M 12 66 A 52 52 0 0 1 116 66" fill="none" stroke="var(--grid)" strokeWidth="10" strokeLinecap="round" />
        <motion.path
          d="M 12 66 A 52 52 0 0 1 116 66"
          fill="none"
          stroke={color}
          strokeWidth="10"
          strokeLinecap="round"
          strokeDasharray={len}
          initial={{ strokeDashoffset: len }}
          animate={{ strokeDashoffset: len * (1 - v) }}
          transition={{ duration: 0.8, ease: [0.22, 1, 0.36, 1] }}
        />
      </svg>
      <div className="-mt-9 text-2xl font-semibold tabular-nums">
        <AnimatedNumber value={v * 100} format={(n) => `${n.toFixed(1)}%`} />
      </div>
    </div>
  );
}

/** Tiny line chart for table rows. */
export function Sparkline({ data, className }: { data: number[]; className?: string }) {
  const w = 120;
  const h = 28;
  const max = Math.max(1, ...data);
  const step = data.length > 1 ? w / (data.length - 1) : w;
  const pts = data.map((v, i) => `${(i * step).toFixed(1)},${(h - 2 - (v / max) * (h - 4)).toFixed(1)}`).join(" ");
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className={cn("h-7 w-[120px]", className)} aria-hidden>
      <polyline points={`0,${h} ${pts} ${w},${h}`} fill="var(--primary)" fillOpacity="0.12" stroke="none" />
      <polyline points={pts} fill="none" stroke="var(--primary)" strokeWidth="1.5" strokeLinejoin="round" />
    </svg>
  );
}

export function StatusDot({ state, pulse }: { state: "good" | "warn" | "bad" | "idle"; pulse?: boolean }) {
  const c = { good: "bg-healthy", warn: "bg-degraded", bad: "bg-down", idle: "bg-muted-foreground" }[state];
  return (
    <span className="relative inline-flex size-2.5" aria-hidden>
      {pulse && <span className={cn("absolute inline-flex size-full animate-ping rounded-full opacity-60", c)} />}
      <span className={cn("relative inline-flex size-2.5 rounded-full", c)} />
    </span>
  );
}

export function ModeBadge({ mode }: { mode: ModeState["mode"] }) {
  const { t } = useTranslation();
  const style = {
    normal: "bg-healthy/15 text-healthy ring-healthy/30",
    degraded: "bg-degraded/15 text-degraded ring-degraded/30",
    isolation: "bg-down/15 text-down ring-down/30",
  }[mode];
  return (
    <span className={cn("inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ring-1", style)}>
      <StatusDot state={mode === "normal" ? "good" : mode === "degraded" ? "warn" : "bad"} pulse={mode !== "normal"} />
      {t(`mode.${mode}`)}
    </span>
  );
}

/** Prominent banner for the operating mode. */
export function StatusBanner({ mode }: { mode: ModeState }) {
  const { t } = useTranslation();
  const conf = {
    normal: { icon: CheckCircle2, cls: "from-healthy/20 border-healthy/30 text-healthy" },
    degraded: { icon: AlertTriangle, cls: "from-degraded/20 border-degraded/35 text-degraded" },
    isolation: { icon: ShieldAlert, cls: "from-down/25 border-down/40 text-down" },
  }[mode.mode];
  const Icon = conf.icon;
  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: -6 }}
      animate={{ opacity: 1, y: 0 }}
      role="status"
      aria-live="polite"
      className={cn("mb-6 flex flex-wrap items-center gap-4 rounded-2xl border bg-gradient-to-r to-transparent px-5 py-4", conf.cls)}
    >
      <Icon className="size-7 shrink-0" aria-hidden />
      <div className="min-w-0 flex-1">
        <div className="text-foreground flex flex-wrap items-center gap-2 text-base font-semibold">
          {t(`mode.${mode.mode}`)}
          {mode.control !== "auto" && (
            <span className="text-muted-foreground rounded-md border px-1.5 py-0.5 text-[11px] font-medium">forced by operator</span>
          )}
        </div>
        <p className="text-muted-foreground text-sm">
          {t(`mode.${mode.mode}_desc`)} {mode.control === "auto" && <span className="opacity-80">({mode.reason})</span>}
        </p>
      </div>
      <div className="text-muted-foreground basis-full text-xs sm:basis-auto sm:text-right">
        <div>
          International upstreams{" "}
          <span className="text-foreground font-semibold tabular-nums">
            {mode.international_healthy}/{mode.international_total}
          </span>
        </div>
        <div>{t("common.since", { when: fmtAgo(mode.since) })}</div>
      </div>
    </motion.div>
  );
}

/** Button that asks for confirmation first. */
export function ConfirmAction({
  trigger,
  title,
  description,
  confirm,
  destructive,
  onConfirm,
}: {
  trigger: ReactNode;
  title: string;
  description: ReactNode;
  confirm: string;
  destructive?: boolean;
  onConfirm: () => void;
}) {
  const { t } = useTranslation();
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>{trigger}</AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
          <AlertDialogAction
            onClick={onConfirm}
            className={cn(destructive && "bg-destructive hover:bg-destructive/90 text-white")}
          >
            {confirm}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

export function EmptyState({ children }: { children: ReactNode }) {
  return <div className="text-muted-foreground px-5 py-10 text-center text-sm">{children}</div>;
}

/** Tooltip style shared by all charts. */
export const chartTooltip = {
  contentStyle: {
    background: "var(--popover)",
    border: "1px solid var(--border)",
    borderRadius: 10,
    fontSize: 12,
    color: "var(--popover-foreground)",
  },
  labelStyle: { color: "var(--muted-foreground)" },
  cursor: { stroke: "var(--border)" },
};

/** Placeholder while the first data loads. */
export function Skeletonish() {
  return (
    <div className="space-y-4" aria-busy="true" aria-label="Loading">
      <div className="bg-muted/40 h-20 animate-pulse rounded-2xl" />
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        {[0, 1, 2, 3].map((i) => (
          <div key={i} className="bg-muted/40 h-28 animate-pulse rounded-2xl" />
        ))}
      </div>
      <div className="bg-muted/40 h-72 animate-pulse rounded-2xl" />
    </div>
  );
}
