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
const port = ref<number>(22);
const credentialId = ref<string>("");
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
    credentialId.value = "";
    sudoPolicy.value = h?.sudo_policy ?? "deny";
    sudoPasswordSource.value = "reuse_login";
    shellEnvMode.value = "login";
    initScript.value = "";
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
const requiresSudoPassword = computed(
  () => sudoPolicy.value !== "deny" && sudoPasswordSource.value === "own",
);

function submit() {
  error.value = null;

  if (!address.value.trim()) {
    error.value = "请填写主机地址";
    return;
  }
  if (!Number.isInteger(port.value) || port.value < 1 || port.value > 65535) {
    error.value = "端口必须是 1–65535 之间的整数";
    return;
  }

  emit("submit", {
    id: props.host?.id ?? null,
    name: name.value.trim() || null,
    address: address.value.trim(),
    port: port.value,
    credential_id: credentialId.value || null,
    proxy_jump_host_id: null,
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

      <!-- sudo 密码：仅在需要注入且选择单独配置时出现 -->
      <template v-if="sudoPolicy !== 'deny'">
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
