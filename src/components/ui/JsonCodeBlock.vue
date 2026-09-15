<script setup lang="ts">
/**
 * JSON 代码块：美化缩进 + 轻度语法着色。
 *
 * 用途：展示"MCP Server 客户端配置"这类**本应用自己生成的** JSON——
 * 原来是一整行紧凑 JSON，既难读、窄卡片里还会被右缘裁断。
 *
 * 设计取舍（theme-spec §4.3）：
 * - **不引入 highlight.js / Shiki**：为一段自有 JSON 增加依赖与两套主题适配
 *   不划算，也违背"装饰克制"的设计原则；
 * - 着色只用既有主题令牌（键=accent、字符串=success、字面量=warning、
 *   标点=更弱的 muted），明暗主题自动适配；
 * - 用 Vue 模板渲染 `<span>`，**不用 `v-html`**：即便将来数据来源变化，
 *   也不会留下面向注入的面。
 * - 解析失败时退回纯文本显示，绝不因为格式化而丢内容。
 */
import { computed } from "vue";

/** 词法单元：决定颜色，不改变文本。 */
type TokenKind = "punct" | "key" | "string" | "literal";

interface Token {
  kind: TokenKind;
  text: string;
}

const props = defineProps<{
  /** 待展示的 JSON 文本；解析失败则原样展示。 */
  json: string;
}>();

/** 与 JSON.stringify 一致的缩进宽度。 */
const INDENT = "  ";

/** 把已解析的 JSON 值展开成「缩进 + 词法单元」序列。 */
function tokenize(value: unknown, depth: number, out: Token[]): void {
  const pad = INDENT.repeat(depth);
  const padInner = INDENT.repeat(depth + 1);

  if (Array.isArray(value)) {
    if (value.length === 0) {
      out.push({ kind: "punct", text: "[]" });
      return;
    }
    out.push({ kind: "punct", text: "[\n" });
    value.forEach((item, i) => {
      out.push({ kind: "punct", text: padInner });
      tokenize(item, depth + 1, out);
      out.push({
        kind: "punct",
        text: i === value.length - 1 ? "\n" : ",\n",
      });
    });
    out.push({ kind: "punct", text: `${pad}]` });
    return;
  }

  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>);
    if (entries.length === 0) {
      out.push({ kind: "punct", text: "{}" });
      return;
    }
    out.push({ kind: "punct", text: "{\n" });
    entries.forEach(([key, val], i) => {
      out.push({ kind: "punct", text: padInner });
      out.push({ kind: "key", text: JSON.stringify(key) });
      out.push({ kind: "punct", text: ": " });
      tokenize(val, depth + 1, out);
      out.push({
        kind: "punct",
        text: i === entries.length - 1 ? "\n" : ",\n",
      });
    });
    out.push({ kind: "punct", text: `${pad}}` });
    return;
  }

  // 字符串 / 数字 / 布尔 / null：交给 JSON.stringify 保证转义正确。
  const literal = JSON.stringify(value) ?? "null";
  out.push({
    kind: typeof value === "string" ? "string" : "literal",
    text: literal,
  });
}

const tokens = computed<Token[] | null>(() => {
  try {
    const parsed: unknown = JSON.parse(props.json);
    const out: Token[] = [];
    tokenize(parsed, 0, out);
    return out;
  } catch {
    // 不是合法 JSON：原样展示，不因美化而丢内容。
    return null;
  }
});

const kindClass: Record<TokenKind, string> = {
  punct: "text-text-muted",
  key: "text-accent",
  string: "text-success",
  literal: "text-warning",
};
</script>

<template>
  <pre
    class="rounded-lg bg-surface-code px-3 py-2.5 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap break-all text-text-code"
  ><template v-if="tokens"><span
        v-for="(token, i) in tokens"
        :key="i"
        :class="kindClass[token.kind]"
      >{{ token.text }}</span></template><template v-else>{{ json }}</template></pre>
</template>
