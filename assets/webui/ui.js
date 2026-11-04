// Small shared components.
import { html, useEffect, useRef, useState } from "./lib.js";

const PATHS = {
  home: "M3 11.5 12 4l9 7.5M5.5 10v9.5h13V10",
  memory: "M12 5a3 3 0 0 0-5.8.9A3 3 0 0 0 4.4 11a3 3 0 0 0 1.9 5A3 3 0 0 0 12 17zM12 5a3 3 0 0 1 5.8.9 3 3 0 0 1 1.8 5.1 3 3 0 0 1-1.9 5A3 3 0 0 1 12 17z",
  terminal: "M4 5h16v14H4zM7.5 9.5l3 2.5-3 2.5M13 15h3.5",
  folder: "M3.5 6.5h6l2 2h9v10h-17z",
  plus: "M12 5v14M5 12h14",
  close: "M6 6l12 12M18 6L6 18",
  chevron: "M9 6l6 6-6 6",
  left: "M15 6l-6 6 6 6",
  right: "M9 6l6 6-6 6",
  up: "M12 19V6M6 12l6-6 6 6",
  reply: "M9 7 4 12l5 5M4 12h10a6 6 0 0 1 6 6v1",
  attach: "M20 11.5 12 19.5a5 5 0 0 1-7-7l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7L9.7 17.2a1.6 1.6 0 0 1-2.3-2.3L15 7.5",
  external: "M14 5h5v5M19 5l-8 8M18 14v5H5V6h5",
  dock: "M4 5h16v14H4zM14.5 5v14",
  undock: "M4 5h16v14H4zM14.5 9.5v5",
  menu: "M4 7h16M4 12h16M4 17h16",
  rail: "M4 5h16v14H4zM15 5v14M17.5 9h0M17.5 12h0",
  doc: "M7 3.5h7l4 4v13H7zM14 3.5v4h4M9.5 12.5h6M9.5 16h6",
  html: "M8 8l-4 4 4 4M16 8l4 4-4 4M13.5 6l-3 12",
  image: "M4 5h16v14H4zM8.5 10a1.5 1.5 0 1 0 0-.01M20 15l-4.5-4.5L6 19",
  pdf: "M7 3.5h7l4 4v13H7zM14 3.5v4h4",
  link: "M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3A4 4 0 0 0 11 18.7l1-1",
  play: "M7 5l12 7-12 7z",
  stop: "M6.5 6.5h11v11h-11z",
  check: "M5 12.5l4.5 4.5L19 7.5",
  unarchive: "M3.5 4.5h17v4.5h-17zM5 9v10.5h14V9M12 17v-5.5M9.5 14 12 11.5l2.5 2.5",
  cloud: "M7 18.5h10.5a3.75 3.75 0 0 0 .5-7.47A6 6 0 0 0 6.4 9.9 4.3 4.3 0 0 0 7 18.5z",
};

export function Icon({ name, class: className = "" }) {
  return html`<svg class="i ${className}" viewBox="0 0 24 24" aria-hidden="true"><path d=${PATHS[name] || ""} /></svg>`;
}

/** The app's mark: "am" in white on the brand purple. The favicon in index.html draws the same thing. */
export const LOGO_PATH = "M12.8 13.4V18.6M16.4 13.4V18.6M16.4 15.4a2 2 0 0 1 4 0V18.6M20.4 15.4a2 2 0 0 1 4 0V18.6";
export function Logo() {
  return html`<svg class="logo" viewBox="0 0 32 32" aria-hidden="true"><rect width="32" height="32" rx="7" fill="var(--brand)" /><g fill="none" stroke="#fff" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="10.2" cy="16" r="2.6" /><path d=${LOGO_PATH} /></g></svg>`;
}

export const ARTIFACT_ICONS = { markdown: "doc", html: "html", image: "image", pdf: "pdf", link: "link", text: "doc", other: "doc" };

/** Red while anything is new; neutral once it's all been looked at. */
export function AttentionCount({ counts }) {
  if (!counts || !counts.open) return null;
  return html`<span class="count ${counts.new ? "attention-new" : ""}" data-testid="attention-count">${counts.label}</span>`;
}

export function Chevron({ open }) {
  return html`<svg class="i chev ${open ? "open" : ""}" viewBox="0 0 24 24" aria-hidden="true"><path d=${PATHS.chevron} /></svg>`;
}

/**
 * A setting that saves when you leave it. It keeps its own draft while focused: Preact
 * writes a bound `value` back to the DOM on every render, and with live updates arriving
 * at any moment, that would wipe whatever is being typed.
 */
export function TextField({ value, onCommit, multiline = false, ...rest }) {
  const current = value ?? "";
  const [draft, setDraft] = useState(current);
  const focused = useRef(false);
  useEffect(() => {
    if (!focused.current) setDraft(current);
  }, [current]);
  const commit = () => {
    if (draft !== current) onCommit(draft);
  };
  const handlers = {
    value: draft,
    onInput: (e) => setDraft(e.target.value),
    onFocus: () => { focused.current = true; },
    onBlur: () => { focused.current = false; commit(); },
    onKeyDown: (e) => {
      if (e.key === "Enter" && !multiline) e.target.blur();
      if (e.key === "Escape") {
        setDraft(current);
        e.stopPropagation();
      }
    },
  };
  return multiline
    ? html`<textarea ...${rest} ...${handlers}></textarea>`
    : html`<input type="text" ...${rest} ...${handlers} />`;
}
