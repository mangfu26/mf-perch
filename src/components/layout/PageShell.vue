<script setup lang="ts">
/**
 * 页面外壳：标题 + 操作区 + 内容区。
 * 统一各页面布局节奏，避免每个页面各自定义标题样式。
 *
 * `width` 决定内容列宽（见 theme-spec.md §4.2）：
 * - `full`（默认）：铺满可用宽度——列表 / 卡片网格 / 左右分栏这类页面需要它；
 * - `narrow`：标题区与内容区**共享一个居中限宽列**。
 *   设置页这类"标签 ↔ 值"表单在宽屏下若铺满，视线要来回横跳；
 *   限宽之后必须**居中**，否则整列会贴着左边、右侧空一大片，看起来没对齐。
 *   注意标题区一起进该列：只居中内容会让标题与卡片分成两条轴线。
 */
import type { Component } from "vue";

import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    title: string;
    subtitle?: string;
    icon?: Component;
    width?: "full" | "narrow";
  }>(),
  { width: "full" },
);

/** 居中限宽列：与设置页原有的 `max-w-3xl` 保持一致。 */
const columnClass = cn(
  "flex min-h-0 flex-1 flex-col",
  props.width === "narrow" && "mx-auto w-full max-w-3xl",
);
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
