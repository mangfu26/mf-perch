<script setup lang="ts">
/**
 * 通用按钮（D24 主题令牌驱动）。
 *
 * 变体：primary（主操作）/ default（次要）/ ghost（弱化）/ danger（危险操作）。
 */
import { computed } from "vue";
import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    variant?: "primary" | "default" | "ghost" | "danger";
    size?: "sm" | "md";
    disabled?: boolean;
    type?: "button" | "submit";
  }>(),
  {
    variant: "default",
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
  ),
);
</script>

<template>
  <button :type="type" :disabled="disabled" :class="classes">
    <slot />
  </button>
</template>
