// The health indicator, always on screen, and the Health page it opens.
import { html, useState } from "./lib.js";
import { state, api, attempt, projectPath, refreshCurrent, timeAgo, timeUntil, update, useTick } from "./store.js";
import { RunModal } from "./terminal.js";

export const STATE_LEVEL = { ok: "ok", unknown: "warn", unchecked: "warn", expired: "bad", missing: "bad" };
export const LEVEL_PILL = { ok: "ok", warn: "queued", bad: "failed" };

function summaryOf() {
  return (state.overview && state.overview.health) || { level: "warn", label: "Checking health…" };
}

/** In the sidebar, on desktop and in the phone's menu. */
export function HealthIndicator() {
  const { level, label } = summaryOf();
  return html`
    <div class="side-foot">
      <a class="side-link ${state.route.view === "health" ? "active" : ""}" href="/health" title=${label} data-testid="health-indicator" data-level=${level}>
        <span class="dot ${level}" style="margin:6px 4px 0"></span>
        <span class="side-label"><span class="side-name"><span>${label}</span></span></span>
      </a>
    </div>`;
}

/** In the page header on a phone, where the sidebar is out of sight and only a dot fits. */
export function HealthDot() {
  const { level, label } = summaryOf();
  return html`
    <a class="icon-btn health-dot" href="/health" title=${label} aria-label="Health: ${label}" data-testid="health-dot" data-level=${level}>
      <span class="dot ${level}"></span>
    </a>`;
}

function LoginRow({ login, onFix }) {
  const level = STATE_LEVEL[login.state];
  return html`
    <div class="health-row" data-testid="login-row" data-login=${login.name} data-state=${login.state}>
      <span class="dot ${level}"></span>
      <div class="row-main">
        <div class="health-head">
          <b>${login.name}</b>
          <span class="pill ${LEVEL_PILL[level]}" data-testid="login-state">${login.label}</span>
          ${login.checking && html`<span class="spinner" title="Checking" data-testid="login-checking"></span>`}
        </div>
        ${login.detail && html`<div class="help" data-testid="login-detail">${login.detail}</div>`}
      </div>
      <span class="row-side">${login.checked ? `Checked ${timeAgo(login.checked)}` : ""}</span>
      ${login.fix && html`<button class="btn small primary" data-testid="login-fix" title=${`Runs ${login.fix}`} onClick=${() => onFix(login)}>Fix</button>`}
    </div>`;
}

function MachineRow({ row }) {
  const level = row.high ? "warn" : row.percent === null ? "idle" : "ok";
  const reading = row.percent !== null ? `${Math.round(row.percent)}%` : row.read ? "Couldn't read" : "Not read yet";
  return html`
    <div class="health-row" data-testid="machine-row" data-name=${row.name}>
      <span class="dot ${level}"></span>
      <div class="row-main">
        <div class="health-head">
          <b>${row.label}</b>
          <span>${reading}</span>
          ${row.high && html`<span class="pill queued">High</span>`}
        </div>
        <div class="help">Warns at ${row.limit}%</div>
      </div>
      <span class="row-side">${row.read ? `Read ${timeAgo(row.read)}` : ""}</span>
    </div>`;
}

function checkLine(row) {
  if (row.archived) return "Archived, so checks are paused";
  if (row.every === "off") return "Checks are off";
  const parts = [`Every ${row.every}`];
  parts.push(row.sent ? `last ran ${timeAgo(row.sent)}` : row.last ? "" : "hasn't run yet");
  // With nothing in progress a check comes due, but costs nothing and isn't sent.
  if (row.last && row.last > (row.sent || 0)) parts.push(row.sent ? `skipped ${timeAgo(row.last)}` : `last due ${timeAgo(row.last)}`);
  if (row.next) parts.push(`next ${timeUntil(row.next)}`);
  return parts.filter(Boolean).join(" · ");
}

function ProjectRow({ row }) {
  const check = row.check || {};
  const level = row.problem ? "warn" : row.archived || row.every === "off" ? "idle" : "ok";
  return html`
    <div class="health-row" data-testid="project-health" data-project=${row.project}>
      <span class="dot ${level}"></span>
      <div class="row-main">
        <div class="health-head">
          <a href=${projectPath(row.project)}>${row.name}</a>
          ${row.problem && html`<span class="pill queued" data-testid="project-problem">${row.problem}</span>`}
          ${check.state === "running" && html`<span class="spinner" title="Checking now"></span>`}
        </div>
        <div class="help">${checkLine(row)}</div>
        ${check.reply && html`<div class="help health-reply" data-testid="project-reply">${check.reply}</div>`}
        ${check.state === "queued" && html`<div class="help">The last check is waiting for the manager.</div>`}
      </div>
    </div>`;
}

export function HealthPage() {
  useTick(15000);
  const [fixing, setFixing] = useState(null);
  const health = state.health;
  if (!health) return html`<div class="page" data-testid="health-page"></div>`;
  const checkNow = () =>
    attempt(async () => {
      const data = await api("/api/health/check", { json: {} });
      update((s) => (s.health = data));
    });
  return html`
    <div class="page" data-testid="health-page"><div class="page-inner">
      <section class="section" data-testid="health-logins">
        <div class="row-actions" style="margin:0 0 10px">
          <h2 class="grow" style="margin:0">Logins</h2>
          ${health.checking && html`<span class="spinner"></span>`}
          <button class="btn small" disabled=${health.checking} onClick=${checkNow} data-testid="health-check-now">${health.checking ? "Checking…" : "Check now"}</button>
        </div>
        ${health.logins.map((login) => html`<${LoginRow} key=${login.name} login=${login} onFix=${setFixing} />`)}
        <p class="help" style="margin:10px 0 0">Checked every ${health.check_every.logins}. Agents run under these logins, so an expired one stops their turns.</p>
      </section>
      <section class="section" data-testid="health-machine">
        <h2>Machine</h2>
        ${health.machine.map((row) => html`<${MachineRow} key=${row.name} row=${row} />`)}
      </section>
      <section class="section" data-testid="health-projects">
        <h2>Project health checks</h2>
        ${health.projects.map((row) => html`<${ProjectRow} key=${row.project} row=${row} />`)}
        <p class="help" style="margin:10px 0 0">The manager looks for stuck work, and is only asked while something is in progress. Its reply is free text, so a problem it found shows only in the reply and in the attention queue. The dot turns amber if the check itself failed or the manager looks stuck.</p>
      </section>
      ${fixing && html`<${RunModal} login=${fixing} onClose=${() => { setFixing(null); refreshCurrent(); }} />`}
    </div></div>`;
}
