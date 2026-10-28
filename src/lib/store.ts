import { reactive } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface StatusSnapshot {
  engine: string;
  logged_in: boolean;
  device_connected: boolean;
  listening: boolean;
  battery: number | null;
  recordings_dir: string;
  autostart: boolean;
  version: string;
}

export type SessionState = "idle" | "recording" | "recognizing";

export interface LogEntry {
  line: string;
  kind?: "ok" | "err" | "warn" | "result";
}

export const store = reactive({
  engine: "豆包云端",
  loggedIn: false,
  deviceConnected: false,
  listening: false,
  battery: null as number | null,
  recordingsDir: "",
  autostart: false,
  version: "",
  session: "idle" as SessionState,
  partial: "",
  logs: [] as LogEntry[],
  loginBusy: false,
  testBusy: false,
  helpOpen: false,
  helpText: "",
  // 识别引擎配置
  asrEngine: "doubao" as "doubao" | "volc",
  apiKey: "",
  resourceId: "",
  engineSettingsOpen: false,
});

let unlisteners: UnlistenFn[] = [];

export function pushLog(entry: LogEntry) {
  store.logs.push(entry);
  if (store.logs.length > 600) {
    store.logs.splice(0, store.logs.length - 600);
  }
}

function applyStatus(key: string, value: string) {
  switch (key) {
    case "engine":
      store.engine = value;
      break;
    case "login":
      store.loggedIn = value === "已登录";
      break;
    case "device":
      store.deviceConnected = value === "已连接";
      break;
    case "battery": {
      if (value.endsWith("%")) {
        const pct = parseInt(value.slice(0, -1), 10);
        store.battery = Number.isNaN(pct) ? null : pct;
      } else {
        store.battery = null;
      }
      break;
    }
    case "listen":
      store.listening = value === "运行中";
      break;
  }
}

export async function initStore() {
  const snap = await invoke<StatusSnapshot>("get_status");
  store.engine = snap.engine;
  store.loggedIn = snap.logged_in;
  store.deviceConnected = snap.device_connected;
  store.listening = snap.listening;
  store.battery = snap.battery;
  store.recordingsDir = snap.recordings_dir;
  store.autostart = snap.autostart;
  store.version = snap.version;
  invoke<string>("get_help")
    .then((t) => (store.helpText = t))
    .catch(() => {});
  invoke<{ engine: string; api_key: string; resource_id: string }>("get_asr_config")
    .then((c) => {
      store.asrEngine = c.engine === "volc" ? "volc" : "doubao";
      store.apiKey = c.api_key;
      store.resourceId = c.resource_id;
    })
    .catch(() => {});

  unlisteners = await Promise.all([
    listen<{ key: string; value: string }>("status", (e) => {
      applyStatus(e.payload.key, e.payload.value);
    }),
    listen<{ line: string }>("log", (e) => {
      pushLog({ line: e.payload.line });
    }),
    listen<{ ok: boolean }>("login-done", (e) => {
      store.loginBusy = false;
      pushLog({
        line: e.payload.ok ? "✓ 登录成功，凭证已保存" : "✗ 未完成登录（详见日志）",
        kind: e.payload.ok ? "ok" : "err",
      });
    }),
    listen<{ summary: string }>("test-done", (e) => {
      store.testBusy = false;
      const s = e.payload.summary;
      const kind = s.startsWith("链路正常")
        ? ("ok" as const)
        : s.startsWith("未登录") || s.startsWith("凭证")
          ? ("err" as const)
          : ("result" as const);
      pushLog({ line: s, kind });
    }),
    listen<{ text: string }>("partial", (e) => {
      store.partial = e.payload.text;
    }),
    listen<{ state: SessionState }>("session", (e) => {
      store.session = e.payload.state;
      if (e.payload.state === "idle") store.partial = "";
    }),
  ]);

  // 监听就绪后领取启动阶段的积压日志（避免丢启动日志）
  const backlog = await invoke<string[]>("frontend_ready").catch(() => [] as string[]);
  for (const line of backlog) pushLog({ line });
}

export function disposeStore() {
  for (const u of unlisteners) u();
  unlisteners = [];
}
