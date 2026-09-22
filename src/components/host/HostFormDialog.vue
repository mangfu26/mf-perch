<script setup lang="ts">
/**
 * 主机表单（新增 / 编辑）。
 *
 * 设计要点：
 * - 默认 sudo 策略为 `deny`（最安全，Q33），放宽需用户主动选择
 * - 环境加载默认 `login`（D4：避免 Agent 找不到用户自定义 PATH 的命令）
 * - 绑定凭据为可选项，未绑定时该主机对 Agent 标记为 not ready
 */
import { ref, watch, computed } from "vue";
import { useI18n } from "vue-i18n";
import BaseModal from "@/components/ui/BaseModal.vue";
import BaseButton from "@/components/ui/BaseButton.vue";
import BaseInput from "@/components/ui/BaseInput.vue";
import FormField from "@/components/ui/FormField.vue";
import type { HostInput, HostSummary, ShellEnvMode, SudoPasswordSource, SudoPolicy } from "@/lib/api";
import { parsePort } from "@/lib/host-form";
import { useCredentialsStore } from "@/stores/credentials";

const { t } = useI18n();

const props = defineProps<{
  open: boolean;
  /** 传入表示编辑，否则为新增。 */
  host?: HostSummary | null;
  saving?: boolean;
}>();

const emit = defineEmits<{
  (e: "submit", input: HostInput): void;
  (e: "close"): void;
}>();

const credentials = useCredentialsStore();

const name = ref("");
const address = ref("");
// 数字输入框经 v-model 回传的是 DOM 字符串（`"2222"`），故类型含 string，
// 提交前统一交给 parsePort 归一。
const port = ref<number | string>(22);
const credentialId = ref<string>("");
const proxyJumpHostId = ref<string | null>(null);
const sudoPolicy = ref<SudoPolicy>("deny");
const sudoPasswordSource = ref<SudoPasswordSource>("reuse_login");
const sudoPassword = ref("");
const shellEnvMode = ref<ShellEnvMode>("login");
const initScript = ref("");
const error = ref<string | null>(null);

/** 打开时重置表单（编辑则回填）。 */
watch(
  () => props.open,
  (open) => {
    if (!open) return;
    error.value = null;
    sudoPassword.value = "";

    const h = props.host;
    name.value = h?.name ?? "";
    address.value = h?.address ?? "";
    port.value = h?.port ?? 22;
    // 编辑时必须回填**全部可编辑字段**（B5）：提交会整体覆盖主机配置，
    // 少回填一个字段，用户"只改个名字"就会把该字段静默清空。
    // sudo 密码本身不回显（后端不返回），留空表示保留原值。
    credentialId.value = h?.credential_id ?? "";
    proxyJumpHostId.value = h?.proxy_jump_host_id ?? null;
    sudoPolicy.value = h?.sudo_policy ?? "deny";
    sudoPasswordSource.value = h?.sudo_password_source ?? "reuse_login";
    shellEnvMode.value = h?.shell_env_mode ?? "login";
    initScript.value = h?.init_script ?? "";
  },
  { immediate: true },
);

watch(
  () => props.open,
  async (open) => {
    if (open && credentials.credentials.length === 0) {
      await credentials.refresh();
    }
  },
);

const isEdit = computed(() => !!props.host);
const title = computed(() => (isEdit.value ? t("host.edit") : t("host.add")));
/**
 * 是否存在"提权"这一步（D60）。
 *
 * `deny` 拒绝提权、`not_needed` 登录身份本身已是特权用户，两档都**没有口令可配**；
 * 只有 `ask` / `auto` 才需要向用户要 sudo 密码。
 */
const usesElevation = computed(
  () => sudoPolicy.value === "ask" || sudoPolicy.value === "auto",
);
const requiresSudoPassword = computed(
  () => usesElevation.value && sudoPasswordSource.value === "own",
);

function submit() {
  error.value = null;

  if (!address.value.trim()) {
    error.value = "请填写主机地址";
    return;
  }
  const p = parsePort(port.value);
  if (p === null) {
    error.value = "端口必须是 1–65535 之间的整数";
    return;
  }

  emit("submit", {
    id: props.host?.id ?? null,
    name: name.value.trim() || null,
    address: address.value.trim(),
    port: p,
    credential_id: credentialId.value || null,
    // 保留原有的跳板机配置（当前界面无控件，但不得因编辑而丢失）。
    proxy_jump_host_id: proxyJumpHostId.value,
    sudo_policy: sudoPolicy.value,
    sudo_password_source: sudoPasswordSource.value,
    // 留空表示不修改已有密码（编辑场景）。
    sudo_password: sudoPassword.value || null,
    shell_env_mode: shellEnvMode.value,
    init_script: initScript.value.trim() || null,
  });
}
</script>

<template>
  <BaseModal :open="open" :title="title" @close="emit('close')">
    <div class="grid gap-4">
      <FormField :label="t('host.name')" :hint="t('common.optional')">
        <BaseInput v-model="name" :placeholder="t('host.namePlaceholder')" />
      </FormField>

      <div class="grid grid-cols-[1fr_120px] gap-3">
        <FormField :label="t('host.address')" required>
          <BaseInput
            v-model="address"
            :placeholder="t('host.addressPlaceholder')"
            mono
          />
        </FormField>
        <FormField :label="t('host.port')" required>
          <BaseInput v-model="port" type="number" mono />
        </FormField>
      </div>

      <FormField :label="t('host.credential')" :hint="t('host.credentialHint')">
        <BaseInput v-model="credentialId" as="select">
          <option value="">{{ t("host.credentialNone") }}</option>
          <option v-for="c in credentials.options()" :key="c.value" :value="c.value">
            {{ c.label }}
          </option>
        </BaseInput>
      </FormField>

      <!-- sudo 策略：默认 deny，放宽需显式选择（Q33 / P2） -->
      <FormField :label="t('host.sudoPolicy')">
        <div class="grid gap-2">
          <label
            v-for="opt in [
              { v: 'deny', label: t('host.sudoPolicyDeny'), desc: t('host.sudoPolicyDenyDesc'), tone: 'safe' },
              { v: 'ask', label: t('host.sudoPolicyAsk'), desc: t('host.sudoPolicyAskDesc'), tone: 'warn' },
              { v: 'auto', label: t('host.sudoPolicyAuto'), desc: t('host.sudoPolicyAutoDesc'), tone: 'warn' },
              { v: 'not_needed', label: t('host.sudoPolicyNotNeeded'), desc: t('host.sudoPolicyNotNeededDesc'), tone: 'warn' },
            ]"
            :key="opt.v"
            class="flex cursor-pointer items-start gap-2.5 rounded-[9px] border px-3 py-2.5 transition-colors"
            :class="
              sudoPolicy === opt.v
                ? 'border-accent bg-accent-soft'
                : 'border-border-base hover:bg-surface-hover'
            "
          >
            <input
              v-model="sudoPolicy"
              type="radio"
              :value="opt.v"
              class="mt-0.5 accent-[var(--accent)]"
            />
            <span>
              <span class="block text-[12.5px] font-medium">{{ opt.label }}</span>
              <span class="mt-0.5 block text-[11.5px] leading-relaxed text-text-muted">
                {{ opt.desc }}
              </span>
            </span>
          </label>
        </div>
      </FormField>

      <!-- sudo 密码：只有确实存在"提权"这一步时才出现（D60） -->
      <template v-if="usesElevation">
        <FormField :label="t('host.sudoPasswordSource')">
          <BaseInput v-model="sudoPasswordSource" as="select">
            <option value="reuse_login">{{ t("host.sudoPasswordReuse") }}</option>
            <option value="own">{{ t("host.sudoPasswordOwn") }}</option>
          </BaseInput>
        </FormField>

        <FormField
          v-if="requiresSudoPassword"
          :label="t('host.sudoPassword')"
          :hint="isEdit ? t('credential.secretUnchanged') : undefined"
        >
          <BaseInput v-model="sudoPassword" type="password" />
        </FormField>
      </template>

      <FormField :label="t('host.shellEnvMode')">
        <BaseInput v-model="shellEnvMode" as="select">
          <option value="login">{{ t("host.shellEnvLogin") }}</option>
          <option value="clean">{{ t("host.shellEnvClean") }}</option>
        </BaseInput>
      </FormField>

      <FormField :label="t('host.initScript')" :hint="t('host.initScriptHint')">
        <BaseInput
          v-model="initScript"
          as="textarea"
          :rows="3"
          mono
        />
      </FormField>

      <p
        v-if="error"
        class="rounded-lg border border-danger/30 bg-danger-soft px-3 py-2 text-[12px] text-danger"
      >
        {{ error }}
      </p>
    </div>

    <template #footer>
      <BaseButton variant="default" :disabled="saving" @click="emit('close')">
        {{ t("common.cancel") }}
      </BaseButton>
      <BaseButton variant="primary" :disabled="saving" @click="submit">
        {{ saving ? t("common.loading") : t("common.save") }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
