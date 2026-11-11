export const fmtInt = (n: number) => new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(n);

export function fmtCompact(n: number) {
  return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(n);
}

export function fmtMs(ms: number) {
  if (!Number.isFinite(ms) || ms <= 0) return "0 ms";
  if (ms < 1) return `${(ms * 1000).toFixed(0)} µs`;
  if (ms < 10) return `${ms.toFixed(2)} ms`;
  if (ms < 1000) return `${ms.toFixed(0)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export function fmtPct(r: number) {
  return `${(r * 100).toFixed(1)}%`;
}

export function fmtAgo(unix: number | null | undefined) {
  if (!unix) return "never";
  const s = Math.max(0, Math.round(Date.now() / 1000 - unix));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

export function fmtTime(unix: number) {
  return new Date(unix * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

export function fmtDateTime(unix: number) {
  return new Date(unix * 1000).toLocaleString();
}

export function fmtBytes(n: number | null | undefined) {
  if (!n) return "—";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}
