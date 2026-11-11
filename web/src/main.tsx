import "./index.css";
import "./lib/i18n";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Route, Routes } from "react-router";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Layout } from "@/components/Layout";
import { api, setUnauthorizedHandler } from "@/lib/api";
import { LiveProvider } from "@/lib/live";
import { ThemeProvider } from "@/lib/theme";
import { Login } from "@/pages/Login";
import { Overview } from "@/pages/Overview";
import { Policy } from "@/pages/Policy";
import { Queries } from "@/pages/Queries";
import { Resilience } from "@/pages/Resilience";
import { Settings } from "@/pages/Settings";
import { Top } from "@/pages/Top";
import { Upstreams } from "@/pages/Upstreams";

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: 1, refetchOnWindowFocus: false, staleTime: 2000 } },
});

function App() {
  const [auth, setAuth] = useState<"checking" | "in" | "out">("checking");

  useEffect(() => {
    setUnauthorizedHandler(() => setAuth("out"));
    api.session().then((ok) => setAuth(ok ? "in" : "out"));
  }, []);

  if (auth === "checking") return null;
  if (auth === "out") return <Login onDone={() => setAuth("in")} />;

  const logout = async () => {
    await api.logout().catch(() => {});
    queryClient.clear();
    setAuth("out");
  };

  return (
    <LiveProvider>
      <Layout onLogout={logout}>
        <Routes>
          <Route path="/" element={<Overview />} />
          <Route path="/queries" element={<Queries />} />
          <Route path="/top" element={<Top />} />
          <Route path="/resilience" element={<Resilience />} />
          <Route path="/upstreams" element={<Upstreams />} />
          <Route path="/policy" element={<Policy />} />
          <Route path="/settings" element={<Settings />} />
          <Route path="*" element={<Overview />} />
        </Routes>
      </Layout>
    </LiveProvider>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <TooltipProvider delayDuration={300}>
          <BrowserRouter>
            <App />
          </BrowserRouter>
          <Toaster position="bottom-right" richColors closeButton />
        </TooltipProvider>
      </QueryClientProvider>
    </ThemeProvider>
  </StrictMode>,
);
