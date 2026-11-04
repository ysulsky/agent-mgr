// The artifact panel: documents, HTML in a sandboxed iframe, versions, and page comments.
import { html, useEffect, useLayoutEffect, useRef, useState, useCallback } from "./lib.js";
import {
  state, api, attempt, closeArtifact, dockArtifact, onArtifactChange, openArtifact, recall, refreshCurrent, remember, timeAgo, toast, useTick,
} from "./store.js";
import { renderMarkdown } from "./md.js";
import { Icon, ARTIFACT_ICONS } from "./ui.js";

const IFRAME_SANDBOX = "allow-scripts allow-popups allow-popups-to-escape-sandbox allow-downloads allow-modals";
const MIN_WIDTH = 320;
const RESERVED_WIDTH = 120;

export function panelPrefs() {
  return { width: recall("panel.width", 620) };
}

/** Keep some of the view beside the panel: it must stay possible to click outside it. */
function clampWidth(width, container) {
  const available = container ? container.getBoundingClientRect().width : window.innerWidth - 240;
  return Math.max(MIN_WIDTH, Math.min(width, Math.max(MIN_WIDTH, available - RESERVED_WIDTH)));
}

function CommentBox({ selection, slug, info, onClose, boxRef, textRef }) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const submit = async () => {
    if (!text.trim() || sending) return;
    setSending(true);
    const rev = info.rev || (info.versions[0] && info.versions[0].rev) || "";
    const sent = await attempt(() =>
      api(`/api/projects/${encodeURIComponent(slug)}/agents/manager/messages`, {
        json: { text, comment: { artifact: info.path, rev, quote: selection.text } },
      }));
    setSending(false);
    if (sent) {
      toast("Sent to the manager");
      refreshCurrent();
      onClose();
    }
  };
  const onKeyDown = (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      submit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    }
  };
  return html`
    <div class="comment-box" ref=${boxRef} data-testid="comment-box" style=${`left:${selection.left}px;top:${selection.top}px`}
      data-keep-panel="1" onMouseDown=${(e) => e.stopPropagation()}>
      <div class="comment-quote" data-testid="comment-quote">${selection.text.length > 160 ? `${selection.text.slice(0, 159)}…` : selection.text}</div>
      <textarea ref=${textRef} rows="2" value=${text} placeholder="Comment for the manager" aria-label="Comment for the manager"
        onInput=${(e) => setText(e.target.value)} onKeyDown=${onKeyDown}></textarea>
      <div class="hint">Enter sends · Shift+Enter for a new line · Esc closes</div>
    </div>`;
}

export function ArtifactPanel() {
  useTick(15000);
  const panel = state.panel;
  const [info, setInfo] = useState(null);
  const [error, setError] = useState(null);
  const [reload, setReload] = useState(0);
  const [prefs, setPrefs] = useState(panelPrefs);
  const [selection, setSelection] = useState(null);
  const [dragging, setDragging] = useState(false);
  const bodyRef = useRef(null);
  const docRef = useRef(null);
  const frameRef = useRef(null);
  const boxRef = useRef(null);
  const textRef = useRef(null);
  const panelRef = useRef(null);
  const pendingReload = useRef(false);
  const forwardedKey = useRef(false);
  const restored = useRef(panel && panel.restored);
  const selectionRef = useRef(null);
  selectionRef.current = selection;
  if (!selection) forwardedKey.current = false;

  const slug = panel && panel.slug;
  const path = panel && panel.path;
  const rev = panel && panel.rev;
  const docked = !!panel && panel.docked;

  const load = useCallback(async () => {
    if (!panel) return;
    const params = new URLSearchParams({ path });
    if (rev) params.set("rev", rev);
    // The panel may have moved on by the time this answers: to another document or project.
    const stale = () => !state.panel || state.panel.slug !== slug || state.panel.path !== path || state.panel.rev !== rev;
    try {
      const data = await api(`/api/projects/${encodeURIComponent(slug)}/artifact?${params}`);
      if (stale()) return;
      // Coming back to a docked panel whose artifact has since gone just leaves it closed.
      if (restored.current && !data.exists && !data.rev) {
        closeArtifact();
        return;
      }
      restored.current = false;
      setInfo(data);
      setError(null);
    } catch (e) {
      if (stale()) return;
      // Only a refusal says it's gone: no answer, or a failure, is no reason to forget the panel.
      if (restored.current && e.status < 500) {
        closeArtifact();
        return;
      }
      setError(e.message);
      setInfo(null);
    }
  }, [slug, path, rev]);

  useEffect(() => {
    setSelection(null);
    load();
  }, [slug, path, rev, reload]);

  // Follow the working copy as it changes, unless a passage is selected: a reload would lose it.
  useEffect(() => {
    if (!panel) return undefined;
    return onArtifactChange((event) => {
      if (event.project !== slug || rev) return;
      if (!(event.paths || []).includes(path)) return;
      if (selectionRef.current) {
        pendingReload.current = true;
        return;
      }
      setReload((n) => n + 1);
    });
  }, [slug, path, rev, !!panel]);

  const clearSelection = useCallback(() => {
    setSelection(null);
    window.getSelection()?.removeAllRanges();
    if (frameRef.current && frameRef.current.contentWindow) {
      frameRef.current.contentWindow.postMessage({ am: "clear-selection" }, "*");
    }
    if (pendingReload.current) {
      pendingReload.current = false;
      setReload((n) => n + 1);
    }
  }, []);

  const placeBox = useCallback((text, rect, offsetLeft = 0, offsetTop = 0) => {
    const body = bodyRef.current;
    if (!body) return;
    const bounds = body.getBoundingClientRect();
    const scrollTop = body.scrollTop;
    const width = Math.min(380, bounds.width - 24);
    const left = Math.max(12, Math.min(rect.left + offsetLeft - bounds.left, bounds.width - width - 12));
    const top = rect.bottom + offsetTop - bounds.top + scrollTop + 6;
    setSelection((current) => ({ text, left, top, typing: current ? current.typing : false }));
  }, []);

  // Passages selected in a markdown or text document.
  useEffect(() => {
    if (!panel) return undefined;
    const onChange = () => {
      const doc = docRef.current;
      if (!doc) return;
      if (document.activeElement === textRef.current) return;
      const s = window.getSelection();
      const text = s ? s.toString() : "";
      if (!text.trim() || !s.rangeCount || !doc.contains(s.anchorNode)) {
        if (selectionRef.current && !selectionRef.current.frame) clearIfEmpty();
        return;
      }
      placeBox(text, s.getRangeAt(0).getBoundingClientRect());
    };
    const clearIfEmpty = () => {
      setSelection(null);
      if (pendingReload.current) {
        pendingReload.current = false;
        setReload((n) => n + 1);
      }
    };
    document.addEventListener("selectionchange", onChange);
    return () => document.removeEventListener("selectionchange", onChange);
  }, [!!panel, placeBox]);

  // The bridge inside an HTML artifact. It has an opaque origin, so its messages are
  // recognized by the window they came from, never by what they claim.
  useEffect(() => {
    if (!panel) return undefined;
    const onMessage = (event) => {
      const frame = frameRef.current;
      if (!frame || event.source !== frame.contentWindow) return;
      const data = event.data;
      if (!data || typeof data !== "object") return;
      if (data.am === "selection") {
        if (document.activeElement === textRef.current) return;
        const text = String(data.text || "").slice(0, 4000);
        if (!text.trim() || !data.rect) {
          if (selectionRef.current) {
            setSelection(null);
            if (pendingReload.current) {
              pendingReload.current = false;
              setReload((n) => n + 1);
            }
          }
          return;
        }
        const frameBounds = frame.getBoundingClientRect();
        placeBox(text, { left: Number(data.rect.left) || 0, bottom: Number(data.rect.bottom) || 0 }, frameBounds.left, frameBounds.top);
        setSelection((s) => (s ? { ...s, frame: true } : s));
      } else if (data.am === "key") {
        if (data.key === "Escape") {
          if (selectionRef.current) clearSelection();
          else closeArtifact();
          return;
        }
        // Typing moves into the box: this first key arrives from the frame, and once focus
        // has moved, the rest is typed there directly. A page can send these messages itself,
        // so one key per box is all that's accepted, or it could write the user's comment.
        const box = textRef.current;
        if (selectionRef.current && box && typeof data.key === "string" && data.key.length === 1 && !forwardedKey.current) {
          forwardedKey.current = true;
          box.focus();
          const start = box.selectionStart;
          box.value = box.value.slice(0, start) + data.key + box.value.slice(box.selectionEnd);
          box.selectionStart = box.selectionEnd = start + 1;
          box.dispatchEvent(new Event("input", { bubbles: true }));
        }
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [!!panel, placeBox, clearSelection]);

  // Typing moves into the comment box; until then it doesn't take focus, so the
  // selection can still be copied.
  useEffect(() => {
    if (!panel) return undefined;
    const onKeyDown = (e) => {
      const inField = e.target && /^(TEXTAREA|INPUT|SELECT)$/.test(e.target.tagName);
      if (e.key === "Escape") {
        if (inField && e.target !== textRef.current) return;
        if (selectionRef.current) {
          e.preventDefault();
          clearSelection();
        } else {
          e.preventDefault();
          closeArtifact();
        }
        return;
      }
      if (selectionRef.current && !inField && !e.metaKey && !e.ctrlKey && !e.altKey && e.key.length === 1 && textRef.current) {
        textRef.current.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [!!panel, clearSelection]);

  // A click outside closes the panel, unless it's docked.
  useEffect(() => {
    if (!panel || docked) return undefined;
    const onMouseDown = (e) => {
      const target = e.target;
      if (!target || !target.closest) return;
      if (target.closest(".artifact-panel") || target.closest("[data-keep-panel]") || target.closest(".artifact-ref") || target.closest(".modal-scrim")) return;
      closeArtifact();
    };
    document.addEventListener("mousedown", onMouseDown);
    return () => document.removeEventListener("mousedown", onMouseDown);
  }, [!!panel, docked]);

  // Resizing, from the left edge. The right edge stays put, at the edge of the window.
  const startDrag = (e) => {
    e.preventDefault();
    const element = panelRef.current;
    if (!element) return;
    const right = element.getBoundingClientRect().right;
    const available = element.parentElement;
    setDragging(true);
    const onMove = (move) => {
      const width = clampWidth(right - move.clientX, available);
      setPrefs((p) => ({ ...p, width }));
    };
    const onUp = () => {
      setDragging(false);
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      setPrefs((p) => {
        remember("panel.width", p.width);
        return p;
      });
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  };

  // An iframe under the pointer would swallow the drag's mouse events.
  useLayoutEffect(() => {
    document.body.classList.toggle("panel-resizing", dragging);
  }, [dragging]);

  if (!panel) return null;
  const project = state.projects[slug];
  const entries = (project && project.artifacts) || [];
  const hasCurrent = entries.some((a) => a.path === path);
  const width = clampWidth(prefs.width, panelRef.current && panelRef.current.parentElement);

  return html`
    <div class="artifact-panel ${docked ? "docked" : ""}" ref=${panelRef} style=${`width:${width}px`} data-testid="artifact-panel" data-docked=${docked}>
      <div class="panel-resizer ${dragging ? "active" : ""}" onMouseDown=${startDrag} data-testid="panel-resizer" title="Drag to resize"></div>
      <div class="panel-head">
        <select class="switcher" aria-label="Artifact" data-testid="artifact-switcher" value=${path} onChange=${(e) => openArtifact(slug, e.target.value)}>
          ${!hasCurrent && html`<option value=${path}>${path.replace(/^artifacts\//, "")}</option>`}
          ${entries.map((a) => html`<option value=${a.path}>${a.kind === "link" && a.title ? a.title : a.name}</option>`)}
        </select>
        ${info && info.mtime && !rev && html`<span class="panel-time" data-testid="panel-time" title=${new Date(info.mtime * 1000).toLocaleString()}>${timeAgo(info.mtime)}</span>`}
        <span class="grow"></span>
        ${info && info.versions.length > 0 && html`
          <select class="versions" aria-label="Version" data-testid="version-select" value=${rev || ""} onChange=${(e) => openArtifact(slug, path, e.target.value || null)}>
            <option value="">Current</option>
            ${info.versions.map((v) => html`<option value=${v.rev}>${v.rev} · ${timeAgo(v.ts)} · ${v.subject}</option>`)}
          </select>`}
        <button class="icon-btn ${docked ? "active" : ""}" title=${docked ? "Undock" : "Dock beside the view"} aria-label=${docked ? "Undock" : "Dock"} aria-pressed=${docked} data-testid="dock" onClick=${() => dockArtifact(!docked)}><${Icon} name="dock" /></button>
        ${info && html`<a class="icon-btn" title="Open in a new tab" aria-label="Open in a new tab" href=${info.stable_url} target="_blank" rel="noopener" data-testid="open-new-tab"><${Icon} name="external" /></a>`}
        <button class="icon-btn" title="Close" aria-label="Close" data-testid="panel-close" onClick=${closeArtifact}><${Icon} name="close" /></button>
      </div>
      ${info && info.unpublished && html`<div class="panel-note">This working copy has changes that haven't been published.</div>`}
      <div class="panel-body ${info && (info.kind === "html" || info.kind === "pdf") ? "frame" : ""}" ref=${bodyRef}>
        ${error && html`<div class="terminal-message">${error}</div>`}
        ${info && !info.exists && !info.rev && html`<div class="terminal-message">This file no longer exists in the working copy. Earlier versions are in the menu above.</div>`}
        ${info && (info.exists || info.rev) && html`<${ArtifactBody} info=${info} slug=${slug} reload=${reload} docRef=${docRef} frameRef=${frameRef} />`}
        ${selection && info && html`
          <${CommentBox} selection=${selection} slug=${slug} info=${info} boxRef=${boxRef} textRef=${textRef} onClose=${clearSelection} />`}
      </div>
    </div>`;
}

function ArtifactBody({ info, slug, reload, docRef, frameRef }) {
  const { kind } = info;
  if (kind === "markdown") {
    return html`<div class="panel-doc md" ref=${docRef} data-testid="artifact-doc"
      dangerouslySetInnerHTML=${{ __html: renderMarkdown(info.text, { base: info.url, slug }) }}></div>`;
  }
  if (kind === "html") {
    return html`<iframe key=${`${info.embed_url}#${reload}`} ref=${frameRef} src=${info.embed_url} sandbox=${IFRAME_SANDBOX} title=${info.name} data-testid="artifact-frame"></iframe>`;
  }
  if (kind === "image") {
    return html`<div class="panel-image"><img key=${reload} src=${`${info.url}${info.rev ? "" : `?v=${info.mtime}`}`} alt=${info.name} data-testid="artifact-image" /></div>`;
  }
  if (kind === "pdf") {
    return html`<iframe key=${`${info.url}#${reload}`} src=${info.url} title=${info.name} data-testid="artifact-pdf"></iframe>`;
  }
  if (kind === "link") {
    const link = info.link || {};
    return html`
      <div class="link-card" data-testid="artifact-link">
        <h2><${Icon} name="link" /> ${link.title || info.name}</h2>
        ${link.description && html`<p class="muted">${link.description}</p>`}
        <div class="url">${link.url || "This link isn't valid."}</div>
        ${link.url && html`<a class="btn primary" href=${link.url} target="_blank" rel="noopener noreferrer">Open <${Icon} name="external" /></a>`}
      </div>`;
  }
  if (kind === "text") {
    return html`<pre class="panel-text" ref=${docRef} data-testid="artifact-doc">${info.text}</pre>`;
  }
  return html`
    <div class="link-card">
      <h2><${Icon} name=${ARTIFACT_ICONS.other} /> ${info.name}</h2>
      <p class="muted">This file type can't be shown here.</p>
      <a class="btn" href=${info.stable_url} target="_blank" rel="noopener">Open or download</a>
    </div>`;
}

/** Links to artifacts anywhere in the app open the panel. */
export function installArtifactLinks() {
  // A path a message names may not be a picture yet, or any more. Its link stays in the text.
  document.addEventListener("error", (e) => {
    const link = e.target && e.target.closest ? e.target.closest("a.inline-image[data-named]") : null;
    if (link) link.remove();
  }, true);
  document.addEventListener("click", (e) => {
    const link = e.target && e.target.closest ? e.target.closest("a.artifact-ref") : null;
    if (!link) return;
    e.preventDefault();
    const path = link.getAttribute("data-artifact");
    const current = state.route.slug;
    const target = link.getAttribute("data-project") || current;
    if (!target || !path) return;
    if (target !== current) {
      import("./store.js").then(({ navigate, projectPath }) => {
        navigate(projectPath(target));
        openArtifact(target, path);
      });
      return;
    }
    openArtifact(target, path);
  });
}

