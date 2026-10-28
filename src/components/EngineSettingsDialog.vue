<script setup lang="ts">
import { ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { Eye, EyeOff, ExternalLink, Loader2, Save, Sparkles } from "lucide-vue-next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { store, pushLog } from "@/lib/store";

const VOLC_ACTIVATE_URL =
  "https://console.volcengine.com/speech/new/setting/activate?ResourceID=volc.seedasr.sauc.duration&_vtm_=a106466.b106468.0_0.0_0.0.215_7701485951495521827&projectName=default";

const draftEngine = ref<"doubao" | "volc">("doubao");
const draftKey = ref("");
const showKey = ref(false);
const saving = ref(false);

watch(
  () => store.engineSettingsOpen,
  (open) => {
    if (open) {
      draftEngine.value = store.asrEngine;
      draftKey.value = store.apiKey;
      showKey.value = false;
      invoke<{ engine: string; api_key: string; resource_id: string }>("get_asr_config")
        .then((c) => {
          store.resourceId = c.resource_id;
        })
        .catch(() => {});
    }
  },
);

function openActivatePage() {
  invoke("open_url", { url: VOLC_ACTIVATE_URL }).catch((e) =>
    pushLog({ line: `打开开通页面失败: ${e}`, kind: "err" }),
  );
}

async function save() {
  if (draftEngine.value === "volc" && draftKey.value.trim() === "") {
    pushLog({ line: "启用火山引擎前请先填写 API Key", kind: "err" });
    return;
  }
  saving.value = true;
  try {
    await invoke("set_asr_config", {
      engine: draftEngine.value,
      apiKey: draftKey.value.trim(),
    });
    store.asrEngine = draftEngine.value;
    store.apiKey = draftKey.value.trim();
    store.engineSettingsOpen = false;
  } catch (e) {
    pushLog({ line: `保存引擎配置失败: ${e}`, kind: "err" });
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <Dialog v-model:open="store.engineSettingsOpen">
    <DialogContent class="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>识别引擎设置</DialogTitle>
        <DialogDescription>
          免费引擎依赖豆包扫码登录；火山引擎为官方付费服务，契约稳定、更准更快。
        </DialogDescription>
      </DialogHeader>

      <!-- 引擎选择 -->
      <div class="grid gap-2.5">
        <button
          class="flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 text-left transition-colors"
          :class="
            draftEngine === 'doubao'
              ? 'border-emerald-400/50 bg-emerald-400/[0.07]'
              : 'border-white/8 bg-white/[0.02] hover:border-white/15'
          "
          @click="draftEngine = 'doubao'"
        >
          <span
            class="mt-0.5 inline-flex size-4 shrink-0 items-center justify-center rounded-full border-2"
            :class="draftEngine === 'doubao' ? 'border-emerald-400' : 'border-white/25'"
          >
            <span v-if="draftEngine === 'doubao'" class="size-2 rounded-full bg-emerald-400" />
          </span>
          <span>
            <span class="block text-sm font-semibold">豆包云端（免费）</span>
            <span class="mt-1 block text-xs leading-relaxed text-muted-foreground">
              通过扫码登录使用豆包网页版识别通道，零成本开箱即用。
            </span>
          </span>
        </button>

        <button
          class="flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 text-left transition-colors"
          :class="
            draftEngine === 'volc'
              ? 'border-emerald-400/50 bg-emerald-400/[0.07]'
              : 'border-white/8 bg-white/[0.02] hover:border-white/15'
          "
          @click="draftEngine = 'volc'"
        >
          <span
            class="mt-0.5 inline-flex size-4 shrink-0 items-center justify-center rounded-full border-2"
            :class="draftEngine === 'volc' ? 'border-emerald-400' : 'border-white/25'"
          >
            <span v-if="draftEngine === 'volc'" class="size-2 rounded-full bg-emerald-400" />
          </span>
          <span class="min-w-0">
            <span class="flex flex-wrap items-center gap-2 text-sm font-semibold">
              火山引擎·流式语音识别 2.0
              <span
                class="rounded border border-emerald-400/30 bg-emerald-400/10 px-1.5 py-0.5 text-[10px] font-medium text-emerald-300"
              >
                推荐
              </span>
            </span>
            <span class="mt-1 block text-xs leading-relaxed text-muted-foreground">
              官方付费 API（小时版，开通即送 20 小时），二遍识别+标点+数字规整，契约稳定不依赖 Cookie。
            </span>
          </span>
        </button>
      </div>

      <!-- 火山引擎配置区 -->
      <div v-if="draftEngine === 'volc'" class="grid gap-3">
        <div class="flex items-center justify-between rounded-lg border border-white/8 bg-white/[0.02] px-3 py-2.5">
          <div class="min-w-0">
            <div class="text-xs font-medium text-foreground/80">服务实例（Resource ID）</div>
            <div class="mt-0.5 truncate font-mono text-[11px] text-muted-foreground">
              {{ store.resourceId || "volc.seedasr.sauc.duration" }} · 小时版
            </div>
          </div>
          <Button size="sm" variant="outline" class="shrink-0" @click="openActivatePage">
            <ExternalLink />
            开通服务（送20小时）
          </Button>
        </div>

        <div class="grid gap-1.5">
          <label class="text-xs font-medium text-foreground/80" for="volc-api-key">
            API Key（控制台 &gt; API Key 管理）
          </label>
          <div class="relative">
            <input
              id="volc-api-key"
              v-model="draftKey"
              :type="showKey ? 'text' : 'password'"
              placeholder="粘贴火山引擎 API Key"
              autocomplete="off"
              spellcheck="false"
              class="h-9 w-full rounded-md border border-input bg-background/60 pr-9 pl-3 font-mono text-sm outline-none placeholder:text-muted-foreground/50 focus-visible:ring-[3px] focus-visible:ring-ring/40"
            />
            <button
              class="absolute top-1/2 right-2 -translate-y-1/ cursor-pointer text-muted-foreground hover:text-foreground"
              type="button"
              @click="showKey = !showKey"
            >
              <Eye v-if="!showKey" class="size-4" />
              <EyeOff v-else class="size-4" />
            </button>
          </div>
          <p class="flex items-start gap-1.5 text-[11px] leading-relaxed text-muted-foreground/70">
            <Sparkles class="mt-0.5 size-3 shrink-0" />
            Key 仅保存在本机 exe 旁的 config.json，不会上传给任何第三方。
          </p>
        </div>
      </div>

      <div class="flex items-center justify-end gap-2">
        <Button variant="ghost" @click="store.engineSettingsOpen = false">取消</Button>
        <Button
          class="bg-emerald-400/90 text-emerald-950 hover:bg-emerald-300"
          :disabled="saving"
          @click="save"
        >
          <Loader2 v-if="saving" class="animate-spin" />
          <Save v-else />
          保存
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
