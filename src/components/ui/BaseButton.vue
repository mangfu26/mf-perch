<script setup lang="ts">
/**
 * 通用按钮（D24 主题令牌驱动）。
 *
 * - `variant`：视觉重量（primary 主操作 / default 次要 / ghost 弱化 / danger 危险块）；
 * - `tone`：**语义色**，用于弱化按钮（如卡片上的图标按钮）在悬停时表达意图：
 *   编辑 → `accent`，删除 → `danger`。
 *
 * 为什么要有 `tone` 而不是让调用方传 class：Vue 会把外部 class 追加到根元素上，
 * 但它不经过 `cn`，tailwind-merge 无法消解与 variant 内 `hover:text-*` 的冲突，
 * 结果取决于样式表顺序、不可靠。放进 `cn` 里则**确定由 tone 覆盖 variant**。
 * 常驻红色不作为默认：一屏多个删除图标会持续抢注意力，主流做法是
 * 常态中性 + 悬停/确认时转危险色（theme-spec §4.4）。
 */
import { computed } from "vue";
import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    variant?: "primary" | "default" | "ghost" | "danger";
    tone?: "default" | "accent" | "danger";
    size?: "sm" | "md";
    disabled?: boolean;
    type?: "button" | "submit";
  }>(),
  {
    variant: "default",
    tone: "default",
    size: "md",
    disabled: false,
    type: "button",
  },
);

const classes = computed(() =>
  cn(
    "inline-flex items-center justify-center gap-1.5 rounded-[9px] font-medium transition-all",
    "disabled:cursor-not-allowed disabled:opacity-50",
    props.size === "sm" ? "px-2.5 py-1.5 text-[12px]" : "px-3.5 py-2 text-[12.5px]",
    props.variant === "primary" &&
      "border border-transparent bg-[var(--btn-primary-bg)] font-semibold text-white [box-shadow:var(--btn-primary-shadow)] hover:brightness-110",
    props.variant === "default" &&
      "border border-border-base bg-surface text-text-base hover:bg-surface-hover",
    props.variant === "ghost" && "text-text-muted hover:bg-surface-hover hover:text-text-base",
    props.variant === "danger" &&
      "border border-danger/30 bg-danger-soft text-danger hover:brightness-110",
    // 语义色放在最后：让 tailwind-merge 以它为准覆盖上面的 hover 样式（同上说明）。
    props.tone === "accent" && "hover:bg-accent-soft hover:text-accent",
    props.tone === "danger" && "hover:bg-danger-soft hover:text-danger",
  ),
);
</script>

<template>
  <button :type="type" :disabled="disabled" :class="classes">
    <slot />
  </button>
</template>
