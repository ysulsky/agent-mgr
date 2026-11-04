// Images in the chat open in a lightbox over the page. The artifact panel's own documents keep their behaviour.
import { html, render, useLayoutEffect, useRef, useState } from "./lib.js";
import { Icon } from "./ui.js";

const CHAT_IMAGE = ".item a.inline-image img, .item .attachments a img";

function describe(img) {
  const link = img.closest("a");
  const path = link && link.getAttribute("data-artifact");
  const name = path || img.getAttribute("alt") || decodeURIComponent(new URL(img.src).pathname.split("/").pop());
  const alt = img.getAttribute("alt") || "";
  return { src: img.currentSrc || img.src, name, caption: alt && alt !== name ? alt : "" };
}

function Lightbox({ images, start, onClose }) {
  const [index, setIndex] = useState(start);
  const closeRef = useRef(null);
  const image = images[index];
  const many = images.length > 1;
  const step = (delta) => setIndex((i) => (i + delta + images.length) % images.length);

  // Before paint, so a key pressed straight after the click already reaches the lightbox.
  useLayoutEffect(() => {
    closeRef.current.focus();
    const onKey = (e) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft" && many) step(-1);
      else if (e.key === "ArrowRight" && many) step(1);
      else if (e.key === "Tab") {
        // Keep focus inside the dialog.
        const stops = [...document.querySelectorAll(".lightbox a, .lightbox button")];
        const at = stops.indexOf(document.activeElement);
        const next = (at + (e.shiftKey ? -1 : 1) + stops.length) % stops.length;
        stops[next].focus();
      } else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const onBackdrop = (e) => {
    if (!e.target.closest("img, a, button, .lightbox-bar")) onClose();
  };
  const file = image.name.split("/").pop();
  return html`
    <div class="lightbox" role="dialog" aria-modal="true" aria-label=${`Image: ${image.name}`} data-keep-panel data-testid="lightbox" onClick=${onBackdrop}>
      <div class="lightbox-bar">
        <div class="lightbox-title">
          <span class="ellipsis" data-testid="lightbox-name">${image.caption || image.name}</span>
          ${image.caption && html`<span class="lightbox-sub ellipsis">${image.name}</span>`}
        </div>
        ${many && html`<span class="lightbox-count" data-testid="lightbox-count">${index + 1} / ${images.length}</span>`}
        <a class="lightbox-action" href=${image.src} target="_blank" rel="noopener">Open in new tab</a>
        <a class="lightbox-action" href=${image.src} download=${file}>Download</a>
        <button class="lightbox-close" ref=${closeRef} aria-label="Close" title="Close (Esc)" onClick=${onClose}><${Icon} name="close" /></button>
      </div>
      <div class="lightbox-stage">
        ${many && html`<button class="lightbox-nav prev" aria-label="Previous image" onClick=${() => step(-1)}><${Icon} name="left" /></button>`}
        <img src=${image.src} alt=${image.caption || image.name} data-testid="lightbox-image" />
        ${many && html`<button class="lightbox-nav next" aria-label="Next image" onClick=${() => step(1)}><${Icon} name="right" /></button>`}
      </div>
    </div>`;
}

export function openLightbox(images, start = 0, opener = document.activeElement) {
  const host = document.createElement("div");
  document.body.append(host);
  document.documentElement.classList.add("lightbox-open");
  const close = () => {
    render(null, host);
    host.remove();
    document.documentElement.classList.remove("lightbox-open");
    if (opener && opener.isConnected && opener.focus) opener.focus();
  };
  render(html`<${Lightbox} images=${images} start=${start} onClose=${close} />`, host);
}

/** Runs before the artifact-link handler, so a chat image no longer opens the panel. */
export function installLightbox() {
  document.addEventListener("click", (e) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    // The image itself, or its link when opened from the keyboard.
    const el = e.target && e.target.closest ? e.target.closest("img, a") : null;
    const img = el && (el.tagName === "IMG" ? el : el.querySelector(":scope > img"));
    if (!img || !img.matches(CHAT_IMAGE) || img.closest(".artifact-panel")) return;
    e.preventDefault();
    e.stopPropagation();
    const all = [...img.closest(".item").querySelectorAll(CHAT_IMAGE)];
    openLightbox(all.map(describe), all.indexOf(img), img.closest("a"));
  }, true);
}
