<script setup lang="ts">
import { onMounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import {
  Activity,
  AudioLines,
  BookOpen,
  Cloud,
  FlaskConical,
  FolderOpen,
  Globe,
  KeyRound,
  Loader2,
  Mic,
  Power,
  QrCode,
  Settings2,
  Usb,
} from "lucide-vue-next";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { store, initStore, pushLog } from "@/lib/store";
import StatusCard from "@/components/StatusCard.vue";
import BatteryGauge from "@/components/BatteryGauge.vue";
import LogPanel from "@/components/LogPanel.vue";
import HelpDialog from "@/components/HelpDialog.vue";
import EngineSettingsDialog from "@/components/EngineSettingsDialog.vue";

onMounted(() => {
  initStore().catch((e) => {
    pushLog({ line: `初始化失败: ${e}`, kind: "err" });
  });
});

async function startLogin() {
  store.loginBusy = true;
  try {
    await invoke("start_login");
  } catch (e) {
    store.loginBusy = false;
    pushLog({ line: `发起登录失败: ${e}`, kind: "err" });
  }
}

async function startBrowserLogin() {
  store.loginBusy = true;
  try {
    await invoke("start_browser_login");
  } catch (e) {
    store.loginBusy = false;
    pushLog({ line: `发起浏览器登录失败: ${e}`, kind: "err" });
  }
}

async function runLinkTest() {
  store.testBusy = true;
  try {
    await invoke("run_link_test");
  } catch (e) {
    store.testBusy = false;
    pushLog({ line: `链路测试失败: ${e}`, kind: "err" });
  }
}

function openRecordingsDir() {
  invoke("open_recordings_dir").catch((e) => pushLog({ line: `${e}`, kind: "err" }));
}

async function setAutostart(on: boolean) {
  try {
    const ok = await invoke<boolean>("set_autostart", { on });
    store.autostart = ok ? on : store.autostart;
    pushLog({
      line: ok
        ? on
          ? "✓ 已开启开机自动启动（写入注册表 HKCU\\...\\Run）"
          : "已关闭开机自动启动"
        : "✗ 开机自启写入注册表失败（详见日志）",
      kind: ok ? (on ? "ok" : undefined) : "err",
    });
  } catch (e) {
    pushLog({ line: `设置开机自启失败: ${e}`, kind: "err" });
  }
}

function exitApp() {
  invoke("exit_app").catch(() => {});
}
</script>

<template>
  <div class="flex h-screen flex-col bg-background">
    <!-- 顶部横幅 -->
    <header
      class="relative overflow-hidden border-b border-white/5 bg-gradient-to-r from-[oklch(0.22_0.03_250)] via-[oklch(0.2_0.025_220)] to-[oklch(0.19_0.03_165)] px-6 pt-5 pb-4"
    >
      <div
        class="pointer-events-none absolute -top-24 left-1/3 h-48 w-96 rounded-full bg-emerald-400/10 blur-3xl"
      />
      <div class="relative flex items-center justify-between gap-4">
        <div class="flex items-center gap-3.5">
          <div
            class="flex size-11 items-center justify-center rounded-xl border border-emerald-300/20 bg-emerald-400/10 shadow-[inset_0_1px_0_rgba(255,255,255,0.06)]"
          >
            <AudioLines class="size-5.5 text-emerald-300" />
          </div>
          <div>
            <div class="flex items-baseline gap-2.5">
              <h1
                class="bg-gradient-to-r from-emerald-200 via-teal-100 to-emerald-200 bg-clip-text text-xl font-bold tracking-wide text-transparent"
              >
                RustVueVoice
              </h1>
              <span
                class="rounded-md border border-white/10 bg-white/5 px-1.5 py-0.5 text-[10px] font-medium text-foreground/60"
              >
                {{ store.version || "v1.0.0" }}
              </span>
            </div>
            <p class="mt-1 text-xs text-foreground/50">
              双引擎云端语音识别 · Tauri 2 + Vue 3 · 电量在按键与打开窗口时自动刷新
            </p>
          </div>
        </div>
        <BatteryGauge :pct="store.battery" />
      </div>
    </header>

    <main class="flex min-h-0 flex-1 flex-col gap-4 px-6 py-4">
      <!-- 状态卡 -->
      <section class="grid shrink-0 grid-cols-5 gap-3">
        <StatusCard label="识别引擎" :value="store.engine" :icon="Cloud" />
        <StatusCard
          label="豆包登录"
          :value="store.loggedIn ? '已登录' : '未登录'"
          :icon="KeyRound"
          :tone="store.loggedIn ? 'ok' : 'err'"
        />
        <StatusCard
          label="接收器"
          :value="store.deviceConnected ? '已连接' : '未连接（自动重试中）'"
          :icon="Usb"
          :tone="store.deviceConnected ? 'ok' : 'err'"
        />
        <StatusCard
          label="鼠标电量"
          :value="store.battery == null ? '—' : `${store.battery}%`"
          :icon="Mic"
          :tone="store.battery == null ? 'neutral' : store.battery >= 20 ? 'ok' : 'err'"
        />
        <StatusCard
          label="监听"
          :value="store.listening ? '运行中' : '-'"
          :icon="Activity"
        />
      </section>

      <!-- 实时识别条 -->
      <Transition
        enter-active-class="transition duration-200 ease-out"
        enter-from-class="opacity-0 -translate-y-1"
        leave-active-class="transition duration-150 ease-in"
        leave-to-class="opacity-0 -translate-y-1"
      >
        <section
          v-if="store.session !== 'idle'"
          class="relative shrink-0 overflow-hidden rounded-xl border border-emerald-300/15 bg-emerald-400/[0.04] px-4 py-3"
        >
          <div class="flex items-center gap-2.5">
            <template v-if="store.session === 'recording'">
              <span class="relative flex size-2.5">
                <span
                  class="absolute inline-flex size-full animate-ping rounded-full bg-rose-400 opacity-60"
                />
                <span class="relative inline-flex size-2.5 rounded-full bg-rose-400" />
              </span>
              <span class="text-sm font-medium text-rose-200">正在录音 · 松开结束</span>
            </template>
            <template v-else>
              <Loader2 class="size-4 animate-spin text-emerald-300" />
              <span class="text-sm font-medium text-emerald-200">识别中…</span>
            </template>
          </div>
          <p
            v-if="store.partial"
            class="mt-2 line-clamp-2 text-[13px] leading-relaxed text-foreground/75"
          >
            {{ store.partial }}
          </p>
        </section>
      </Transition>

      <!-- 操作区 -->
      <section class="flex shrink-0 flex-wrap items-center gap-2.5">
        <Button
          :disabled="store.loginBusy"
          class="bg-emerald-400/90 text-emerald-950 shadow-[0_0_18px_rgba(52,211,153,0.25)] hover:bg-emerald-300"
          @click="startLogin"
        >
          <Loader2 v-if="store.loginBusy" class="animate-spin" />
          <QrCode v-else />
          扫码登录
        </Button>
        <Button variant="outline" :disabled="store.loginBusy" @click="startBrowserLogin">
          <Globe />
          浏览器登录
        </Button>
        <Button variant="outline" :disabled="store.testBusy" @click="runLinkTest">
          <Loader2 v-if="store.testBusy" class="animate-spin" />
          <FlaskConical v-else />
          云链路测试
        </Button>
        <Button variant="outline" @click="openRecordingsDir">
          <FolderOpen />
          打开录音目录
        </Button>
        <Button variant="ghost" @click="store.helpOpen = true">
          <BookOpen />
          使用说明
        </Button>
        <Button variant="ghost" @click="store.engineSettingsOpen = true">
          <Settings2 />
          引擎设置
        </Button>

        <div class="ml-auto flex items-center gap-2.5">
          <label class="flex cursor-pointer items-center gap-2 text-sm text-foreground/75">
            <Switch
              :model-value="store.autostart"
              @update:model-value="(v) => setAutostart(v === true)"
            />
            开机自动启动
          </label>
          <Button
            variant="ghost"
            class="text-rose-300/90 hover:bg-rose-400/10 hover:text-rose-300"
            @click="exitApp"
          >
            <Power />
            退出
          </Button>
        </div>
      </section>

      <p class="shrink-0 text-xs text-muted-foreground/60">
        长按鼠标语音键说话，松开后文字自动输入到光标处 · 双击语音键隐藏悬浮层 · 关闭本窗口即退出程序
      </p>

      <!-- 日志 -->
      <LogPanel />
    </main>

    <HelpDialog />
    <EngineSettingsDialog />
  </div>
</template>
