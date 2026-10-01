import { defineConfig } from "vite";
export default defineConfig({
  base: "./",
  build: { rolldownOptions: { input: ["index.html", "redirect.html"] } },
  test: { include: ["tests/**/*.test.js"] },
});
