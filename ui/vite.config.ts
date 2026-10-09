import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri から使う前提の設定 (https://v2.tauri.app/start/frontend/vite/)
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: { target: "safari15", outDir: "dist" },
});
