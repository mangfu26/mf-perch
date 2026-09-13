<script setup lang="ts">
/**
 * 页面外壳：标题 + 操作区 + 内容区。
 * 统一各页面布局节奏，避免每个页面各自定义标题样式。
 *
 * `width` 决定内容列宽（见 theme-spec.md §4.2）：
 * - `wide`（默认）：居中限宽 1280px。内容不再随窗口无限拉宽——否则卡片会被撑得
 *   很空、卡片右上角的操作按钮要横跨整屏才点得到；
 * - `narrow`：居中限宽 768px，用于设置页这类"标签 ↔ 值"的窄行表单；
 * - `full`：铺满可用宽度（保留给将来确实需要横向空间的分栏视图）。
 *
 * 标题区与内容区**共享同一列**：只居中内容会让标题与卡片分处两条轴线。
 */
import type { Component } from "vue";

import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    title: string;
    subtitle?: string;
    icon?: Component;
    width?: "narrow" | "wide" | "full";
  }>(),
  { width: "wide" },
);

/** 内容列宽档位。 */
const columnClass = cn("flex min-h-0 flex-1 flex-col", {
  "mx-auto w-full max-w-3xl": props.width === "narrow",
  "mx-auto w-full max-w-7xl": props.width === "wide",
});
</script>

<template>
  <div class="flex h-full flex-col overflow-hidden px-6 py-5">
    <div :class="columnClass">
      <header class="mb-4 flex shrink-0 items-start justify-between gap-4">
        <div class="flex items-start gap-3">
          <div
            v-if="icon"
            class="mt-0.5 grid h-9 w-9 shrink-0 place-items-center rounded-[10px] bg-accent-soft text-accent"
          >
            <component :is="icon" class="h-[18px] w-[18px]" />
          </div>
          <div>
            <h1 class="text-[19px] font-semibold tracking-tight">{{ title }}</h1>
            <p v-if="subtitle" class="mt-1 text-[12.5px] text-text-muted">{{ subtitle }}</p>
          </div>
        </div>
        <div class="flex shrink-0 items-center gap-2">
          <slot name="actions" />
        </div>
      </header>
      <div class="min-h-0 flex-1 overflow-auto">
        <slot />
      </div>
    </div>
  </div>
</template>
