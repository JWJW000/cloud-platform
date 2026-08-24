/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// admin-web 前端构建配置。
//
// 本地联调：dev server 把 /api 代理到 Master（127.0.0.1:8080）。
// 注意 changeOrigin 必须保持 false：Master 的 CSRF 中间件会把
// site_base（http://localhost:3000）当作隐式合法 Origin，改写 Host 反而
// 会让 Origin/Host 不一致并误判跨站。见 master 侧 csrf_origin_middleware。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 3000,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8080",
        changeOrigin: false,
      },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    css: true,
  },
});
