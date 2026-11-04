// The root: sidebar, page layout and routing.
import { html, render, useEffect, useState } from "./lib.js";
import {
  state, api, attempt, connectEvents, navigate, projectPath, recall, refreshCurrent, remember, setArchived, toast, useStore, agentPath,
} from "./store.js";
import { useMedia } from "./store.js";
import { ChatView } from "./chat.js";
import { Rail } from "./rail.js";
import { ArtifactPanel, installArtifactLinks } from "./artifact.js";
import { installLightbox } from "./lightbox.js";
import { TerminalView } from "./terminal.js";
import { MemoryPage, OffloadedPage, SettingsPage, TurnPage } from "./pages.js";
import { HealthDot, HealthIndicator, HealthPage } from "./health.js";
import { AttentionCount, Chevron, Icon, Logo } from "./ui.js";

function SideBadges({ summary }) {
  if (!summary) return null;
  const hasAny = summary.updated > 0 || summary.unread > 0 || summary.attention.open > 0;
  if (!hasAny) return null;
  return html`
    <div class="side-badges">
      ${summary.attention.open > 0 && html`<${AttentionCount} counts=${summary.attention} />`}
      ${summary.unread > 0 && html`<span class="count unread" title="Unread answers" data-testid="unread-count">${summary.unread} unread</span>`}
    </div>`;
}

function ProjectLink({ summary, active }) {
  const status = summary.status || (summary.running ? "Working" : summary.description);
  return html`
    <a class="side-link ${active ? "active" : ""}" href=${projectPath(summary.slug)} data-testid="sidebar-project" data-slug=${summary.slug}>
      <span class="side-label">
        <span class="side-name">
          <span>${summary.name}</span>
          ${summary.running > 0 && html`<span class="dot running" title="Working"></span>`}
          ${summary.updated > 0 && html`<span class="dot update" title="An answer was updated" data-testid="update-dot"></span>`}
        </span>
        ${status && html`<div class="side-status">${status}</div>`}
        <${SideBadges} summary=${summary} />
      </span>
    </a>`;
}

/** Archived projects are out of the way, so a row shows no attention or unread counts. */
function ArchivedProjects({ projects = [], activeSlug }) {
  const [open, setOpen] = useState(() => recall("sidebar.archived", false));
  if (!projects.length) return null;
  const toggle = () => {
    remember("sidebar.archived", !open);
    setOpen(!open);
  };
  return html`
    <div class="side-archived" data-testid="archived-section">
      <button class="side-heading side-toggle" aria-expanded=${open} data-testid="archived-toggle" onClick=${toggle}>
        <${Chevron} open=${open} /><span>Archived (${projects.length})</span>
      </button>
      ${open && projects.map((p) => html`
        <div class="side-archived-row" key=${p.slug} data-testid="archived-project" data-archived-slug=${p.slug}>
          <a class="side-link grow ${activeSlug === p.slug ? "active" : ""}" href=${projectPath(p.slug)}>
            <span class="side-label">
              <span class="side-name">
                <span>${p.name}</span>
                ${p.offloaded && html`<span class="side-cloud" title="Offloaded: restore it to open it" data-testid="archived-offloaded"><${Icon} name="cloud" /></span>`}
                ${p.running > 0 && html`<span class="dot running" title="A turn is finishing"></span>`}
              </span>
              ${p.held > 0 && html`<div class="side-status">${p.held} message${p.held === 1 ? "" : "s"} held</div>`}
            </span>
          </a>
          <button class="icon-btn" title="Un-archive" aria-label="Un-archive ${p.name}" data-testid="archived-unarchive" onClick=${() => setArchived(p.slug, false)}><${Icon} name="unarchive" /></button>
        </div>`)}
    </div>`;
}

function NewProject({ onClose }) {
  const [name, setName] = useState("");
  const submit = () =>
    attempt(async () => {
      const project = await api("/api/projects", { json: { name: name.trim() } });
      onClose();
      navigate(projectPath(project.slug));
    });
  return html`
    <div class="modal-scrim" onMouseDown=${(e) => e.target === e.currentTarget && onClose()}>
      <div class="modal small" role="dialog" aria-label="New project">
        <div class="modal-head"><span class="grow">New project</span><button class="icon-btn" aria-label="Close" onClick=${onClose}><${Icon} name="close" /></button></div>
        <div class="modal-body">
          <input type="text" placeholder="Name" aria-label="Project name" value=${name} ref=${(el) => el && !name && el.focus()}
            onInput=${(e) => setName(e.target.value)} onKeyDown=${(e) => { if (e.key === "Enter" && name.trim()) submit(); if (e.key === "Escape") onClose(); }} />
          <p class="help">A name is all it takes. The project's manager waits for your first message to learn what it's for.</p>
          <div class="row-actions"><button class="btn primary" disabled=${!name.trim()} onClick=${submit} data-testid="create-project">Create</button></div>
        </div>
      </div>
    </div>`;
}

function Sidebar({ open, onNavigate }) {
  const { overview, route, connected } = state;
  const [creating, setCreating] = useState(false);
  return html`
    <nav class="sidebar ${open ? "open" : ""}" aria-label="Main" onClick=${(e) => e.target.closest && e.target.closest("a") && onNavigate()} data-testid="sidebar">
      <div class="brand"><${Logo} />Agent Mgr${!connected && html`<span class="disconnected" data-testid="disconnected">Reconnecting…</span>`}</div>
      <div class="side-scroll">
        <a class="side-link ${route.view === "project" && route.slug === "home" ? "active" : ""}" href="/" data-testid="sidebar-home">
          <${Icon} name="home" />
          <span class="side-label">
            <span class="side-name"><span>Home</span>${overview && overview.home && overview.home.updated > 0 && html`<span class="dot update" data-testid="update-dot"></span>`}</span>
            <${SideBadges} summary=${overview && overview.home} />
          </span>
        </a>
        <a class="side-link ${route.view === "memory" ? "active" : ""}" href="/memory" data-testid="sidebar-memory"><${Icon} name="memory" /><span class="side-label"><span class="side-name"><span>Memory</span></span></span></a>
        <a class="side-link ${route.view === "terminal" ? "active" : ""}" href="/terminal" data-testid="sidebar-terminal"><${Icon} name="terminal" /><span class="side-label"><span class="side-name"><span>Terminal</span></span></span></a>
        <div class="side-heading"><span>Projects</span><button class="icon-btn" title="New project" aria-label="New project" data-testid="new-project" onClick=${() => setCreating(true)}><${Icon} name="plus" /></button></div>
        ${overview && overview.projects.length === 0 && html`<div class="card-empty" style="padding:2px 8px">Create one here, or ask the home manager.</div>`}
        ${overview && overview.projects.map((p) => html`<${ProjectLink} key=${p.slug} summary=${p} active=${route.slug === p.slug} />`)}
        ${overview && html`<${ArchivedProjects} projects=${overview.archived} activeSlug=${route.slug} />`}
      </div>
      <${HealthIndicator} />
      ${creating && html`<${NewProject} onClose=${() => setCreating(false)} />`}
    </nav>`;
}

function ProjectPage({ route, onMenu }) {
  const { slug, tab, agent, turn } = route;
  const [railOpen, setRailOpen] = useState(false);
  const narrow = useMedia("(max-width: 1100px)");
  const docked = !!state.panel && state.panel.slug === slug && state.panel.docked;
  const data = state.projects[slug];
  const isHome = slug === "home";
  const summary = data && data.summary;
  const managerUnread = data && data.unread.agents.manager;
  const hasUpdate = managerUnread && managerUnread.updated.length > 0;
  const base = isHome ? "/p/home" : `/p/${encodeURIComponent(slug)}`;
  const name = isHome ? "Home" : (data && data.project.name) || slug;
  const known = !state.overview || isHome || [...state.overview.projects, ...(state.overview.archived || [])].some((p) => p.slug === slug);
  const missing = !known && !data;

  useEffect(() => {
    setRailOpen(false);
  }, [slug, agent]);

  // The overlay rail comes out over the right edge, which is where a docked panel has just gone.
  useEffect(() => {
    if (docked) setRailOpen(false);
  }, [docked]);

  if (missing) {
    return html`<div class="main"><div class="empty"><h2>No project named ${slug}</h2><p><a href="/">Go home</a></p></div></div>`;
  }
  // Wait to learn the project exists before asking for its chat.
  if (!state.overview) return html`<div class="main"></div>`;
  if (data && data.project.offloaded) {
    return html`
      <div class="main" data-testid="project-page">
        <header class="page-header">
          <button class="icon-btn menu-btn" aria-label="Menu" onClick=${onMenu}><${Icon} name="menu" /></button>
          <${HealthDot} />
          <div class="page-title" data-testid="page-title">${name}</div>
        </header>
        <div class="content"><${OffloadedPage} key=${slug} slug=${slug} data=${data} /></div>
      </div>`;
  }
  const showRail = tab === "chat";
  return html`
    <div class="main ${narrow || docked ? "rail-collapsed" : ""}" data-testid="project-page">
      <header class="page-header">
        <button class="icon-btn menu-btn" aria-label="Menu" onClick=${onMenu}><${Icon} name="menu" /></button>
        <${HealthDot} />
        <div class="page-title" data-testid="page-title">
          ${agent !== "manager" ? html`<a class="crumb" href=${projectPath(slug)}>${name}</a> <span class="faint">/</span> ${agent}` : name}
        </div>
        <div class="tabs" role="tablist">
          <a class="tab ${tab === "chat" ? "active" : ""}" role="tab" aria-selected=${tab === "chat"} href=${agentPath(slug, "manager")} data-testid="tab-chat">
            Chat
            ${hasUpdate && html`<span class="dot update" title="An answer was updated" data-testid="chat-tab-dot"></span>`}
            ${!isHome && summary && summary.attention.open > 0 && html`<${AttentionCount} counts=${data.attention_counts} />`}
            ${isHome && data && data.attention_counts.open > 0 && html`<${AttentionCount} counts=${data.attention_counts} />`}
          </a>
          ${!isHome && html`<a class="tab ${tab === "terminal" ? "active" : ""}" role="tab" aria-selected=${tab === "terminal"} href="${base}/terminal" data-testid="tab-terminal">Terminal</a>`}
          <a class="tab ${tab === "settings" ? "active" : ""}" role="tab" aria-selected=${tab === "settings"} href="${base}/settings" data-testid="tab-settings">Settings</a>
        </div>
        ${showRail && html`<button class="icon-btn rail-toggle" aria-label="Show cards" title="Show cards" onClick=${() => setRailOpen(!railOpen)}><${Icon} name="rail" /></button>`}
      </header>
      ${data && data.project.archived_at && html`
        <div class="panel-note" data-testid="archived-banner">
          This project is archived. Nothing new runs, and messages wait until you <button class="link-btn" data-testid="banner-unarchive" onClick=${() => setArchived(slug, false)}>un-archive it</button>.
        </div>`}
      <div class="content">
        ${tab === "chat" && !turn && html`<${ChatView} key=${`${slug}/${agent}`} slug=${slug} agent=${agent} />`}
        ${tab === "chat" && turn && html`<${TurnPage} key=${turn} slug=${slug} agent=${agent} turn=${turn} />`}
        ${tab === "terminal" && html`<${TerminalView} key=${slug} path=${`/api/projects/${encodeURIComponent(slug)}/terminal`} label="Terminal for ${name}" />`}
        ${tab === "settings" && html`<${SettingsPage} key=${slug} slug=${slug} />`}
        ${state.panel && state.panel.slug === slug && html`<${ArtifactPanel} key=${`panel:${slug}`} />`}
        ${showRail && html`<${Rail} slug=${slug} open=${railOpen} />`}
      </div>
    </div>`;
}

function SimplePage({ title, onMenu, children }) {
  return html`
    <div class="main">
      <header class="page-header">
        <button class="icon-btn menu-btn" aria-label="Menu" onClick=${onMenu}><${Icon} name="menu" /></button>
        <${HealthDot} />
        <div class="page-title">${title}</div>
      </header>
      <div class="content">${children}</div>
    </div>`;
}

function App() {
  useStore();
  const [menuOpen, setMenuOpen] = useState(false);
  const { route, toasts, error, overview } = state;
  const openMenu = () => setMenuOpen(!menuOpen);
  return html`
    <div class="layout">
      <${Sidebar} open=${menuOpen} onNavigate=${() => setMenuOpen(false)} />
      ${!overview && error && html`<div class="main"><div class="empty"><h2>Can't reach the daemon</h2><p>${error}</p></div></div>`}
      ${route.view === "project" && html`<${ProjectPage} route=${route} onMenu=${openMenu} />`}
      ${route.view === "memory" && html`<${SimplePage} title="Memory" onMenu=${openMenu}><${MemoryPage} /><//>`}
      ${route.view === "health" && html`<${SimplePage} title="Health" onMenu=${openMenu}><${HealthPage} /><//>`}
      ${route.view === "terminal" && html`<${SimplePage} title="Terminal" onMenu=${openMenu}><${TerminalView} key="home" path="/api/terminal" label="Terminal" /><//>`}
    </div>
    <div class="toasts" role="status">${toasts.map((t) => html`<div class="toast ${t.kind}" key=${t.id}>${t.text}</div>`)}</div>`;
}

// Links inside the app navigate without a page load.
document.addEventListener("click", (e) => {
  const link = e.target && e.target.closest ? e.target.closest("a[href]") : null;
  if (!link || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  if (link.target === "_blank" || link.classList.contains("artifact-ref")) return;
  const href = link.getAttribute("href");
  if (!href || !href.startsWith("/") || href.startsWith("/artifacts/") || href.startsWith("/api/") || href.startsWith("/a/")) return;
  e.preventDefault();
  navigate(href);
});

installArtifactLinks();
installLightbox();
render(html`<${App} />`, document.getElementById("app"));
refreshCurrent();
connectEvents();
export { toast };
