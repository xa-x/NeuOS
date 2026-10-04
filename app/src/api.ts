import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Action =
  | { kind: "openPath"; path: string }
  | { kind: "launchApp"; exec: string; appName: string }
  | { kind: "openUrl"; url: string }
  | { kind: "openReader"; url: string }
  | { kind: "askAi"; prompt: string }
  | { kind: "runShell"; command: string }
  | { kind: "command"; name: string; args: string }
  | { kind: "notImplemented"; what: string };

export interface SearchItem {
  id: string;
  title: string;
  subtitle: string | null;
  badge: string;
  score: number;
  action: Action;
}

export type GroupKind =
  | "commands"
  | "apps"
  | "files"
  | "content"
  | "web"
  | "ai";

export interface ResultGroup {
  kind: GroupKind;
  items: SearchItem[];
}

export interface SearchResponse {
  intent: string;
  groups: ResultGroup[];
}

export interface ShellLine {
  stream: "out" | "err";
  line: string;
}

export interface AgentStep {
  name: string;
  ok: boolean | null;
  summary: string;
  args?: unknown;
}

export interface AgentConfirm {
  id: number;
  tool: string;
  summary: string;
}

export function search(query: string): Promise<SearchResponse> {
  return invoke<SearchResponse>("search", { query });
}

/** Activate an item; empty string means the launcher closed itself. */
export function activate(action: Action): Promise<string> {
  return invoke<string>("activate", { action });
}

export function hideWindow(): Promise<void> {
  return invoke("hide_window");
}

export function activeHotkey(): Promise<string> {
  return invoke<string>("active_hotkey");
}

export function runShell(
  command: string,
  confirmed: boolean,
): Promise<{ status: "started" } | { status: "needConfirm"; reason: string }> {
  return invoke("run_shell", { command, confirmed });
}

export function agentAsk(prompt: string): Promise<void> {
  return invoke("agent_ask", { prompt });
}

export function agentConfirm(id: number, approve: boolean): Promise<boolean> {
  return invoke("agent_confirm", { id, approve });
}

export function toolsList(): Promise<unknown[]> {
  return invoke("tools_list");
}

export function toolRun(name: string, argsJson: string): Promise<void> {
  return invoke("tool_run", { name, argsJson });
}

export function toolsNew(description: string): Promise<void> {
  return invoke("tools_new", { description });
}

export function webSearch(query: string): Promise<SearchItem[]> {
  return invoke("web_search", { query });
}

export function indexStatus(): Promise<string> {
  return invoke("index_status");
}

export function onLauncherShown(cb: () => void): Promise<UnlistenFn> {
  return listen("launcher-shown", () => cb());
}

export function onIndexProgress(
  cb: (p: { files: number; docs: number; done: boolean }) => void,
): Promise<UnlistenFn> {
  return listen("index-progress", (e) => cb(e.payload as never));
}

export function onShellOut(cb: (l: ShellLine) => void): Promise<UnlistenFn> {
  return listen("shell-out", (e) => cb(e.payload as never));
}

export function onShellDone(cb: (code: number | null) => void): Promise<UnlistenFn> {
  return listen("shell-done", (e) => cb((e.payload as { code: number | null }).code));
}

export interface AgentEvents {
  meta?: (m: { provider: string; model: string }) => void;
  delta?: (text: string) => void;
  toolStart?: (s: { name: string; args: unknown }) => void;
  toolDone?: (s: { name: string; ok: boolean; summary: string }) => void;
  confirm?: (c: AgentConfirm) => void;
  done?: (d: { answer: string; steps: number }) => void;
  error?: (message: string) => void;
}

export function onAgentEvents(handlers: AgentEvents): Promise<UnlistenFn>[] {
  const listens: Promise<UnlistenFn>[] = [];
  const on = <T,>(event: string, cb?: (p: T) => void) => {
    if (cb) listens.push(listen(event, (e) => cb(e.payload as T)));
  };
  on("agent-meta", handlers.meta);
  on("agent-delta", handlers.delta);
  on("agent-tool-start", handlers.toolStart);
  on("agent-tool-done", handlers.toolDone);
  on("agent-confirm", handlers.confirm);
  on("agent-done", handlers.done);
  on("agent-error", handlers.error);
  return listens;
}
