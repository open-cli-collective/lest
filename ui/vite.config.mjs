// Dev server: proxies /api to a running `lest ui` so the UI can be edited
// with hot reload. LEST_UI_URL is the address `lest ui` printed; its token is
// used when LEST_UI_TOKEN is not set.
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const env = process.env;
const printed = env.LEST_UI_URL ? new URL(env.LEST_UI_URL) : null;
const target = printed ? printed.origin : "http://127.0.0.1:4790";
const token = env.LEST_UI_TOKEN || printed?.searchParams.get("token") || "";

export default defineConfig({
  plugins: [react()],
  build: { outDir: "dist", emptyOutDir: true, sourcemap: false },
  server: {
    host: "127.0.0.1",
    proxy: {
      "/api": {
        target,
        changeOrigin: true,
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      },
    },
  },
  test: { include: ["src/**/*.test.ts"], environment: "node" },
});
