import { useCallback, useEffect, useRef, useState } from "react";
import {
  activate,
  activeHotkey,
  agentAsk,
  agentConfirm,
  hideWindow,
  indexStatus,
  onAgentEvents,
  onIndexProgress,
  onLauncherShown,
  onShellDone,
  onShellOut,
  runShell,
  search,
  toolRun,
  toolsList,
  toolsNew,
  webSearch,
  type Action,
  type AgentConfirm,
  type AgentStep,
  type GroupKind,
  type SearchItem,
  type SearchResponse,
  type ShellLine,
} from "./api";

const GROUP_LABELS: Record<GroupKind, string> = {
  commands: "Commands",
  apps: "Apps",
  files: "Files",
  content: "In files",
  web: "Web",
  ai: "AI",
};

type Row =
  | { type: "header"; kind: GroupKind }
  | { type: "item"; item: SearchItem; flatIndex: number };

interface ShellView {
  title: string;
  cmd: string;
  lines: ShellLine[];
  exit: number | null;
  running: boolean;
  needConfirm: string | null;
}

interface AnswerView {
  meta: string | null;
  text: string;
  steps: AgentStep[];
  confirm: AgentConfirm | null;
  running: boolean;
  error: string | null;
}

function freshShell(title: string, cmd: string): ShellView {
  return { title, cmd, lines: [], exit: null, running: false, needConfirm: null };
}

function freshAnswer(): AnswerView {
  return { meta: null, text: "", steps: [], confirm: null, running: false, error: null };
}

function badgeFor(item: SearchItem): string {
  switch (item.action.kind) {
    case "command":
      return "/";
    case "askAi":
      return "◆";
    case "runShell":
      return ">_";
    case "openUrl":
      return "↗";
    case "openReader":
      return "R";
    default:
      return item.badge;
  }
}

export default function App() {
  const [query, setQuery] = useState("");
  const [response, setResponse] = useState<SearchResponse>({ intent: "", groups: [] });
  const [selected, setSelected] = useState(0);
  const [groupFilter, setGroupFilter] = useState<GroupKind | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [hotkey, setHotkey] = useState("alt+space");
  const [idx, setIdx] = useState<{ files: number; done: boolean } | null>(null);

  const [view, setView] = useState<"search" | "shell" | "answer">("search");
  const [shell, setShell] = useState<ShellView>(freshShell("", ""));
  const [answer, setAnswer] = useState<AnswerView>(freshAnswer());

  const inputRef = useRef<HTMLInputElement>(null);
  const outRef = useRef<HTMLDivElement>(null);
  const rowRefs = useRef<(HTMLDivElement | null)[]>([]);
  const toastTimer = useRef<number | null>(null);
  const searchSeq = useRef(0);
  const viewRef = useRef(view);
  viewRef.current = view;

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 3600);
  }, []);

  // ---- global wiring ------------------------------------------------------

  useEffect(() => {
    const unlisteners: Promise<{ (): void }>[] = [];

    unlisteners.push(
      onLauncherShown(() => {
        setQuery("");
        setGroupFilter(null);
        setToast(null);
        setView("search");
        setShell(freshShell("", ""));
        setAnswer(freshAnswer());
        setSelected(0);
        requestAnimationFrame(() => inputRef.current?.focus());
      }),
    );
    unlisteners.push(activeHotkey().then(setHotkey).then(() => () => {}));
    unlisteners.push(
      onIndexProgress((p) => {
        setIdx({ files: p.files, done: p.done });
        if (p.done) window.setTimeout(() => setIdx(null), 4000);
      }),
    );

    unlisteners.push(
      onShellOut((l) => {
        if (viewRef.current !== "shell") return;
        setShell((s) => ({ ...s, lines: [...s.lines.slice(-400), l] }));
      }),
    );
    unlisteners.push(
      onShellDone((code) => {
        if (viewRef.current !== "shell") return;
        setShell((s) => ({ ...s, exit: code, running: false, needConfirm: null }));
      }),
    );

    unlisteners.push(
      ...onAgentEvents({
        meta: (m) => setAnswer((a) => ({ ...a, meta: `${m.provider} · ${m.model}` })),
        delta: (t) => setAnswer((a) => ({ ...a, text: a.text + t })),
        toolStart: (s) =>
          setAnswer((a) => ({ ...a, steps: [...a.steps, { name: s.name, ok: null, summary: "", args: s.args }] })),
        toolDone: (s) =>
          setAnswer((a) => ({
            ...a,
            steps: a.steps.map((st, i) =>
              i === a.steps.length - 1 && st.ok === null ? { ...st, ok: s.ok, summary: s.summary } : st,
            ),
          })),
        confirm: (c) => setAnswer((a) => ({ ...a, confirm: c })),
        done: () => setAnswer((a) => ({ ...a, running: false, confirm: null })),
        error: (message) => setAnswer((a) => ({ ...a, running: false, error: message, confirm: null })),
      }),
    );

    return () => {
      for (const u of unlisteners) u.then((f) => f());
    };
  }, []);

  // search-as-you-type with stale-response guard
  useEffect(() => {
    let alive = true;
    const seq = ++searchSeq.current;
    if (viewRef.current !== "search") return;
    search(query)
      .then((res) => {
        if (alive && seq === searchSeq.current) {
          setResponse(res);
          setSelected(0);
        }
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [query]);

  // autoscroll shell output and answer text
  useEffect(() => {
    outRef.current?.scrollTo({ top: outRef.current.scrollHeight });
  }, [shell.lines, answer.text]);

  // ---- flows --------------------------------------------------------------

  const startShell = useCallback(
    async (cmd: string, title?: string) => {
      setView("shell");
      setShell({ ...freshShell(title ?? `> ${cmd}`, cmd), running: true });
      const res = await runShell(cmd, false).catch((e: string) => {
        showToast(String(e));
        return null;
      });
      if (!res) return;
      if (res.status === "needConfirm") {
        setShell((s) => ({ ...s, running: false, needConfirm: res.reason }));
      }
    },
    [showToast],
  );

  const startAnswer = useCallback(async (prompt: string) => {
    setView("answer");
    setAnswer({ ...freshAnswer(), running: true });
    await agentAsk(prompt).catch((e: string) =>
      setAnswer((a) => ({ ...a, running: false, error: String(e) })),
    );
  }, []);

  const runCommand = useCallback(
    async (name: string, args: string) => {
      switch (name) {
        case "ai":
          if (!args.trim()) {
            showToast("Type your question after /ai — or just use ?your question");
            return;
          }
          await startAnswer(args);
          break;
        case "run":
          if (!args.trim()) {
            showToast("Type a command after /run — or use >command");
            return;
          }
          await startShell(args);
          break;
        case "web": {
          if (!args.trim()) {
            showToast("Type a search after /web");
            return;
          }
          const items = await webSearch(args).catch(() => []);
          if (items.length === 0) {
            showToast("No results (or network unavailable)");
            return;
          }
          setResponse({ intent: "WEB", groups: [{ kind: "web", items }] });
          setSelected(0);
          break;
        }
        case "tools": {
          const rest = args.trim();
          if (rest === "new" || rest.startsWith("new ")) {
            const description = rest.slice(3).trim();
            if (!description) {
              showToast("Describe the tool: /tools new summarize my git log");
              return;
            }
            setView("answer");
            setAnswer({ ...freshAnswer(), running: true });
            await toolsNew(description);
            return;
          }
          const tools = (await toolsList().catch(() => [])) as {
            name: string;
            description: string;
            source: string;
          }[];
          const items: SearchItem[] = tools.map((t) => ({
            id: `tool:${t.name}`,
            title: `/${t.name}  —  ${t.description}`,
            subtitle: t.source,
            badge: t.source === "plugin" ? "PL" : "B",
            score: 1,
            action: { kind: "command", name: t.name, args: "" },
          }));
          setResponse({ intent: "TOOLS", groups: [{ kind: "commands", items }] });
          setSelected(0);
          break;
        }
        case "index":
          showToast(await indexStatus().catch(() => "index status unavailable"));
          break;
        case "settings":
          showToast(await activate({ kind: "command", name: "settings", args: "" }));
          break;
        default:
          // plugin tool
          setView("shell");
          setShell({ ...freshShell(`/${name} ${args}`.trim(), name), running: true });
          await toolRun(name, JSON.stringify(args ? { input: args } : {})).catch((e: string) =>
            showToast(String(e)),
          );
      }
    },
    [showToast, startAnswer, startShell],
  );

  const runItem = useCallback(
    async (item: SearchItem) => {
      const a = item.action;
      switch (a.kind) {
        case "runShell":
          await startShell(a.command);
          return;
        case "askAi":
          await startAnswer(a.prompt);
          return;
        case "command":
          await runCommand(a.name, a.args);
          return;
        default:
          try {
            const message = await activate(a as Action);
            if (message) showToast(message);
          } catch (e) {
            showToast(String(e));
          }
      }
    },
    [runCommand, showToast, startAnswer, startShell],
  );

  // ---- keyboard -----------------------------------------------------------

  const visibleGroups = groupFilter
    ? response.groups.filter((g) => g.kind === groupFilter)
    : response.groups;
  const rows: Row[] = [];
  let flatIndex = 0;
  for (const group of visibleGroups) {
    rows.push({ type: "header", kind: group.kind });
    for (const item of group.items) rows.push({ type: "item", item, flatIndex: flatIndex++ });
  }
  const flatItems = visibleGroups.flatMap((g) => g.items);

  useEffect(() => {
    rowRefs.current[selected]?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      void hideWindow();
      return;
    }
    if (view === "shell") {
      if (e.key === "Enter") {
        e.preventDefault();
        if (shell.needConfirm) {
          const cmd = shell.cmd;
          setShell((s) => ({ ...s, running: true, needConfirm: null }));
          void runShell(cmd, true);
        } else if (!shell.running) {
          void hideWindow();
        }
      } else if (e.key === "Tab" && shell.needConfirm) {
        e.preventDefault();
        setShell((s) => ({ ...s, needConfirm: null, exit: 130 }));
      }
      return;
    }
    if (view === "answer") {
      if (e.key === "Enter") {
        e.preventDefault();
        if (answer.confirm) {
          const id = answer.confirm.id;
          setAnswer((a) => ({ ...a, confirm: null }));
          void agentConfirm(id, true);
        } else if (!answer.running) {
          void hideWindow();
        }
      } else if (e.key === "Tab" && answer.confirm) {
        e.preventDefault();
        const id = answer.confirm.id;
        setAnswer((a) => ({ ...a, confirm: null }));
        void agentConfirm(id, false);
      }
      return;
    }
    // search view
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (flatItems.length === 0) return;
      setSelected((s) =>
        e.key === "ArrowDown" ? (s + 1) % flatItems.length : (s - 1 + flatItems.length) % flatItems.length,
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      const item = flatItems[selected];
      if (item) void runItem(item);
    } else if (e.key === "Tab") {
      e.preventDefault();
      const item = flatItems[selected];
      if (groupFilter || !item) setGroupFilter(null);
      else {
        const kind = visibleGroups.find((g) => g.items.includes(item))?.kind;
        if (kind) setGroupFilter(kind);
      }
      setSelected(0);
    }
  };

  const resultCount = flatItems.length;
  const intentRight = idx
    ? idx.done
      ? `index ready · ${fmtCount(idx.files)} files`
      : `indexing ${fmtCount(idx.files)}…`
    : resultCount > 0
      ? `${resultCount} results`
      : "";

  return (
    <div className="h-full w-full">
      <div className="neu-card neu-material mx-auto flex max-h-[540px] w-full flex-col overflow-hidden rounded-[20px]">
        {/* input row — padding areas are drag handles */}
        <div data-tauri-drag-region className="flex h-[52px] shrink-0 cursor-default items-center gap-3 px-4">
          <span data-tauri-drag-region className="text-[13px] leading-none text-signal">◆</span>
          {view === "search" ? (
            <input
              ref={inputRef}
              value={query}
              onChange={(e) => {
                setQuery(e.currentTarget.value);
                setGroupFilter(null);
              }}
              onKeyDown={onKeyDown}
              className="neu-input min-w-0 flex-1 bg-transparent text-[15px] text-snow caret-signal outline-none placeholder:text-mist/60"
              placeholder="Search files, apps, web —   / commands   > shell   ? ask AI"
              autoFocus
              spellCheck={false}
              autoComplete="off"
            />
          ) : (
            <div
              ref={inputRef as never}
              tabIndex={0}
              onKeyDown={onKeyDown as never}
              className="min-w-0 flex-1 truncate text-[15px] text-snow outline-none"
            >
              {view === "shell" ? shell.title : answer.meta ?? "thinking…"}
            </div>
          )}
          <span data-tauri-drag-region className="neu-sub font-mono text-[10px] tracking-[0.08em] text-mist/70">
            {hotkey}
          </span>
        </div>

        {/* intent strip */}
        <div data-tauri-drag-region className="neu-seam flex h-[24px] shrink-0 cursor-default items-center justify-between px-4">
          <span data-tauri-drag-region className="flex items-center gap-2 font-mono text-[10px] font-medium uppercase tracking-[0.14em] text-mist">
            <span
              className={`inline-block h-[5px] w-[5px] rounded-full ${
                response.intent ? "bg-signal" : "bg-mist/40"
              } ${answer.running || shell.running ? "animate-pulse" : ""}`}
            />
            {view === "shell"
              ? shell.needConfirm
                ? "CONFIRM"
                : shell.running
                  ? "RUNNING"
                  : `EXIT ${shell.exit ?? "?"}`
              : view === "answer"
                ? answer.error
                  ? "ERROR"
                  : answer.running
                    ? "AGENT"
                    : "DONE"
                : response.intent || "ready"}
            {groupFilter && (
              <span className="rounded-sm bg-signal/15 px-1.5 py-px text-signal">
                {GROUP_LABELS[groupFilter]}
              </span>
            )}
          </span>
          <span data-tauri-drag-region className="font-mono text-[10px] text-mist/60">
            {intentRight}
          </span>
        </div>

        {/* body */}
        {view === "search" ? (
          rows.length > 0 ? (
            <div role="listbox" aria-label="Results" className="neu-scroll neu-seam neu-view min-h-0 flex-1 overflow-y-auto pb-1">
              {rows.map((row, i) =>
                row.type === "header" ? (
                  <div
                    key={`h-${row.kind}-${i}`}
                    className="neu-group-header px-4 pb-1.5 pt-2.5 font-mono text-[10px] font-medium uppercase tracking-[0.18em] text-mist/80"
                  >
                    {GROUP_LABELS[row.kind]}
                  </div>
                ) : (
                  <div
                    key={row.item.id + row.flatIndex}
                    ref={(el) => {
                      rowRefs.current[row.flatIndex] = el;
                    }}
                    onMouseEnter={() => setSelected(row.flatIndex)}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      void runItem(row.item);
                    }}
                    role="option"
                    aria-selected={selected === row.flatIndex}
                    className={`neu-row relative flex h-[44px] cursor-default items-center gap-3 px-4 ${
                      selected === row.flatIndex ? "bg-signal/[0.13]" : ""
                    }`}
                  >
                    {selected === row.flatIndex && (
                      <span className="neu-bar absolute left-0 top-1/2 h-5 w-[2px] -translate-y-1/2 rounded-full bg-signal" />
                    )}
                    <span className="neu-badge flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-[8px] font-mono text-[9px] font-medium tracking-[0.02em] text-mist">
                      {badgeFor(row.item)}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-[13.5px] text-snow">
                      {row.item.title}
                    </span>
                    {row.item.subtitle && (
                      <span className="neu-sub max-w-[45%] shrink-0 truncate text-right text-[11px] text-mist/95">
                        {row.item.subtitle}
                      </span>
                    )}
                  </div>
                ),
              )}
            </div>
          ) : (
            <div className="neu-seam neu-view px-4 py-7 text-center font-mono text-[11px] tracking-[0.01em] text-mist/70">
              no matches — try fewer letters, or <span className="text-signal">?</span> to ask AI
            </div>
          )
        ) : view === "shell" ? (
          <div className="neu-seam neu-view min-h-0 flex-1">
            <div ref={outRef} className="neu-scroll h-full max-h-[400px] overflow-y-auto px-4 py-3">
              {shell.needConfirm ? (
                <div className="neu-confirm rounded-[10px] p-3 font-mono text-[12px] text-signal">
                  ⚠ {shell.needConfirm}
                </div>
              ) : shell.lines.length === 0 && shell.running ? (
                <div className="font-mono text-[12px] text-mist/70">running…</div>
              ) : (
                shell.lines.map((l, i) => (
                  <div
                    key={i}
                    className={`whitespace-pre-wrap break-all font-mono text-[12px] leading-relaxed ${
                      l.stream === "err" ? "text-red-400/90" : "text-snow/90"
                    }`}
                  >
                    {l.line || "\u00A0"}
                  </div>
                ))
              )}
            </div>
          </div>
        ) : (
          <div ref={outRef} className="neu-scroll neu-seam neu-view min-h-0 flex-1 overflow-y-auto px-4 py-3">
            {answer.steps.length > 0 && (
              <div className="mb-3 space-y-1">
                {answer.steps.map((s, i) => (
                  <div key={i} className="flex items-start gap-2 font-mono text-[11px] leading-relaxed">
                    <span className={s.ok === null ? "text-mist" : s.ok ? "text-emerald-400" : "text-red-400"}>
                      {s.ok === null ? "…" : s.ok ? "✓" : "✗"}
                    </span>
                    <span className="text-mist">
                      <span className="text-snow/80">{s.name}</span>
                      {s.summary ? ` — ${s.summary}` : ""}
                    </span>
                  </div>
                ))}
              </div>
            )}
            {answer.confirm && (
              <div className="neu-confirm mb-3 rounded-[10px] p-3 font-mono text-[12px] text-signal">
                ⚠ {answer.confirm.tool}: {answer.confirm.summary}
              </div>
            )}
            {answer.error ? (
              <div className="whitespace-pre-wrap font-mono text-[12px] leading-relaxed text-red-400/90">
                {answer.error}
              </div>
            ) : (
              <div
                className={`whitespace-pre-wrap break-words text-[13.5px] leading-relaxed text-snow/95 ${
                  answer.running ? "neu-caret" : ""
                }`}
              >
                {answer.text || (answer.running ? "…" : "")}
              </div>
            )}
          </div>
        )}

        {toast && (
          <div className="neu-toast neu-seam bg-signal/10 px-4 py-2 font-mono text-[11px] tracking-[0.01em] text-signal">
            {toast}
          </div>
        )}

        {/* footer hints */}
        <div className="neu-seam flex h-[26px] shrink-0 items-center justify-between px-4 font-mono text-[10px] tracking-[0.01em] text-mist/70">
          <span>
            {view === "search"
              ? "↑↓ move · ⏎ open · tab filter"
              : view === "shell"
                ? shell.needConfirm
                  ? "⏎ run anyway · tab cancel"
                  : shell.running
                    ? "streaming…"
                    : "⏎ close"
                : answer.confirm
                  ? "⏎ approve · tab decline"
                  : answer.running
                    ? "agent working…"
                    : "⏎ close"}
          </span>
          <span>esc hide</span>
        </div>
      </div>
    </div>
  );
}

function fmtCount(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
