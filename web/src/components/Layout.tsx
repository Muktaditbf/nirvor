import { AnimatePresence, motion } from "framer-motion";
import {
  Activity,
  Gauge as GaugeIcon,
  Languages,
  ListOrdered,
  LogOut,
  Menu,
  Moon,
  Network,
  Settings,
  Shield,
  ShieldCheck,
  Sun,
  X,
} from "lucide-react";
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { NavLink, useLocation } from "react-router";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ModeBadge, StatusDot } from "@/components/common";
import { useLive } from "@/lib/live";
import { useTheme } from "@/lib/theme";
import { cn } from "@/lib/utils";

const NAV = [
  { to: "/", key: "overview", icon: GaugeIcon },
  { to: "/queries", key: "queries", icon: Activity },
  { to: "/top", key: "top", icon: ListOrdered },
  { to: "/resilience", key: "resilience", icon: ShieldCheck },
  { to: "/upstreams", key: "upstreams", icon: Network },
  { to: "/policy", key: "policy", icon: Shield },
  { to: "/settings", key: "settings", icon: Settings },
] as const;

function Logo() {
  return (
    <div className="flex items-center gap-2.5 px-2">
      <svg viewBox="0 0 32 32" className="size-8" aria-hidden>
        <defs>
          <linearGradient id="lg" x1="0" x2="1" y1="0" y2="1">
            <stop offset="0" stopColor="var(--chart-1)" />
            <stop offset="1" stopColor="var(--chart-3)" />
          </linearGradient>
        </defs>
        <rect width="32" height="32" rx="9" fill="url(#lg)" opacity="0.18" />
        <circle cx="16" cy="16" r="3.2" fill="url(#lg)" />
        <circle cx="16" cy="16" r="8" fill="none" stroke="url(#lg)" strokeWidth="1.6" strokeDasharray="3 3" />
        <circle cx="16" cy="16" r="12.5" fill="none" stroke="url(#lg)" strokeWidth="1.2" opacity="0.6" />
      </svg>
      <div className="leading-tight">
        <div className="text-sm font-semibold tracking-tight">nirvor</div>
        <div className="text-muted-foreground text-[11px]">resilient DNS</div>
      </div>
    </div>
  );
}

function NavItems({ onNavigate }: { onNavigate?: () => void }) {
  const { t } = useTranslation();
  return (
    <nav aria-label="Main" className="flex flex-col gap-1">
      {NAV.map(({ to, key, icon: Icon }) => (
        <NavLink
          key={to}
          to={to}
          end={to === "/"}
          onClick={onNavigate}
          className={({ isActive }) =>
            cn(
              "group relative flex items-center gap-3 rounded-xl px-3 py-2 text-sm font-medium transition-colors",
              isActive ? "text-foreground" : "text-muted-foreground hover:text-foreground hover:bg-accent/40",
            )
          }
        >
          {({ isActive }) => (
            <>
              {isActive && (
                <motion.span
                  layoutId="nav-active"
                  className="bg-accent/70 ring-primary/20 absolute inset-0 rounded-xl ring-1"
                  transition={{ type: "spring", stiffness: 500, damping: 40 }}
                />
              )}
              <Icon className={cn("relative size-4", isActive && "text-primary")} aria-hidden />
              <span className="relative">{t(`nav.${key}`)}</span>
            </>
          )}
        </NavLink>
      ))}
    </nav>
  );
}

export function Layout({ children, onLogout }: { children: ReactNode; onLogout: () => void }) {
  const { t, i18n } = useTranslation();
  const { theme, toggle } = useTheme();
  const { connected, tick } = useLive();
  const [open, setOpen] = useState(false);
  const location = useLocation();
  const nextLang = i18n.language === "bn" ? "en" : "bn";

  return (
    <div className="min-h-dvh">
      <a href="#main" className="bg-primary text-primary-foreground sr-only z-50 rounded-md px-3 py-2 focus:not-sr-only focus:fixed focus:top-3 focus:left-3">
        Skip to content
      </a>
      {/* Desktop sidebar */}
      <aside className="glass fixed inset-y-0 left-0 z-30 hidden w-64 flex-col border-r px-3 py-5 lg:flex">
        <Logo />
        <div className="mt-8 flex-1">
          <NavItems />
        </div>
        <div className="text-muted-foreground px-3 text-[11px]">Never an open resolver · ACL enforced</div>
      </aside>

      {/* Mobile drawer */}
      <AnimatePresence>
        {open && (
          <>
            <motion.div
              className="fixed inset-0 z-40 bg-black/50 lg:hidden"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              onClick={() => setOpen(false)}
            />
            <motion.aside
              className="bg-background fixed inset-y-0 left-0 z-50 w-72 border-r px-3 py-5 lg:hidden"
              initial={{ x: -300 }}
              animate={{ x: 0 }}
              exit={{ x: -300 }}
              transition={{ type: "spring", stiffness: 400, damping: 40 }}
              role="dialog"
              aria-label="Navigation"
            >
              <div className="mb-8 flex items-center justify-between">
                <Logo />
                <Button variant="ghost" size="icon" onClick={() => setOpen(false)} aria-label="Close menu">
                  <X className="size-5" />
                </Button>
              </div>
              <NavItems onNavigate={() => setOpen(false)} />
            </motion.aside>
          </>
        )}
      </AnimatePresence>

      <div className="lg:pl-64">
        <header className="glass sticky top-0 z-20 flex h-14 items-center gap-3 border-b px-4 sm:px-6">
          <Button variant="ghost" size="icon" className="lg:hidden" onClick={() => setOpen(true)} aria-label="Open menu">
            <Menu className="size-5" />
          </Button>
          <div className="flex items-center gap-2">
            {tick && <ModeBadge mode={tick.mode.mode} />}
          </div>
          <div className="ml-auto flex items-center gap-1 sm:gap-2">
            <span className="text-muted-foreground mr-2 hidden items-center gap-2 text-xs sm:flex" aria-live="polite">
              <StatusDot state={connected ? "good" : "warn"} pulse={connected} />
              {connected ? t("common.live") : t("common.offline")}
            </span>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon" onClick={() => i18n.changeLanguage(nextLang)} aria-label={t("common.language")}>
                  <Languages className="size-4" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{nextLang === "bn" ? "বাংলা" : "English"}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon" onClick={toggle} aria-label={t("common.theme")}>
                  {theme === "dark" ? <Sun className="size-4" /> : <Moon className="size-4" />}
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("common.theme")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon" onClick={onLogout} aria-label={t("common.signOut")}>
                  <LogOut className="size-4" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("common.signOut")}</TooltipContent>
            </Tooltip>
          </div>
        </header>
        <main id="main" className="mx-auto max-w-[1400px] px-4 py-6 sm:px-6 lg:py-8">
          <motion.div
            key={location.pathname}
            initial={{ opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.25, ease: "easeOut" }}
          >
            {children}
          </motion.div>
        </main>
      </div>
    </div>
  );
}
