<script setup lang="ts">
/**
 * 表单输入控件（input / textarea / select 三合一）。
 *
 * 统一外观以便所有表单风格一致；通过 `as` 切换元素类型。
 */
import { computed } from "vue";
import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    modelValue: string | number | null | undefined;
    as?: "input" | "textarea" | "select";
    type?: string;
    placeholder?: string;
    disabled?: boolean;
    rows?: number;
    mono?: boolean;
  }>(),
  { as: "input", type: "text", rows: 4, mono: false },
);

const emit = defineEmits<{
  (e: "update:modelValue", v: string | number): void;
}>();

const classes = computed(() =>
  cn(
    "w-full rounded-[9px] border border-border-base bg-surface px-3 py-2 text-[13px]",
    "text-text-base outline-none transition-colors",
    "placeholder:text-text-muted focus:border-accent",
    "disabled:cursor-not-allowed disabled:opacity-60",
    // 下拉弹出层：根元素已按主题设 color-scheme，这里再给 <option> 兜底配色，
    // 避免个别 WebView 版本仍用系统默认白底黑字（Q：暗色主题下弹层不适配）。
    props.as === "select" &&
      "[&>option]:bg-surface [&>option]:text-text-base",
    props.mono && "font-mono text-[12px]",
  ),
);

function onInput(e: Event) {
  const target = e.target as HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
  emit("update:modelValue", target.value);
}
</script>

<template>
  <textarea
    v-if="as === 'textarea'"
    :value="modelValue ?? ''"
    :placeholder="placeholder"
    :disabled="disabled"
    :rows="rows"
    :class="cn(classes, 'resize-y leading-relaxed')"
    @input="onInput"
  />
  <select
    v-else-if="as === 'select'"
    :value="modelValue ?? ''"
    :disabled="disabled"
    :class="classes"
    @change="onInput"
  >
    <slot />
  </select>
  <input
    v-else
    :value="modelValue ?? ''"
    :type="type"
    :placeholder="placeholder"
    :disabled="disabled"
    :class="classes"
    @input="onInput"
  />
</template>
