<script setup lang="ts">
/**
 * 表单输入控件（input / textarea / select 三合一）。
 *
 * 统一外观以便所有表单风格一致；通过 `as` 切换元素类型。
 */
import { computed, useAttrs } from "vue";
import { cn } from "@/lib/utils";

// 关闭 attribute 自动透传：改为在各根元素上手动用 cn() 合并外部 class。
// 否则外部传入的宽度类（如 w-48）会被 Vue 原样追加到内部 classes 之后，
// 与内置的 w-full 特异性相同、胜负取决于样式表顺序——导致下拉撑爆整行。
defineOptions({ inheritAttrs: false });

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

const attrs = useAttrs();

const emit = defineEmits<{
  (e: "update:modelValue", v: string | number): void;
}>();

// 内部基础样式 + 外部 class 经 tailwind-merge 合并：外部冲突项（宽度、
// padding 等）可靠地覆盖内部默认值。
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
    // 外部 class 放最后：让调用方覆盖宽度/内边距等冲突项。
    attrs.class as string | undefined,
  ),
);

// 除 class 外的其它透传属性（id、data-*、aria-* 等）原样落到根元素。
const passthrough = computed(() => {
  const { class: _cls, ...rest } = attrs;
  return rest;
});


function onInput(e: Event) {
  const target = e.target as HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
  emit("update:modelValue", target.value);
}
</script>

<template>
  <textarea
    v-if="as === 'textarea'"
    v-bind="passthrough"
    :value="modelValue ?? ''"
    :placeholder="placeholder"
    :disabled="disabled"
    :rows="rows"
    :class="cn(classes, 'resize-y leading-relaxed')"
    @input="onInput"
  />
  <select
    v-else-if="as === 'select'"
    v-bind="passthrough"
    :value="modelValue ?? ''"
    :disabled="disabled"
    :class="classes"
    @change="onInput"
  >
    <slot />
  </select>
  <input
    v-else
    v-bind="passthrough"
    :value="modelValue ?? ''"
    :type="type"
    :placeholder="placeholder"
    :disabled="disabled"
    :class="classes"
    @input="onInput"
  />
</template>
