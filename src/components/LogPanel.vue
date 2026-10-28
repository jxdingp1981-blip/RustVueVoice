<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { Terminal } from "lucide-vue-next";
import { store } from "@/lib/store";

const viewport = ref<HTMLElement | null>(null);
const stick = ref(true);

function onScroll() {
  const el = viewport.value;
  if (!el) return;
  stick.value = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
}

watch(
  () => store.logs.length,
  async () => {
    if (!stick.value) return;
    await nextTick();
    const el = viewport.value;
    if (el) el.scrollTop = el.scrollHeight;
  },
);

/** 拆分日志行为 时间 | 级别 | 正文 */
function splitLine(line: string): { time: string; level: string; msg: string } {
  const m = line.match(/^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2},\d{3})\s+(INFO|WARNING|ERROR)\s+(.*)$/s);
  if (m) return { time: m[1].slice(11), level: m[2], msg: m[3] };
  return { time: "", level: "", msg: line };
}

function levelColor(level: string, kind?: string): string {
  if (kind === "ok") return "text-emerald-300";
  if (kind === "err") return "text-rose-300";
  if (kind === "result") return "text-sky-300";
  if (level === "WARNING") return "text-amber-300";
  if (level === "ERROR") return "text-rose-300";
  return "text-slate-300/85";
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-white/5 bg-[oklch(0.16_0.008_265)]">
    <div class="flex items-center justify-between border-b border-white/5 px-4 py-2.5">
      <div class="flex items-center gap-2 text-xs font-medium text-muted-foreground">
        <Terminal class="size-3.5" />
        运行日志
      </div>
      <span class="text-[10px] text-muted-foreground/50">logs\rustvuevoice.log 同步落盘</span>
    </div>
    <div
      ref="viewport"
      class="flex-1 overflow-y-auto px-4 py-3 font-mono text-[11.5px] leading-[1.7]"
      @scroll="onScroll"
    >
      <div v-if="store.logs.length === 0" class="text-muted-foreground/50">
        暂无日志。启动后状态与识别过程会实时输出到这里。
      </div>
      <div v-for="(e, i) in store.logs" :key="i" :class="levelColor(splitLine(e.line).level, e.kind)">
        <span v-if="splitLine(e.line).time" class="mr-2 text-slate-500/60">{{ splitLine(e.line).time }}</span>
        <span
          v-if="splitLine(e.line).level"
          class="mr-2 inline-block w-[52px] font-semibold"
          :class="splitLine(e.line).level === 'ERROR' ? 'text-rose-400/80' : splitLine(e.line).level === 'WARNING' ? 'text-amber-400/80' : 'text-slate-500/70'"
        >
          {{ splitLine(e.line).level }}
        </span>
        <span class="whitespace-pre-wrap break-all">{{ splitLine(e.line).msg }}</span>
      </div>
    </div>
  </div>
</template>
