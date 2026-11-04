// Settings, Memory and the turn view.
import { html, useEffect, useLayoutEffect, useRef, useState } from "./lib.js";
import {
  state, api, attempt, agentPath, clock, elapsed, money, navigate, refreshCurrent, setArchived, timeAgo, toast, tokens, useTick,
} from "./store.js";
import { Markdown } from "./md.js";
import { Icon, TextField } from "./ui.js";
import { LEVEL_PILL, STATE_LEVEL } from "./health.js";
import { RunModal } from "./terminal.js";

// ------------------------------------------------------------- settings

function BackendFields({ project, role, backends, save }) {
  const backendKey = `${role}_backend`;
  const modelKey = `${role}_model`;
  const fallback = state.overview ? state.overview.defaults[backendKey] : "claude";
  const chosen = project[backendKey] || fallback;
  const models = (backends[chosen] && backends[chosen].models) || [];
  const listId = `settings-${role}-models`;
  return html`
    <label>${role === "manager" ? "Manager" : "Workers"}</label>
    <div class="field-row" style="margin-top:0">
      <select aria-label="${role} backend" value=${project[backendKey] || ""} onChange=${(e) => save({ [backendKey]: e.target.value })}>
        <option value="">Default (${fallback})</option>
        ${Object.keys(backends).map((b) => html`<option value=${b}>${b}${backends[b].available ? "" : " (not installed)"}</option>`)}
      </select>
      <${TextField} aria-label="${role} model" list=${listId} placeholder="Default model" value=${project[modelKey] || ""}
        onCommit=${(text) => save({ [modelKey]: text.trim() })} />
      <datalist id=${listId}>${models.map((m) => html`<option value=${m} />`)}</datalist>
    </div>`;
}

function ScheduleForm({ slug, agents, onDone }) {
  const [form, setForm] = useState({ name: "", when: "1d", to: "manager", prompt: "" });
  const set = (key) => (e) => setForm({ ...form, [key]: e.target.value });
  const submit = () =>
    attempt(async () => {
      const looksLikeCron = form.when.trim().split(/\s+/).length === 5;
      await api(`/api/projects/${encodeURIComponent(slug)}/schedules`, {
        json: {
          name: form.name.trim(),
          to: form.to,
          prompt: form.prompt,
          every: looksLikeCron ? undefined : form.when.trim(),
          cron: looksLikeCron ? form.when.trim() : undefined,
        },
      });
      onDone();
      refreshCurrent();
    });
  return html`
    <div class="form-grid" style="margin-top:12px">
      <label>Name</label><input type="text" value=${form.name} onInput=${set("name")} aria-label="Schedule name" placeholder="daily-feed" />
      <label>When</label>
      <div>
        <input type="text" value=${form.when} onInput=${set("when")} aria-label="Schedule timing" />
        <div class="help">An interval such as 30m, 2h or 1d, or a cron line such as 0 9 * * 1-5.</div>
      </div>
      <label>Deliver to</label>
      <select value=${form.to} onChange=${set("to")} aria-label="Deliver to">${agents.map((a) => html`<option value=${a.name}>${a.name}</option>`)}</select>
      <label class="top">Prompt</label><textarea rows="3" value=${form.prompt} onInput=${set("prompt")} aria-label="Prompt"></textarea>
      <span></span>
      <div class="row-actions" style="margin-top:0">
        <button class="btn small primary" disabled=${!form.name.trim() || !form.prompt.trim() || !form.when.trim()} onClick=${submit}>Add schedule</button>
        <button class="btn small" onClick=${onDone}>Cancel</button>
      </div>
    </div>`;
}

function ArchiveDialog({ name, running, onConfirm, onClose }) {
  const confirmButton = useRef(null);
  const names = running.map((a) => a.name).join(", ");
  // Before paint, so that Escape reaches the dialog however quickly it's pressed.
  useLayoutEffect(() => confirmButton.current.focus(), []);
  return html`
    <div class="modal-scrim" onMouseDown=${(e) => e.target === e.currentTarget && onClose()} onKeyDown=${(e) => e.key === "Escape" && onClose()}>
      <div class="modal small" role="dialog" aria-modal="true" aria-label="Archive ${name}" data-testid="archive-dialog">
        <div class="modal-head"><span class="grow">Archive ${name}?</span><button class="icon-btn" aria-label="Close" onClick=${onClose}><${Icon} name="close" /></button></div>
        <div class="modal-body">
          <p style="margin-top:0">Its schedules and health checks stop, and no agent starts a new turn. Messages sent to it wait, and are delivered when you un-archive it.</p>
          <p>Nothing is deleted. It moves to Archived in the sidebar, where you can still open it and read everything.</p>
          ${running.length > 0 && html`
            <p class="panel-note" style="border-radius:6px;padding:6px 10px" data-testid="archive-running">
              ${running.length === 1 ? `1 agent is running now (${names}). Its turn` : `${running.length} agents are running now (${names}). Their turns`} will finish normally.
            </p>`}
          <div class="row-actions">
            <button class="btn primary" data-testid="archive-confirm" ref=${confirmButton} onClick=${onConfirm}>Archive project</button>
            <button class="btn" onClick=${onClose}>Cancel</button>
          </div>
        </div>
      </div>
    </div>`;
}

/** Whether it's asking lives here, so it's forgotten once the project is archived, however that happened. */
function ArchiveButton({ slug, name, running }) {
  const [asking, setAsking] = useState(false);
  return html`
    <button class="btn" data-testid="archive-project" onClick=${() => setAsking(true)}>Archive project</button>
    ${asking && html`<${ArchiveDialog} name=${name} running=${running} onConfirm=${() => setArchived(slug, true)} onClose=${() => setAsking(false)} />`}`;
}

/** Asks before offloading an archived project, after measuring it. The server picks what goes: a request names only the project. */
function OffloadDialog({ slug, name, label, onClose }) {
  const [plan, setPlan] = useState(null);
  const url = `/api/projects/${encodeURIComponent(slug)}/offload`;
  const confirmButton = useRef(null);
  useEffect(() => {
    attempt(async () => setPlan(await api(url)));
  }, [slug]);
  useEffect(() => plan && confirmButton.current && confirmButton.current.focus(), [plan]);
  const confirm = () =>
    attempt(async () => {
      await api(url, { method: "POST" });
      onClose();
      refreshCurrent();
    });
  const count = (n, word) => `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;
  return html`
    <div class="modal-scrim" onMouseDown=${(e) => e.target === e.currentTarget && onClose()} onKeyDown=${(e) => e.key === "Escape" && onClose()}>
      <div class="modal small" role="dialog" aria-modal="true" aria-label="Offload ${name}" data-testid="offload-dialog">
        <div class="modal-head"><span class="grow">Offload ${name} to ${label}?</span><button class="icon-btn" aria-label="Close" onClick=${onClose}><${Icon} name="close" /></button></div>
        <div class="modal-body">
          ${!plan
            ? html`<p style="margin-top:0" class="muted"><span class="spinner"></span> Measuring the project…</p>`
            : html`
              <p style="margin-top:0" data-testid="offload-what"><b>The whole project is backed up</b> to ${label} as one archive, and the upload is checked: ${count(plan.files, "file")}, ${plan.size}. That's every chat, answer and session, the tasks, log.md, artifacts with their history, results/, workspace/ and cache/.</p>
              ${plan.session_files > 0 && html`<p data-testid="offload-sessions"><b>So are the agents' CLI sessions</b>, ${count(plan.session_files, "file")} (${plan.session_size}) from ~/.claude and ~/.codex, so each agent picks up where it left off.</p>`}
              <p data-testid="offload-stub"><b>Then only a stub stays here.</b> The project stays under Archived with a cloud mark, and you restore it to open it. Un-archiving restores it first. The local copy is removed only once the upload checks out.</p>
              <p class="help" style="margin:0">Frees about ${plan.freed_size}. The backup stays on ${label} after a restore.</p>
              <div class="row-actions">
                <button class="btn primary" data-testid="offload-confirm" ref=${confirmButton} onClick=${confirm}>Offload to ${label}</button>
                <button class="btn" onClick=${onClose}>Cancel</button>
              </div>`}
        </div>
      </div>
    </div>`;
}

/** An offload or restore that's running: its step, and how far along it is. */
export function OffloadJob({ job, label }) {
  useTick(1000);
  const pct = job.total ? Math.min(100, Math.round((100 * (job.done || 0)) / job.total)) : null;
  return html`
    <div data-testid="offload-progress" data-kind=${job.kind}>
      <div class="offload-line"><span class="spinner"></span> <span>${job.kind === "offload" ? `Offloading to ${label}` : `Restoring from ${label}`}${job.unarchive ? ", then un-archiving" : ""}… <span class="muted">${elapsed(job.started)}</span></span></div>
      <div class="help">${job.step}</div>
      <div class="offload-bar ${pct === null ? "busy" : ""}"><div style=${`width:${pct === null ? 100 : pct}%`}></div></div>
    </div>`;
}

export function restoreProject(slug) {
  return attempt(async () => {
    await api(`/api/projects/${encodeURIComponent(slug)}/restore`, { method: "POST" });
    refreshCurrent();
  });
}

/** In an archived project's Settings, until it's offloaded. Once it is, the stub page shows instead. */
function OffloadSection({ slug, name, offload }) {
  const [asking, setAsking] = useState(false);
  const label = offload.remote_label || "Google Drive";
  return html`
    <div class="offload" data-testid="offload-section">
      ${offload.job
        ? html`<${OffloadJob} job=${offload.job} label=${label} />`
        : html`
          <p class="help" style="margin:0 0 10px">${offload.connected
            ? `To free disk space, back up the whole project to ${label} and keep only a stub here. Restoring brings everything back exactly.`
            : "To offload this project, first connect Google Drive under Storage in the home project's Settings."}</p>
          <button class="btn" data-testid="offload-project" disabled=${!offload.connected} onClick=${() => setAsking(true)}><${Icon} name="cloud" /> Offload to ${label}</button>`}
      ${offload.error && !offload.job && html`<p class="offload-error" data-testid="offload-error">${offload.error}</p>`}
      ${asking && html`<${OffloadDialog} slug=${slug} name=${name} label=${label} onClose=${() => setAsking(false)} />`}
    </div>`;
}

/** All an offloaded project shows: it's on the remote, and Restore brings it back. */
export function OffloadedPage({ slug, data }) {
  const { offload } = data;
  const info = offload.info || {};
  const label = info.label || offload.remote_label || "Google Drive";
  useTick(30000);
  return html`
    <div class="page" data-testid="offloaded-page"><div class="offloaded-card">
      <${Icon} name="cloud" class="offloaded-icon" />
      <h2>Offloaded to ${label}. Restore to open it.</h2>
      <p class="help">Backed up ${timeAgo(info.at)}: ${(info.files || 0).toLocaleString()} files, ${info.size}, with its agents' sessions. It stays archived after a restore.</p>
      ${offload.job
        ? html`<${OffloadJob} job=${offload.job} label=${label} />`
        : html`<button class="btn primary" data-testid="offloaded-restore" onClick=${() => restoreProject(slug)}>Restore</button>`}
      ${offload.error && !offload.job && html`<p class="offload-error" data-testid="offload-error">${offload.error}</p>`}
    </div></div>`;
}

function offloadWhere(entry) {
  if (entry.restored_at && !entry.local_removed) return "Restored";
  return entry.local_removed ? "Remote only" : "Local copy kept";
}

/** On the home project's Settings: where `am storage offload` sends data, and what it has sent. */
function StorageSection({ storage }) {
  const [connecting, setConnecting] = useState(null);
  const { login, entries } = storage;
  const level = login && STATE_LEVEL[login.state];
  const connect = (provider) => setConnecting({ provider, command: storage.connect[provider] });
  return html`
    <section class="section" data-testid="storage-section" data-remote=${storage.remote}>
      <h2>Storage</h2>
      <div class="form-grid">
        <label>Remote</label>
        <div data-testid="storage-remote">
          ${storage.remote
            ? html`<span class="mono">${storage.target}</span>${login && html` <a href="/health" class="pill ${LEVEL_PILL[level]}" data-testid="storage-login" title="Its login, on the Health page">${login.label}</a>`}`
            : html`<span class="muted">Not connected</span>`}
        </div>
        <label>Offloaded</label>
        <div data-testid="storage-offloaded">${entries.length ? `${entries.length} ${entries.length === 1 ? "item" : "items"}, freeing ${storage.offloaded_size} here` : html`<span class="muted">Nothing yet</span>`}</div>
      </div>
      ${storage.remote
        ? html`<p class="help" style="margin-top:10px">When the disk fills up, the home manager moves old results to the ${storage.folder} folder there with <span class="mono">am storage offload</span>, and <span class="mono">am storage restore</span> brings them back. If the login expires, Health shows it with a Fix.</p>`
        : html`
          <p class="help" style="margin:10px 0">When the disk fills up, the home manager moves old results to a storage remote and leaves a stub behind. Connecting opens a terminal and your browser, where you sign in to Google and allow access. rclone can see only the files it creates.</p>
          <div class="row-actions" style="margin-top:0">
            <button class="btn primary" data-testid="connect-drive" disabled=${!storage.rclone} title=${`Runs ${storage.connect["google-drive"]}`} onClick=${() => connect("google-drive")}>Connect Google Drive</button>
          </div>
          ${!storage.rclone && html`<p class="help" data-testid="storage-no-rclone">rclone isn't installed. On a Mac, run <span class="mono">brew install rclone</span>, then reload this page.</p>`}
          <p class="help">For any other rclone remote, set <span class="mono">rclone_remote</span> under <span class="mono">[storage]</span> in <span class="mono">${storage.config}</span>.</p>`}
      ${entries.length > 0 && html`
        <div class="table-scroll" style="margin-top:10px"><table class="table" data-testid="storage-entries">
          <thead><tr><th>Path</th><th>Size</th><th>Offloaded</th><th>Now</th></tr></thead>
          <tbody>
            ${entries.map((e) => html`
              <tr key=${e.path}>
                <td><div class="mono">${e.path}</div>${e.note && html`<div class="help">${e.note}</div>`}</td>
                <td>${e.size}</td>
                <td>${timeAgo(e.offloaded_at)}</td>
                <td>${offloadWhere(e)}</td>
              </tr>`)}
          </tbody>
        </table></div>`}
      ${connecting && html`<${RunModal} connect=${connecting} onClose=${() => { setConnecting(null); refreshCurrent(); }} />`}
    </section>`;
}

export function SettingsPage({ slug }) {
  const data = state.projects[slug];
  const [adding, setAdding] = useState(false);
  useTick(30000);
  if (!data || !state.overview) return html`<div class="page"></div>`;
  const { project, schedules } = data;
  const backends = state.overview.backends;
  const url = `/api/projects/${encodeURIComponent(slug)}`;
  const save = (fields) =>
    attempt(async () => {
      await api(url, { method: "PATCH", json: fields });
      toast("Saved");
      refreshCurrent();
    });
  const scheduleAction = (name, method, suffix = "") =>
    attempt(async () => {
      await api(`${url}/schedules/${encodeURIComponent(name)}${suffix}`, { method });
      refreshCurrent();
    });
  const toggle = (entry) =>
    attempt(async () => {
      await api(`${url}/schedules`, { json: { name: entry.name, enabled: !entry.enabled } });
      refreshCurrent();
    });
  const agents = data.agents.filter((a) => a.project === slug && !a.retired);
  const totalCost = data.agents.filter((a) => a.project === slug).reduce((sum, a) => sum + (a.cost_usd || 0), 0);
  const archived = !!project.archived_at;
  const running = agents.filter((a) => a.main_turn || a.side_turns.length);
  return html`
    <div class="page" data-testid="settings"><div class="page-inner">
      <section class="section">
        <h2>Project</h2>
        <div class="form-grid">
          <label>Name</label>
          <${TextField} value=${project.name} aria-label="Project name" onCommit=${(text) => text.trim() && save({ name: text.trim() })} />
          <label class="top">Description</label>
          <${TextField} multiline rows="2" value=${project.description} aria-label="Description" onCommit=${(text) => save({ description: text })} />
          <label>Status line</label>
          <${TextField} value=${project.status} aria-label="Status line" placeholder="The manager keeps this current" onCommit=${(text) => save({ status: text })} />
        </div>
        <p class="help" style="margin-top:10px">The manager owns these settings and may change them as the work evolves.</p>
      </section>

      <section class="section">
        <h2>Backends</h2>
        <div class="form-grid">
          <${BackendFields} project=${project} role="manager" backends=${backends} save=${save} />
          <${BackendFields} project=${project} role="worker" backends=${backends} save=${save} />
        </div>
        <p class="help" style="margin-top:10px">These are defaults. Each agent can override them from its row in the Agents card, and they take effect on its next turn.</p>
      </section>

      <section class="section">
        <h2>Schedules</h2>
        <div class="form-grid" style="margin-bottom:12px">
          <label>Health check</label>
          <div>
            <${TextField} value=${project.health_every || ""} aria-label="Health check frequency" placeholder=${`Default: every ${state.overview.defaults.health_every}`} onCommit=${(text) => save({ health_every: text.trim() })} />
            <div class="help">How often the manager looks for stuck work, such as 30m or 2h, or off. Skipped while nothing is in progress.${archived ? " Paused while archived." : ""}${data.health_last ? ` Last checked ${timeAgo(data.health_last)}.` : ""}</div>
          </div>
        </div>
        ${schedules.length > 0 && html`
          <div class="table-scroll"><table class="table" data-testid="schedules">
            <thead><tr><th>Name</th><th>When</th><th>To</th><th>Next</th><th></th></tr></thead>
            <tbody>
              ${schedules.map((entry) => html`
                <tr key=${entry.name}>
                  <td><div>${entry.name}</div><div class="help">${entry.prompt}</div></td>
                  <td class="mono">${entry.every || entry.cron}</td>
                  <td>${entry.to}</td>
                  <td>${entry.enabled === false ? html`<span class="muted">Off</span>` : archived ? html`<span class="muted">Paused</span>` : entry.next ? clock(entry.next) : ""}</td>
                  <td style="white-space:nowrap">
                    <button class="btn small" disabled=${archived} title=${archived ? "Paused while archived" : ""} onClick=${() => scheduleAction(entry.name, "POST", "/run")}>Run now</button>${" "}
                    <button class="btn small" onClick=${() => toggle(entry)}>${entry.enabled === false ? "Turn on" : "Turn off"}</button>${" "}
                    <button class="icon-btn" title="Delete" aria-label="Delete ${entry.name}" onClick=${() => scheduleAction(entry.name, "DELETE")}><${Icon} name="close" /></button>
                  </td>
                </tr>`)}
            </tbody>
          </table></div>`}
        ${adding
          ? html`<${ScheduleForm} slug=${slug} agents=${agents} onDone=${() => setAdding(false)} />`
          : html`<button class="btn small" onClick=${() => setAdding(true)} data-testid="add-schedule"><${Icon} name="plus" /> Add schedule</button>`}
      </section>

      ${data.storage && html`<${StorageSection} storage=${data.storage} />`}

      <section class="section">
        <h2>Details</h2>
        <div class="form-grid">
          <label>Directory</label><div class="mono" style="word-break:break-all">${data.paths.project}</div>
          <label>Spent so far</label><div>${money(totalCost)}</div>
        </div>
      </section>

      ${slug !== "home" && html`
        <section class="section" data-testid="archive-section">
          <h2>Archive</h2>
          ${archived
            ? html`
              <p class="help" style="margin:0 0 10px">Archived ${timeAgo(project.archived_at)}. Nothing new runs, and messages wait. Un-archiving delivers them. Schedules resume from then, and skip the runs they missed.</p>
              <button class="btn" data-testid="settings-unarchive" disabled=${!!(data.offload && data.offload.job)} onClick=${() => setArchived(slug, false)}>Un-archive project</button>
              ${data.offload && html`<${OffloadSection} slug=${slug} name=${project.name} offload=${data.offload} />`}`
            : html`
              <p class="help" style="margin:0 0 10px">Set this project aside when it's finished or on hold. Nothing runs while it's archived and nothing is deleted, and you can un-archive it at any time.</p>
              <${ArchiveButton} slug=${slug} name=${project.name} running=${running} />`}
        </section>`}
    </div></div>`;
}

// ------------------------------------------------------------- memory

export function MemoryPage() {
  const [entries, setEntries] = useState(null);
  const [selected, setSelected] = useState(null);
  const [entry, setEntry] = useState(null);
  const [editing, setEditing] = useState(null);
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");

  const loadList = async () => {
    const data = await attempt(() => api("/api/memory"));
    if (data) setEntries(data.entries);
  };
  const loadEntry = async (name, rev = null) => {
    const query = rev ? `?rev=${encodeURIComponent(rev)}` : "";
    const data = await attempt(() => api(`/api/memory/${encodeURIComponent(name)}${query}`));
    if (data) {
      setEntry(data);
      setSelected(name);
      setEditing(null);
      setCreating(false);
    }
  };
  useEffect(() => {
    loadList();
  }, []);

  const save = async () => {
    const done = await attempt(() => api(`/api/memory/${encodeURIComponent(selected)}`, { method: "PUT", json: { text: editing } }));
    if (done) {
      toast("Saved");
      loadEntry(selected);
      loadList();
    }
  };
  const create = async () => {
    const done = await attempt(() => api("/api/memory", { json: { name: newName.trim(), text: editing } }));
    if (done) {
      const name = newName.trim();
      setNewName("");
      loadList();
      loadEntry(name);
    }
  };
  const remove = async () => {
    if (!confirm(`Delete the memory "${selected}"? Its history stays in git.`)) return;
    const done = await attempt(() => api(`/api/memory/${encodeURIComponent(selected)}`, { method: "DELETE" }));
    if (done) {
      setSelected(null);
      setEntry(null);
      loadList();
    }
  };

  return html`
    <div class="memory-layout" data-testid="memory">
      <div class="memory-list">
        <button class="btn small" style="width:100%;margin-bottom:8px" data-testid="new-memory" onClick=${() => { setCreating(true); setSelected(null); setEntry(null); setEditing(""); }}><${Icon} name="plus" /> New memory</button>
        ${entries && entries.length === 0 && html`<div class="card-empty">Facts about you and how to get things done. Agents read and add to these.</div>`}
        ${(entries || []).map((e) => html`
          <button class="row-btn ${selected === e.name ? "selected" : ""}" style="border-radius:6px;padding:6px 8px" key=${e.name} onClick=${() => loadEntry(e.name)}>
            <span class="row-main"><div class="row-title">${e.name}</div><div class="row-sub">${e.summary}</div></span>
          </button>`)}
      </div>
      <div class="memory-entry">
        ${creating && html`
          <div class="page-inner">
            <h2>New memory</h2>
            <input type="text" placeholder="Name, such as prefers-short-answers" aria-label="Memory name" value=${newName} onInput=${(e) => setNewName(e.target.value)} />
            <textarea value=${editing} aria-label="Memory text" onInput=${(e) => setEditing(e.target.value)}></textarea>
            <div class="row-actions"><button class="btn primary" disabled=${!newName.trim() || !editing.trim()} onClick=${create}>Save</button><button class="btn" onClick=${() => setCreating(false)}>Cancel</button></div>
          </div>`}
        ${!creating && entry && html`
          <div class="page-inner">
            <div class="row-actions" style="margin-top:0">
              <h2 class="grow">${entry.name}${entry.rev ? html` <span class="chip">${entry.rev}</span>` : ""}</h2>
              ${editing === null && !entry.rev && html`<button class="btn small" onClick=${() => setEditing(entry.text)}>Edit</button><button class="btn small danger" onClick=${remove}>Delete</button>`}
              ${entry.rev && html`<button class="btn small" onClick=${() => loadEntry(entry.name)}>Back to current</button>`}
            </div>
            ${editing !== null
              ? html`
                <textarea value=${editing} aria-label="Memory text" onInput=${(e) => setEditing(e.target.value)}></textarea>
                <div class="row-actions"><button class="btn primary" onClick=${save}>Save</button><button class="btn" onClick=${() => setEditing(null)}>Cancel</button></div>`
              : html`<div class="section"><${Markdown} text=${entry.text} /></div>`}
            ${entry.log && entry.log.length > 0 && html`
              <div>
                <span class="field-label">History</span>
                ${entry.log.map((v) => html`
                  <div class="turn-link" key=${v.rev}>
                    <button class="link-btn mono" onClick=${() => loadEntry(entry.name, v.rev)}>${v.rev}</button>
                    <span class="muted">${v.subject} · ${v.author} · ${timeAgo(v.ts)}</span>
                  </div>`)}
              </div>`}
          </div>`}
        ${!creating && !entry && html`<div class="empty"><h2>Shared memory</h2><p>Every agent can read these, across all projects. Choose one, or add a new one.</p></div>`}
      </div>
    </div>`;
}

// ------------------------------------------------------------- turn view

function TurnEvent({ event }) {
  switch (event.k) {
    case "text":
      return html`<${Markdown} text=${event.text} />`;
    case "tool":
      return html`<div class="event-tool"><b>${event.name}</b> ${event.summary}</div>`;
    case "output":
      return event.text ? html`<div class="event-output ${event.error ? "error" : ""}">${event.text}</div>` : null;
    case "result":
      return html`<div class="event-result ${event.error ? "error" : ""}"><${Markdown} text=${event.text} /></div>`;
    case "init":
      return html`<div class="faint" style="font-size:11px">Started on ${event.model}${event.mode ? `, permission mode ${event.mode}` : ""}</div>`;
    default:
      return null;
  }
}

export function TurnPage({ slug, agent, turn }) {
  const [data, setData] = useState(null);
  const [events, setEvents] = useState([]);
  const [error, setError] = useState(null);
  const offset = useRef(0);
  useTick(2000);

  useEffect(() => {
    let live = true;
    let timer = null;
    offset.current = 0;
    setEvents([]);
    setData(null);
    const poll = async () => {
      try {
        const result = await api(`/api/projects/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}/turns/${encodeURIComponent(turn)}?offset=${offset.current}`);
        if (!live) return;
        offset.current = result.offset;
        setData((previous) => ({ ...previous, ...result, prompt: result.prompt ?? (previous && previous.prompt) }));
        if (result.events.length) setEvents((existing) => [...existing, ...result.events]);
        if (result.running) timer = setTimeout(poll, 1000);
      } catch (e) {
        if (live) setError(e.message);
      }
    };
    poll();
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [slug, agent, turn]);

  const stop = () =>
    attempt(async () => {
      await api(`/api/projects/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}/interrupt`, { json: {} });
      toast("Stopping the turn");
    });

  const meta = data && data.meta;
  const result = data && data.result;
  return html`
    <div class="page" data-testid="turn-view"><div class="page-inner">
      <div class="row-actions" style="margin-top:0">
        <button class="btn small" onClick=${() => navigate(agentPath(slug, agent))}><${Icon} name="left" /> Back to chat</button>
        <span class="grow"></span>
        ${data && data.running && meta && meta.kind === "main" && html`<button class="btn small danger" onClick=${stop} data-testid="turn-stop"><${Icon} name="stop" /> Stop</button>`}
      </div>
      ${error && html`<div class="section">${error}</div>`}
      ${meta && html`
        <div class="section">
          <h2 class="mono">${meta.id}</h2>
          <div class="muted">
            ${meta.kind === "side" ? "Side turn" : "Main turn"} of ${agent} · ${meta.backend}${meta.model ? ` · ${meta.model}` : ""}
            · started ${clock(meta.created)}
            ${data.running ? html` · <span data-testid="turn-running">running for ${elapsed(meta.created)}</span>` : ""}
            ${result ? ` · ${result.state}` : ""}
            ${result && result.cost_usd > 0 ? ` · ${money(result.cost_usd, result.cost_is_estimate)}` : ""}
            ${result && result.tokens ? ` · ${tokens(result.tokens)}` : ""}
          </div>
          ${(meta.triggers || []).map((t) => html`<div class="help" key=${t.id}><b>${t.kind}</b> from ${t.from}: ${t.excerpt}</div>`)}
          ${data.prompt && html`<details class="prompt" style="margin-top:8px"><summary>Prompt</summary><pre>${data.prompt}</pre></details>`}
        </div>`}
      <div class="turn-events">
        ${events.map((event, i) => html`<${TurnEvent} key=${i} event=${event} />`)}
        ${data && data.running && html`<div class="working-row"><span class="spinner"></span> Working…</div>`}
        ${result && result.error && result.state !== "interrupted" && html`<div class="event-result error" data-testid="turn-error">${result.error}</div>`}
      </div>
    </div></div>`;
}
