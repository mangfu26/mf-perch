<script setup lang="ts">
/**
 * 状态标签（D24）。
 *
 * 无障碍要求：状态不能只靠颜色区分，因此颜色 + 文字双通道。
 */
import { computed } from "vue";
import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    tone?: "neutral" | "success" | "info" | "accent" | "warning" | "danger";
    mono?: boolean;
  }>(),
  {
    tone: "neutral",
    mono: true,
  },
);

const classes = computed(() =>
  cn(
    "inline-flex items-center gap-1 rounded-full px-2 py-[2.5px] text-[10.5px]",
    props.mono && "font-mono",
    props.tone === "neutral" && "bg-surface-hover text-text-muted",
    props.tone === "success" && "bg-success-soft text-success",
    props.tone === "info" && "bg-info-soft text-info",
    props.tone === "accent" && "bg-accent-soft text-accent",
    props.tone === "warning" && "bg-warning-soft text-warning",
    props.tone === "danger" && "bg-danger-soft text-danger",
  ),
);
</script>

<template>
  <span :class="classes"><slot /></span>
</template>
