// Markdown for messages, answers and artifacts. Everything agents write is sanitized.
// References to artifacts/... become links that open the artifact panel.
import { marked, DOMPurify, html, useMemo } from "./lib.js";

marked.setOptions({ gfm: true, breaks: true });

const SANITIZE = {
  FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select", "iframe", "object", "embed", "video", "audio", "svg", "math"],
  FORBID_ATTR: ["style", "srcset", "ping"],
  ALLOW_DATA_ATTR: false,
};

// A path to an artifact, possibly under a project: artifacts/x.html, ./artifacts/x.html,
// projects/foo/artifacts/x.html or /Users/me/agents/projects/foo/artifacts/x.html.
const ARTIFACT_RE = /(?<![\w./-])((?:[\w./~-]*\/)?projects\/([a-z0-9-]+)\/|\.\/)?(artifacts\/[A-Za-z0-9_@%+\-./]*[A-Za-z0-9_@%+\-/])/g;

// The same forms, for a whole path at a time.
const ARTIFACT_PATH_RE = /^((?:[\w./~-]*\/)?projects\/([a-z0-9-]+)\/|\.\/)?(artifacts\/[^?#]+)$/;
const IMAGE_RE = /\.(png|jpe?g|gif|webp|svg)$/i;
const MAX_INLINE_IMAGES = 4;

function artifactLink(doc, path, projectSlug, label) {
  const a = doc.createElement("a");
  a.className = "artifact-ref";
  a.setAttribute("data-artifact", path.replace(/\/+$/, ""));
  if (projectSlug) a.setAttribute("data-project", projectSlug);
  a.setAttribute("href", "#");
  a.textContent = label;
  return a;
}

function linkifyTextNodes(root, doc) {
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  for (const node of nodes) {
    if (node.parentElement && node.parentElement.closest("a")) continue;
    const text = node.nodeValue;
    ARTIFACT_RE.lastIndex = 0;
    if (!ARTIFACT_RE.test(text)) continue;
    ARTIFACT_RE.lastIndex = 0;
    const fragment = doc.createDocumentFragment();
    let last = 0;
    let match;
    while ((match = ARTIFACT_RE.exec(text))) {
      const whole = match[0];
      const path = match[3];
      if (path === "artifacts/") continue;
      fragment.append(text.slice(last, match.index));
      fragment.append(artifactLink(doc, path, match[2], whole));
      last = match.index + whole.length;
    }
    fragment.append(text.slice(last));
    node.replaceWith(fragment);
  }
}

function safeDecode(path) {
  try {
    return decodeURI(path);
  } catch {
    return path;
  }
}

function fixLinks(root, base, slug) {
  for (const a of root.querySelectorAll("a[href]")) {
    const href = a.getAttribute("href");
    const local = href.match(/^(?:\.\/)?(artifacts\/[^?#]+)/) || href.match(/^\/artifacts\/[a-z0-9-]+\/([^?#]+)/);
    if (local) {
      const path = local[1].startsWith("artifacts/") ? local[1] : `artifacts/${local[1]}`;
      a.className = "artifact-ref";
      a.setAttribute("data-artifact", safeDecode(path));
      a.setAttribute("href", "#");
      continue;
    }
    if (a.classList.contains("artifact-ref")) continue;
    a.setAttribute("target", "_blank");
    a.setAttribute("rel", "noopener noreferrer");
  }
  for (const img of root.querySelectorAll("img[src]")) {
    const src = img.getAttribute("src");
    if (/^(https?:|data:|blob:)/.test(src)) {
      if (/^https?:/.test(src)) img.replaceWith(Object.assign(document.createElement("span"), { className: "muted", textContent: `[external image: ${src}]` }));
      continue;
    }
    if (base) {
      img.setAttribute("src", new URL(src, new URL(base, location.href)).pathname);
    } else if (slug) {
      resolveImage(img, document, slug);
    }
  }
}

function imageUrl(project, path) {
  return `/artifacts/${encodeURIComponent(project)}/${path.slice("artifacts/".length)}`;
}

/** An image that opens its artifact in the panel when clicked. */
function imageLink(doc, path, projectSlug) {
  const a = artifactLink(doc, path, projectSlug, "");
  a.classList.add("inline-image");
  return a;
}

/** Point a Markdown image at its artifact, whichever way the path was written. */
function resolveImage(img, doc, slug) {
  const match = img.getAttribute("src").match(ARTIFACT_PATH_RE);
  if (!match || /(^|\/)\.\.(\/|$)/.test(match[3])) return;
  const path = safeDecode(match[3]);
  img.setAttribute("src", imageUrl(match[2] || slug, match[3]));
  if (img.closest("a")) return;
  const link = imageLink(doc, path, match[2]);
  img.replaceWith(link);
  link.append(img);
}

/** Show the pictures a message only names by path, under the text that names them. */
function inlineNamedImages(root, doc, slug) {
  const id = (link) => `${link.getAttribute("data-project") || slug}/${link.getAttribute("data-artifact")}`;
  const shown = new Set([...root.querySelectorAll("a.inline-image")].map(id));
  const lastInBlock = new Map();
  for (const link of root.querySelectorAll("a.artifact-ref:not(.inline-image)")) {
    if (shown.size >= MAX_INLINE_IMAGES) return;
    const path = link.getAttribute("data-artifact");
    const block = link.closest("p, li, blockquote, h1, h2, h3, h4");
    if (!IMAGE_RE.test(path) || shown.has(id(link)) || !block || link.closest("pre, td, th")) continue;
    if (/(^|\/)\.\.(\/|$)/.test(path)) continue;
    shown.add(id(link));
    const project = link.getAttribute("data-project");
    const wrapper = imageLink(doc, path, project);
    wrapper.setAttribute("data-named", "1"); // Hidden if it doesn't load: the name stays, as a link.
    const img = root.ownerDocument.createElement("img");
    img.setAttribute("src", imageUrl(project || slug, encodeURI(path)));
    img.setAttribute("alt", path);
    wrapper.append(img);
    const previous = lastInBlock.get(block);
    if (previous) previous.after(wrapper);
    else if (block.tagName === "P") block.after(wrapper);
    else block.append(wrapper);
    lastInBlock.set(block, wrapper);
  }
}

function escapeHtml(text) {
  const div = document.createElement("div");
  div.textContent = text || "";
  return div.innerHTML;
}

export function renderMarkdown(text, { base, slug, inlineImages = false } = {}) {
  try {
    const raw = marked.parse(text || "", { async: false });
    const clean = DOMPurify.sanitize(raw, SANITIZE);
    const template = document.createElement("template");
    template.innerHTML = clean;
    const root = template.content;
    fixLinks(root, base, slug);
    linkifyTextNodes(root, document);
    if (inlineImages && slug) inlineNamedImages(root, document, slug);
    return template.innerHTML;
  } catch (e) {
    // Agents write this text. Whatever's in it, the rest of the page has to keep working.
    console.warn("Could not render markdown", e);
    return `<pre>${escapeHtml(text)}</pre>`;
  }
}

export function Markdown({ text, base, slug, className = "", inlineImages = false }) {
  const rendered = useMemo(() => renderMarkdown(text, { base, slug, inlineImages }), [text, base, slug, inlineImages]);
  return html`<div class="md ${className}" dangerouslySetInnerHTML=${{ __html: rendered }}></div>`;
}
