// The rail of cards beside the chat. Rows expand in place. On the home page it spans
// every project, except Artifacts, which shows the home project's own.
import { html, useEffect, useState } from "./lib.js";
import {
  state, api, attempt, agentPath, markAttentionSeen, projectPath, refreshCurrent, timeAgo, money, tokens, toast, toggleArtifact, turnPath, useTick, elapsed, navigate,
} from "./store.js";
import { Markdown } from "./md.js";
import { Icon, Chevron, AttentionCount, ARTIFACT_ICONS, TextField } from "./ui.js";
import { RunModal } from "./terminal.js";

const ARTIFACTS_SHOWN = 6;
const TASK_LABEL = { todo: "To do", doing: "In progress", blocked: "Blocked", done: "Done" };

function Card({ title, count, children, testid }) {
  return html`
    <section class="card" data-testid=${testid}>
      <div class="card-head"><span class="grow">${title}</span>${count}</div>
      <div class="card-body">${children}</div>
    </section>`;
}

function ProjectTag({ item, scopeAll }) {
  if (!scopeAll) return null;
  return html`<div class="project-tag">${item.project_name}</div>`;
}

function AttentionRow({ item, scopeAll }) {
  const [open, setOpen] = useState(false);
  const [reply, setReply] = useState("");
  const [running, setRunning] = useState(false);
  const seen = () => markAttentionSeen(item);
  const resolve = () =>
    attempt(async () => {
      await api(`/api/projects/${encodeURIComponent(item.project)}/attention/${item.id}/resolve`, { method: "POST" });
      refreshCurrent();
    });
  const send = () =>
    attempt(async () => {
      await api(`/api/projects/${encodeURIComponent(item.project)}/agents/manager/messages`, {
        json: { text: reply, reply_to: { id: item.id, excerpt: item.title } },
      });
      setReply("");
      toast("Sent to the manager");
      refreshCurrent();
    });
  return html`
    <div onMouseEnter=${seen} data-testid="attention-item" data-state=${item.state}>
      <button class="row-btn" aria-expanded=${open} onClick=${() => { seen(); setOpen(!open); }}>
        <${Chevron} open=${open} />
        <span class="row-main">
          <${ProjectTag} item=${item} scopeAll=${scopeAll} />
          <div class="row-title" style=${item.state === "new" ? "font-weight:600" : ""}>${item.title}</div>
        </span>
        <span class="row-side">${timeAgo(item.created)}</span>
      </button>
      ${open && html`
        <div class="row-detail">
          ${item.body && html`<${Markdown} text=${item.body} slug=${item.project} />`}
          ${item.ref && html`<div style="margin-top:5px"><a class="artifact-ref" href="#" data-artifact=${item.ref} data-project=${item.project}>${item.ref}</a></div>`}
          ${item.run && html`
            <code class="command">${item.run}</code>
            <div class="row-actions"><button class="btn small primary" data-testid="run-fix" onClick=${() => setRunning(true)}><${Icon} name="play" /> Run</button></div>`}
          <div style="margin-top:7px">
            <input
              type="text" placeholder="Reply to the manager" value=${reply} aria-label="Reply to the manager about this"
              onInput=${(e) => { setReply(e.target.value); seen(); }}
              onKeyDown=${(e) => { if (e.key === "Enter" && reply.trim()) send(); }} />
          </div>
          <div class="row-actions">
            <button class="btn small" onClick=${resolve}><${Icon} name="check" /> Resolve</button>
            ${scopeAll && html`<a href=${projectPath(item.project)} class="btn small">Open project</a>`}
          </div>
        </div>`}
      ${running && html`<${RunModal} item=${item} onClose=${() => { setRunning(false); refreshCurrent(); }} />`}
    </div>`;
}

export function AttentionCard({ project, scopeAll }) {
  const entries = project.attention;
  return html`
    <${Card} title="Needs your attention" testid="attention-card" count=${html`<${AttentionCount} counts=${project.attention_counts} />`}>
      ${entries.length === 0 && html`<div class="card-empty">Nothing right now.</div>`}
      ${entries.map((item) => html`<${AttentionRow} key=${item.id} item=${item} scopeAll=${scopeAll} />`)}
    <//>`;
}

export function ArtifactsCard({ slug, project }) {
  const [all, setAll] = useState(false);
  const entries = project.artifacts;
  const shown = all ? entries : entries.slice(0, ARTIFACTS_SHOWN);
  const panel = state.panel;
  return html`
    <${Card} title="Artifacts" testid="artifacts-card" count=${entries.length > 0 && html`<span class="count">${entries.length}</span>`}>
      ${entries.length === 0 && html`<div class="card-empty">Nothing published yet.</div>`}
      ${shown.map((a) => html`
        <button
          key=${a.path}
          class="row-btn ${panel && panel.path === a.path && panel.slug === slug ? "selected" : ""}"
          data-artifact-row=${a.path}
          data-keep-panel="1"
          onClick=${() => toggleArtifact(slug, a.path)}>
          <${Icon} name=${ARTIFACT_ICONS[a.kind] || "doc"} />
          <span class="row-main"><div class="row-title">${a.kind === "link" && a.title ? a.title : a.name}</div></span>
          <span class="row-side">${timeAgo(a.mtime)}</span>
        </button>`)}
      ${entries.length > ARTIFACTS_SHOWN && html`
        <button class="show-all" onClick=${() => setAll(!all)}>${all ? "Show fewer" : `Show all ${entries.length}`}</button>`}
    <//>`;
}

function TaskRow({ task, scopeAll }) {
  const [open, setOpen] = useState(false);
  return html`
    <div>
      <button class="row-btn" aria-expanded=${open} onClick=${() => setOpen(!open)}>
        <span class="task-state ${task.status}" title=${TASK_LABEL[task.status]}></span>
        <span class="row-main">
          <${ProjectTag} item=${task} scopeAll=${scopeAll} />
          <div class="row-title" style=${task.status === "done" ? "text-decoration:line-through;color:var(--text-2)" : ""}>${task.title}</div>
          ${task.agent && html`<div class="row-sub">${task.agent}</div>`}
        </span>
      </button>
      ${open && html`
        <div class="row-detail">
          <div class="muted">#${task.id} · ${TASK_LABEL[task.status]} · updated ${timeAgo(task.updated)}</div>
          ${task.notes && html`<${Markdown} text=${task.notes} slug=${task.project} />`}
        </div>`}
    </div>`;
}

export function TasksCard({ project, scopeAll }) {
  const [all, setAll] = useState(false);
  const active = project.tasks.filter((t) => t.status !== "done");
  const shown = all ? project.tasks : active;
  const hidden = project.tasks.length - active.length;
  return html`
    <${Card} title="Tasks" testid="tasks-card" count=${active.length > 0 && html`<span class="count">${active.length}</span>`}>
      ${shown.length === 0 && html`<div class="card-empty">No active tasks.</div>`}
      ${shown.map((task) => html`<${TaskRow} key=${`${task.project}-${task.id}`} task=${task} scopeAll=${scopeAll} />`)}
      ${hidden > 0 && html`<button class="show-all" onClick=${() => setAll(!all)}>${all ? "Show active" : `Show all ${project.tasks.length}`}</button>`}
    <//>`;
}

const STATE_LABEL = {
  running: "Working",
  answering: "Answering",
  idle: "Idle",
  starting: "Starting",
  waiting: "Waiting to retry",
  held: "Held until un-archived",
  error: "Failed",
  retired: "Retired",
};

function AgentSettings({ agent }) {
  const overview = state.overview;
  const backends = overview ? overview.backends : {};
  const save = (fields) =>
    attempt(async () => {
      await api(`/api/projects/${encodeURIComponent(agent.project)}/agents/${encodeURIComponent(agent.name)}`, { method: "PATCH", json: fields });
      refreshCurrent();
    });
  const models = (backends[agent.backend] && backends[agent.backend].models) || [];
  const listId = `models-${agent.project}-${agent.name}`;
  return html`
    <div class="field-row">
      <label>
        <span class="field-label">Backend</span>
        <select value=${agent.own_backend || ""} aria-label="Backend" onChange=${(e) => save({ backend: e.target.value, model: "" })}>
          <option value="">Default (${agent.own_backend ? "project" : agent.backend})</option>
          ${Object.keys(backends).map((b) => html`<option value=${b}>${b}${backends[b].available ? "" : " (not installed)"}</option>`)}
        </select>
      </label>
      <label>
        <span class="field-label">Model</span>
        <${TextField} list=${listId} value=${agent.own_model || ""} placeholder=${agent.model || "default"} aria-label="Model"
          onCommit=${(text) => save({ model: text.trim() })} />
        <datalist id=${listId}>${models.map((m) => html`<option value=${m} />`)}</datalist>
      </label>
    </div>`;
}

function RecentTurns({ agent }) {
  const [turns, setTurns] = useState(null);
  useEffect(() => {
    let live = true;
    api(`/api/projects/${encodeURIComponent(agent.project)}/agents/${encodeURIComponent(agent.name)}/turns?limit=5`)
      .then((data) => live && setTurns(data.turns))
      .catch(() => live && setTurns([]));
    return () => { live = false; };
  }, [agent.turns, agent.main_turn]);
  const finished = (turns || []).filter((t) => t.state !== "running");
  if (!finished.length) return null;
  return html`
    <div class="turn-list">
      <span class="field-label">Recent turns</span>
      ${finished.map((t) => html`
        <a class="turn-link" key=${t.id} href=${turnPath(agent.project, agent.name, t.id)}>
          <span class="mono">${t.id.slice(-8)}</span>
          <span class="muted">${t.kind}${t.state !== "done" ? `, ${t.state}` : ""} · ${timeAgo(t.ended || t.created)}</span>
        </a>`)}
    </div>`;
}

function AgentRow({ agent, scopeAll }) {
  const [open, setOpen] = useState(false);
  useTick(5000);
  const chatLink = agentPath(agent.project, agent.name);
  const retire = () =>
    attempt(async () => {
      if (!confirm(`Retire ${agent.name}? A turn in progress is stopped.`)) return;
      await api(`/api/projects/${encodeURIComponent(agent.project)}/agents/${encodeURIComponent(agent.name)}/retire`, { method: "POST" });
      refreshCurrent();
    });
  const model = agent.model ? `${agent.backend} · ${agent.model}` : agent.backend;
  const unseen = agent.unread + agent.updated.length;
  return html`
    <div data-testid="agent-row" data-agent=${agent.name} data-state=${agent.state}>
      <button class="row-btn" aria-expanded=${open} onClick=${() => setOpen(!open)}>
        <span class="dot ${agent.state}" style="margin-top:6px" title=${STATE_LABEL[agent.state]}></span>
        <span class="row-main">
          <${ProjectTag} item=${agent} scopeAll=${scopeAll} />
          <div class="row-title">${agent.name}${unseen > 0 ? html` <span class="count unread">${unseen}</span>` : ""}</div>
          <div class="row-sub">${STATE_LABEL[agent.state]}${agent.main_started ? ` · ${elapsed(agent.main_started)}` : ""}${agent.queued ? ` · ${agent.queued} queued` : ""}${agent.side_turns.length ? ` · ${agent.side_turns.length} side` : ""}</div>
        </span>
        <span class="row-side" style="text-align:right">
          <div><span class="chip">${agent.backend}</span></div>
          <div title=${tokens(agent.tokens)}>${money(agent.cost_usd, agent.cost_is_estimate)}</div>
        </span>
      </button>
      ${open && html`
        <div class="row-detail">
          ${agent.purpose && html`<div class="muted" style="margin-bottom:4px">${agent.purpose}</div>`}
          ${agent.last_error && html`<div style="color:var(--red);margin-bottom:4px">${agent.last_error}</div>`}
          <div class="row-actions" style="margin-top:0">
            <a class="btn small" href=${chatLink} data-testid="agent-chat-link">Chat</a>
            ${agent.running_turns.map((t) => html`
              <a class="btn small" key=${t.id} href=${turnPath(agent.project, agent.name, t.id)}><span class="spinner"></span> ${t.kind === "side" ? "Side turn" : "Running turn"}</a>`)}
            ${agent.role === "worker" && html`<button class="btn small" onClick=${retire}>Retire</button>`}
          </div>
          <div class="muted" style="margin-top:6px">${model} · ${agent.turns} turn${agent.turns === 1 ? "" : "s"} · ${tokens(agent.tokens)}</div>
          <${AgentSettings} agent=${agent} />
          <${RecentTurns} agent=${agent} />
        </div>`}
    </div>`;
}

export function AgentsCard({ project, scopeAll }) {
  const live = project.agents.filter((a) => !a.retired);
  const busy = live.filter((a) => a.main_turn || a.side_turns.length).length;
  return html`
    <${Card} title="Agents" testid="agents-card" count=${busy > 0 && html`<span class="count">${busy} working</span>`}>
      ${live.map((agent) => html`<${AgentRow} key=${`${agent.project}/${agent.name}`} agent=${agent} scopeAll=${scopeAll} />`)}
    <//>`;
}

export function Rail({ slug, open }) {
  const project = state.projects[slug];
  if (!project) return html`<aside class="rail ${open ? "open" : ""}"></aside>`;
  const scopeAll = project.scope === "all";
  return html`
    <aside class="rail ${open ? "open" : ""}" data-testid="rail">
      <${AttentionCard} project=${project} scopeAll=${scopeAll} />
      <${ArtifactsCard} slug=${slug} project=${project} />
      <${TasksCard} project=${project} scopeAll=${scopeAll} />
      <${AgentsCard} project=${project} scopeAll=${scopeAll} />
    </aside>`;
}

export { navigate };
