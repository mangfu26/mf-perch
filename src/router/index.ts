import { createRouter, createWebHashHistory } from "vue-router";

/**
 * 路由（D26：一级导航 4 项，关于在设置内）。
 *
 * 使用 hash 模式：Tauri 打包后以 file:// 或自定义协议加载页面，
 * hash 模式无需服务端路由配置，最稳妥。
 */
const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    {
      path: "/",
      redirect: "/hosts",
    },
    {
      path: "/hosts",
      name: "hosts",
      component: () => import("@/views/HostsView.vue"),
      meta: { titleKey: "nav.hosts" },
    },
    {
      path: "/credentials",
      name: "credentials",
      component: () => import("@/views/CredentialsView.vue"),
      meta: { titleKey: "nav.credentials" },
    },
    {
      path: "/terminals",
      name: "terminals",
      component: () => import("@/views/TerminalsView.vue"),
      meta: { titleKey: "nav.terminals" },
    },
    {
      path: "/terminals/:id",
      name: "terminal-detail",
      component: () => import("@/views/TerminalDetailView.vue"),
      meta: { titleKey: "terminal.detail" },
    },
    {
      path: "/settings",
      name: "settings",
      component: () => import("@/views/SettingsView.vue"),
      meta: { titleKey: "nav.settings" },
    },
    {
      path: "/:pathMatch(.*)*",
      redirect: "/hosts",
    },
  ],
});

export default router;
