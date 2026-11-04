// Application state, the API client, the router and live updates.
import { useState, useEffect } from "./lib.js";

export const state = {
  overview: null,
  health: null,
  projects: {},
  chats: {},
  route: parseRoute(location.pathname),
  panel: null,
  dockedPanels: recall("panel.dockedPanels", {}),
  toasts: [],
  connected: true,
  replyTo: recall("draft.replyTo", {}),
  viewVersion: {},
  error: null,
};
state.panel = panelFor(state.route.slug);

const listeners = new Set();
let notifyScheduled = false;
let version = 0;

export function notify() {
  version += 1;
  if (notifyScheduled) return;
  notifyScheduled = true;
  // Not requestAnimationFrame: it stops in background tabs, which would hold up
  // everything queued behind it until the tab is shown again.
  setTimeout(() => {
    notifyScheduled = false;
    listeners.forEach((l) => l());
  }, 0);
}

export function update(fn) {
  fn(state);
  notify();
}

/** Re-render this component whenever the store changes. */
export function useStore() {
  const [, force] = useState(0);
  const rendered = version;
  useEffect(() => {
    const listener = () => force((n) => n + 1);
    listeners.add(listener);
    // Effects run after the first paint. Anything that changed before this point
    // notified nobody, and without this the page would sit blank until the next change.
    if (version !== rendered) listener();
    return () => listeners.delete(listener);
  }, []);
  return state;
}

/** Re-render periodically, for "3m ago" labels. */
export function useTick(ms = 15000) {
  const [, force] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => force((n) => n + 1), ms);
    return () => clearInterval(timer);
  }, [ms]);
}

/** Whether a media query matches, kept current as the window changes. */
export function useMedia(query) {
  const [matches, setMatches] = useState(() => matchMedia(query).matches);
  useEffect(() => {
    const list = matchMedia(query);
    const onChange = () => setMatches(list.matches);
    list.addEventListener("change", onChange);
    return () => list.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

// ------------------------------------------------------------- API

export async function api(path, options = {}) {
  const init = { credentials: "same-origin", ...options };
  if (options.json !== undefined) {
    init.body = JSON.stringify(options.json);
    init.headers = { "Content-Type": "application/json", ...(options.headers || {}) };
    init.method = options.method || "POST";
    delete init.json;
  }
  const response = await fetch(path, init);
  if (response.status === 401) {
    location.reload();
    throw new Error("Not signed in");
  }
  const text = await response.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = null;
  }
  if (!response.ok) throw Object.assign(new Error((data && data.error) || text || `Request failed (${response.status})`), { status: response.status });
  return data;
}

export function toast(text, kind = "info") {
  const entry = { id: Math.random(), text, kind };
  update((s) => s.toasts.push(entry));
  setTimeout(() => update((s) => (s.toasts = s.toasts.filter((t) => t !== entry))), kind === "error" ? 7000 : 3200);
}

/** Run an action, turning any failure into a visible message. */
export async function attempt(fn) {
  try {
    return await fn();
  } catch (e) {
    toast(e.message || String(e), "error");
    return undefined;
  }
}

// ------------------------------------------------------------- router

export function parseRoute(path) {
  const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
  if (parts[0] === "memory") return { view: "memory" };
  if (parts[0] === "terminal") return { view: "terminal" };
  if (parts[0] === "health") return { view: "health" };
  if (parts[0] === "p" && parts[1]) {
    const slug = parts[1];
    if (parts[2] === "terminal") return { view: "project", slug, tab: "terminal", agent: "manager" };
    if (parts[2] === "settings") return { view: "project", slug, tab: "settings", agent: "manager" };
    if (parts[2] === "agents" && parts[3]) {
      if (parts[4] === "turns" && parts[5]) return { view: "project", slug, tab: "chat", agent: parts[3], turn: parts[5] };
      return { view: "project", slug, tab: "chat", agent: parts[3] };
    }
    return { view: "project", slug, tab: "chat", agent: "manager" };
  }
  return { view: "project", slug: "home", tab: "chat", agent: "manager" };
}

export function projectPath(slug) {
  return slug === "home" ? "/" : `/p/${encodeURIComponent(slug)}`;
}

export function agentPath(slug, agent) {
  if (agent === "manager") return projectPath(slug);
  return `/p/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}`;
}

export function turnPath(slug, agent, turn) {
  return `/p/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}/turns/${encodeURIComponent(turn)}`;
}

function applyRoute(route) {
  const before = state.route;
  state.route = route;
  // An artifact belongs to its project. Leaving closes an overlay for good, while a docked
  // panel waits for the project to be shown again.
  if (route.slug !== before.slug) state.panel = panelFor(route.slug);
  notify();
  refreshCurrent();
  if (before.slug !== route.slug || before.agent !== route.agent) window.scrollTo(0, 0);
}

export function navigate(path, { replace = false } = {}) {
  if (path === location.pathname) return;
  if (replace) history.replaceState(null, "", path);
  else history.pushState(null, "", path);
  applyRoute(parseRoute(path));
}

window.addEventListener("popstate", () => applyRoute(parseRoute(location.pathname)));

// ------------------------------------------------------------- loading data

export function chatKey(slug, agent) {
  return `${slug}/${agent}`;
}

function mergeItems(existing, fresh) {
  const byId = new Map((existing || []).map((i) => [i.id, i]));
  for (const item of fresh) byId.set(item.id, item);
  return [...byId.values()].sort((a, b) => (a.id.slice(2) < b.id.slice(2) ? -1 : 1));
}

export async function loadOverview() {
  const overview = await api("/api/overview");
  update((s) => {
    s.overview = overview;
    s.error = null;
  });
}

export async function loadHealth() {
  const health = await api("/api/health");
  update((s) => (s.health = health));
}

export async function loadProject(slug) {
  const query = slug === "home" ? "?scope=all" : "";
  const data = await api(`/api/projects/${encodeURIComponent(slug)}${query}`);
  update((s) => (s.projects[slug] = data));
}

export async function loadChat(slug, agent, { before, since } = {}) {
  const params = new URLSearchParams();
  if (before) params.set("before", before);
  if (since) params.set("since", since);
  const query = params.toString();
  const data = await api(`/api/projects/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}/chat${query ? `?${query}` : ""}`);
  update((s) => {
    const key = chatKey(slug, agent);
    const existing = s.chats[key];
    if (before) {
      s.chats[key] = { ...existing, items: mergeItems(existing && existing.items, data.items), hasMore: data.has_more, loaded: true };
    } else if (since) {
      s.chats[key] = { ...existing, items: mergeItems(existing && existing.items, data.items), loaded: true };
    } else {
      // Keep pages of older items already loaded, but take this response's word for the
      // window it covers, so a note that disappeared doesn't linger.
      const oldest = data.items.length ? data.items[0].id.slice(2) : null;
      const older = existing && existing.expanded && oldest ? existing.items.filter((i) => i.id.slice(2) < oldest) : [];
      s.chats[key] = {
        items: mergeItems(older, data.items),
        running: data.running,
        hasMore: existing && existing.expanded ? existing.hasMore : data.has_more,
        expanded: existing && existing.expanded,
        loaded: true,
      };
    }
  });
}

export async function loadEarlier(slug, agent) {
  const key = chatKey(slug, agent);
  const chat = state.chats[key];
  if (!chat || !chat.items.length) return;
  chat.expanded = true;
  await loadChat(slug, agent, { before: chat.items[0].id });
}

let refreshing = false;
let refreshAgain = false;

/** Fetch what's on screen. Calls that arrive during a fetch coalesce into one more. */
export async function refreshCurrent() {
  if (refreshing) {
    refreshAgain = true;
    return;
  }
  refreshing = true;
  try {
    const route = state.route;
    const jobs = [loadOverview()];
    if (route.view === "health") jobs.push(loadHealth());
    if (route.view === "project") {
      jobs.push(loadProject(route.slug));
      if (route.tab === "chat") jobs.push(loadChat(route.slug, route.agent));
    }
    await Promise.all(jobs);
  } catch (e) {
    update((s) => (s.error = e.message));
  } finally {
    refreshing = false;
    if (refreshAgain) {
      refreshAgain = false;
      refreshCurrent();
    }
  }
}

// ------------------------------------------------------------- live updates

let invalidateTimer = null;
const artifactListeners = new Set();

export function onArtifactChange(listener) {
  artifactListeners.add(listener);
  return () => artifactListeners.delete(listener);
}

function invalidate() {
  clearTimeout(invalidateTimer);
  invalidateTimer = setTimeout(refreshCurrent, 60);
}

export function connectEvents() {
  const source = new EventSource("/api/events");
  source.onopen = () => {
    if (!state.connected) refreshCurrent();
    update((s) => (s.connected = true));
  };
  source.onerror = () => update((s) => (s.connected = false));
  source.onmessage = (event) => {
    let data;
    try {
      data = JSON.parse(event.data);
    } catch {
      return;
    }
    if (data.type === "artifacts") artifactListeners.forEach((l) => l(data));
    invalidate();
  };
  // A safety net: running-turn clocks, and anything an event might have missed.
  setInterval(() => {
    if (document.visibilityState === "visible") refreshCurrent();
  }, 20000);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") refreshCurrent();
  });
}

// ------------------------------------------------------------- actions

export function markSeen(slug, question, version) {
  const project = state.projects[slug];
  for (const chat of Object.values(state.chats)) {
    const item = chat.items && chat.items.find((i) => i.id === question);
    if (item) item.seen = Math.max(item.seen || 0, version);
  }
  if (project) {
    for (const agent of Object.values(project.unread.agents)) {
      agent.updated = agent.updated.filter((u) => u.question !== question || u.version > version);
    }
  }
  notify();
  attempt(() => api(`/api/projects/${encodeURIComponent(slug)}/seen`, { json: { versions: { [question]: version } } }));
}

export function markAttentionSeen(item) {
  if (item.state !== "new") return;
  item.state = "seen"; // Show it right away; the next fetch confirms it.
  const project = state.projects[state.route.slug];
  if (project && project.attention_counts) {
    const open = project.attention;
    const fresh = open.filter((i) => i.state === "new").length;
    project.attention_counts = {
      open: open.length,
      new: fresh,
      label: !fresh ? `${open.length} open` : fresh === open.length ? `${open.length} need${open.length === 1 ? "s" : ""} you` : `${fresh} new · ${open.length} open`,
    };
  }
  notify();
  attempt(() => api(`/api/projects/${encodeURIComponent(item.project)}/attention/${item.id}/seen`, { method: "POST" }));
}

export function setArchived(slug, archived) {
  return attempt(async () => {
    const project = await api(`/api/projects/${encodeURIComponent(slug)}/${archived ? "archive" : "unarchive"}`, { method: "POST" });
    toast(project.restoring ? `Restoring ${project.name}. It's un-archived once everything is back.` : `${archived ? "Archived" : "Un-archived"} ${project.name}`);
    await refreshCurrent();
  });
}

export function setReply(slug, agent, target) {
  update((s) => (s.replyTo[chatKey(slug, agent)] = target));
  remember("draft.replyTo", state.replyTo);
}

/** The panel a project shows on arrival. `restored` tells it to go quietly if the artifact is gone. */
function panelFor(slug) {
  const docked = state.dockedPanels[slug];
  return docked ? { ...docked, restored: true } : null;
}

/** Every change to the open panel comes through here, which keeps dockedPanels true to it. */
function setPanel(slug, panel) {
  update((s) => {
    s.panel = panel;
    if (panel && panel.docked) s.dockedPanels[slug] = panel;
    else delete s.dockedPanels[slug];
  });
  remember("panel.dockedPanels", state.dockedPanels);
}

export function openArtifact(slug, path, rev = null) {
  const docked = state.panel && state.panel.slug === slug ? state.panel.docked : recall("panel.docked", false);
  setPanel(slug, { slug, path, rev, docked });
}

export function closeArtifact() {
  if (state.panel) setPanel(state.panel.slug, null);
}

export function dockArtifact(docked) {
  remember("panel.docked", docked);
  setPanel(state.panel.slug, { ...state.panel, docked });
}

export function toggleArtifact(slug, path) {
  if (state.panel && state.panel.slug === slug && state.panel.path === path) closeArtifact();
  else openArtifact(slug, path);
}

// ------------------------------------------------------------- formatting

export function timeAgo(ts) {
  if (!ts) return "";
  const seconds = Math.max(0, Date.now() / 1000 - ts);
  if (seconds < 45) return "just now";
  if (seconds < 90) return "1m ago";
  if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)}h ago`;
  if (seconds < 86400 * 30) return `${Math.round(seconds / 86400)}d ago`;
  return new Date(ts * 1000).toLocaleDateString();
}

export function clock(ts) {
  if (!ts) return "";
  return new Date(ts * 1000).toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

export function elapsed(ts) {
  if (!ts) return "";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
}

export function timeUntil(ts) {
  if (!ts) return "";
  const minutes = Math.round((ts - Date.now() / 1000) / 60);
  if (minutes <= 0) return "due now";
  if (minutes < 90) return `in ${minutes}m`;
  if (minutes < 60 * 36) return `in ${Math.round(minutes / 60)}h`;
  return `in ${Math.round(minutes / 1440)}d`;
}

export function money(value, estimate) {
  const amount = value || 0;
  const text = amount >= 100 ? `$${amount.toFixed(0)}` : amount >= 0.995 ? `$${amount.toFixed(2)}` : amount > 0 ? `$${amount.toFixed(3)}` : "$0";
  return estimate ? `~${text}` : text;
}

export function tokens(t) {
  if (!t) return "";
  const total = (t.input || 0) + (t.output || 0) + (t.cached_input || 0);
  if (total >= 1e6) return `${(total / 1e6).toFixed(1)}M tok`;
  if (total >= 1e3) return `${Math.round(total / 1e3)}k tok`;
  return `${total} tok`;
}

// ------------------------------------------------------------- remembered preferences

export function remember(key, value) {
  try {
    localStorage.setItem(`am.${key}`, JSON.stringify(value));
  } catch {
    // Storage can be unavailable; the preference just won't persist.
  }
}

export function recall(key, fallback) {
  try {
    const raw = localStorage.getItem(`am.${key}`);
    return raw === null ? fallback : JSON.parse(raw);
  } catch {
    return fallback;
  }
}
