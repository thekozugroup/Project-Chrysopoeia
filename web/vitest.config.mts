import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

/** Unit and component tests for the client logic (`pnpm test`). */
export default defineConfig({
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  esbuild: { jsx: "automatic" },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    restoreMocks: true,
  },
});
