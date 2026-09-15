import { fileURLToPath, URL } from "node:url";

import { defineConfig } from "vitest/config";

/**
 * 前端测试配置（AGENTS.md §5.9）。
 *
 * 范围刻意收得很窄：**只测 `src/lib/` 的纯逻辑**（IPC 信封解包、错误码映射、
 * 数值格式化）。这些函数无 DOM 依赖、有真实契约，投入最小、收益最直接。
 *
 * 之所以独立于 `vite.config.ts`：那份配置带着 Tauri 专用的 dev server 端口与
 * watch 规则，对测试没有意义；这里只声明测试真正需要的东西（别名 + 环境）。
 *
 * 组件与 Store 测试**暂不引入**——那需要 `@vue/test-utils` 与 DOM 环境，
 * 等有具体需求再加，避免"框架在就顺手写测试"（§5.1）。
 */
export default defineConfig({
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  test: {
    // 只用 .test.ts；组件测试将来若引入，届时再放开 .vue。
    include: ["src/**/*.test.ts"],
    // node 环境足够：被测的都是纯函数，不需要 jsdom/happy-dom。
    environment: "node",
  },
});
