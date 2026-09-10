<script setup lang="ts">
/** 开关（用于布尔设置项）。 */
const props = defineProps<{
  modelValue: boolean;
  disabled?: boolean;
}>();

const emit = defineEmits<{ (e: "update:modelValue", v: boolean): void }>();

function toggle() {
  if (props.disabled) return;
  emit("update:modelValue", !props.modelValue);
}
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="modelValue"
    :disabled="disabled"
    class="relative h-[22px] w-[38px] shrink-0 rounded-full border transition-colors disabled:opacity-50"
    :class="
      modelValue
        ? 'border-transparent bg-accent'
        : 'border-border-base bg-surface-hover'
    "
    @click="toggle"
  >
    <span
      class="absolute top-[2px] h-[16px] w-[16px] rounded-full bg-white shadow-sm transition-all"
      :class="modelValue ? 'left-[19px]' : 'left-[2px]'"
    />
  </button>
</template>
