// English is complete; Bangla covers navigation and common labels so far. Missing keys fall back to English.
import i18n from "i18next";
import { initReactI18next } from "react-i18next";

const en = {
  nav: {
    overview: "Overview",
    queries: "Live queries",
    top: "Top domains & clients",
    resilience: "Resilience",
    upstreams: "Upstreams",
    policy: "Policy",
    settings: "Settings",
  },
  mode: {
    normal: "Normal",
    degraded: "Degraded",
    isolation: "Isolation",
    auto: "Automatic",
    normal_desc: "All international upstreams are healthy.",
    degraded_desc: "Some international upstreams are failing. Answers still flow through the healthy ones.",
    isolation_desc: "International links are down. Serving from cache, local upstreams and the local root zone.",
  },
  common: {
    live: "Live",
    offline: "Reconnecting",
    save: "Save",
    cancel: "Cancel",
    add: "Add",
    edit: "Edit",
    remove: "Remove",
    reload: "Reload",
    refresh: "Refresh",
    signOut: "Sign out",
    language: "Language",
    theme: "Toggle theme",
    loading: "Loading…",
    saved: "Saved",
    restartNeeded: "Saved. Restart nirvor for these to take effect: {{list}}",
    healthy: "Healthy",
    down: "Down",
    unknown: "Not measured yet",
    pause: "Pause",
    resume: "Resume",
    search: "Search",
    all: "All",
    since: "since {{when}}",
  },
  login: {
    title: "Sign in to nirvor",
    subtitle: "Resolver dashboard",
    password: "Admin password",
    submit: "Sign in",
    firstRun: "First start? The initial password is in data/initial-admin-password.",
  },
  overview: {
    qps: "Queries / second",
    hitRatio: "Cache hit ratio",
    latency: "Latency",
    cacheEntries: "Cached answers",
    blocked: "Blocked",
    stale: "Served stale",
    traffic: "Traffic",
    trafficSub: "Queries per second, last 5 minutes",
    latencyChart: "Latency percentiles",
    upstreamHealth: "Upstream health",
    totalQueries: "Total queries",
  },
};

const bn = {
  nav: {
    overview: "সারসংক্ষেপ",
    queries: "লাইভ কোয়েরি",
    top: "শীর্ষ ডোমেইন ও ক্লায়েন্ট",
    resilience: "স্থিতিস্থাপকতা",
    upstreams: "আপস্ট্রিম",
    policy: "নীতি",
    settings: "সেটিংস",
  },
  mode: { normal: "স্বাভাবিক", degraded: "আংশিক সমস্যা", isolation: "বিচ্ছিন্ন", auto: "স্বয়ংক্রিয়" },
  common: {
    live: "লাইভ",
    offline: "পুনঃসংযোগ হচ্ছে",
    save: "সংরক্ষণ",
    cancel: "বাতিল",
    signOut: "সাইন আউট",
    language: "ভাষা",
    loading: "লোড হচ্ছে…",
  },
  login: { title: "nirvor-এ সাইন ইন করুন", password: "অ্যাডমিন পাসওয়ার্ড", submit: "সাইন ইন" },
};

i18n.use(initReactI18next).init({
  resources: { en: { translation: en }, bn: { translation: bn } },
  lng: localStorage.getItem("nirvor-lang") || "en",
  fallbackLng: "en",
  interpolation: { escapeValue: false },
});

i18n.on("languageChanged", (lng) => {
  localStorage.setItem("nirvor-lang", lng);
  document.documentElement.lang = lng;
});

export default i18n;
