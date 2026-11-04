// The chat: a manager's or worker's messages, answer cards and activity rows.
import { html, useEffect, useLayoutEffect, useRef, useState, useCallback } from "./lib.js";
import {
  state, api, attempt, chatKey, loadChat, loadEarlier, markSeen, setReply, refreshCurrent, timeAgo, clock, elapsed, money, tokens,
  turnPath, useTick, toast, recall, remember, update,
} from "./store.js";
import { Markdown } from "./md.js";
import { Icon, Chevron } from "./ui.js";

function isArchived(slug) {
  const project = state.projects[slug];
  return !!(project && project.project.archived_at);
}

function excerptOf(text, limit = 240) {
  const flat = (text || "").replace(/\s+/g, " ").trim();
  return flat.length > limit ? `${flat.slice(0, limit - 1)}…` : flat;
}

function attachmentUrl(slug, attachment) {
  return `/api/projects/${encodeURIComponent(slug)}/attachments/${encodeURIComponent(attachment.path.split("/").pop())}`;
}

/** Scroll to an item, fetching older chat if it's outside the loaded window. */
export async function jumpTo(slug, agent, id) {
  const base = id.split("@")[0];
  const version = id.includes("@") ? parseInt(id.split("@v")[1], 10) : null;
  if (version) update((s) => (s.viewVersion[base] = version));
  const domId = (version ? `answer-${base}` : `item-${base}`);
  const find = () => document.getElementById(domId) || document.getElementById(`answer-${base}`) || document.getElementById(`item-${base}`);
  let element = find();
  if (!element && (base.startsWith("m-") || base.startsWith("t-"))) {
    await loadChat(slug, agent, { since: base });
    await new Promise((resolve) => setTimeout(resolve, 120));
    element = find();
  }
  if (!element) return;
  document.dispatchEvent(new CustomEvent("am-jump"));
  // An answer comes into view under its question, which goes to the top. Both
  // ids carry the question's id; without the question, the answer itself goes.
  const question = element.id === `answer-${base}` ? document.getElementById(`item-${base}`) : null;
  (question || element).scrollIntoView({ block: "start", behavior: "smooth" });
  element.classList.remove("flash");
  void element.offsetWidth;
  element.classList.add("flash");
}

function ReplyButton({ slug, agent, target, excerpt }) {
  return html`<button
    class="icon-btn reply-btn" title="Reply" aria-label="Reply"
    onClick=${(e) => {
      e.stopPropagation();
      setReply(slug, agent, { id: target, excerpt: excerptOf(excerpt) });
      const box = document.querySelector(".composer textarea");
      if (box) box.focus();
    }}><${Icon} name="reply" /></button>`;
}

function Excerpt({ slug, agent, reply }) {
  return html`<button class="excerpt" title="Go to ${reply.id}" onClick=${() => jumpTo(slug, agent, reply.id)}>${reply.excerpt || reply.id}</button>`;
}

function QuestionBubble({ slug, agent, item, agentBusy, archived }) {
  const interrupt = () =>
    attempt(async () => {
      await api(`/api/projects/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}/interrupt`, { json: { message_id: item.id } });
      refreshCurrent();
    });
  return html`
    <div class="item msg-user" id="item-${item.id}" tabindex="0" data-testid="question">
      <${ReplyButton} slug=${slug} agent=${agent} target=${item.id} excerpt=${item.text} />
      ${item.reply_to && html`<${Excerpt} slug=${slug} agent=${agent} reply=${item.reply_to} />`}
      ${item.comment && html`
        <div class="comment-quote">
          <span class="where">Comment on <a class="artifact-ref" href="#" data-artifact=${item.comment.artifact}>${item.comment.artifact}</a></span>
          ${excerptOf(item.comment.quote, 400)}
        </div>`}
      <${Markdown} text=${item.text} slug=${slug} inlineImages=${true} />
      ${item.attachments && item.attachments.length > 0 && html`
        <div class="attachments">
          ${item.attachments.map((a) =>
            (a.mime || "").startsWith("image/")
              ? html`<a href=${attachmentUrl(slug, a)} target="_blank" rel="noopener"><img src=${attachmentUrl(slug, a)} alt=${a.name} /></a>`
              : html`<a class="file-chip" href=${attachmentUrl(slug, a)}><${Icon} name="attach" /><span class="ellipsis">${a.name}</span></a>`)}
        </div>`}
      <div class="msg-meta">
        ${item.status === "queued" && html`
          <span class="pill queued" data-testid="queued">${archived ? "Held until un-archived" : item.interrupting ? "Interrupting…" : "Queued"}</span>
          ${agentBusy && !archived && !item.interrupting && html`<button class="link-btn" onClick=${interrupt}>Interrupt and send</button>`}`}
        ${item.status === "interrupted" && html`<span class="pill">Interrupted</span>`}
        ${(item.status === "failed" || item.status === "vanished") && html`<span class="pill failed">Failed</span>`}
        <span title=${clock(item.ts)}>${timeAgo(item.ts)}</span>
      </div>
    </div>`;
}

function AnswerCard({ slug, agent, item, scroller }) {
  const answers = item.answers;
  const latest = answers[answers.length - 1];
  const ref = useRef(null);
  const chosen = state.viewVersion[item.id];
  const version = chosen && chosen <= latest.version ? chosen : latest.version;
  const answer = answers.find((a) => a.version === version) || latest;
  const isUnseen = (item.seen || 0) < latest.version;
  const isUpdate = isUnseen && latest.version > 1;

  const step = (delta) => update((s) => {
    const next = Math.min(latest.version, Math.max(1, version + delta));
    s.viewVersion[item.id] = next === latest.version ? undefined : next;
  });

  // The user has seen a version once its card has been on screen, in a focused window, for a moment.
  useEffect(() => {
    if (!isUnseen || !ref.current) return undefined;
    let timer = null;
    let visible = false;
    const check = () => {
      if (timer || !visible || document.visibilityState !== "visible" || !document.hasFocus()) return;
      timer = setTimeout(() => {
        timer = null;
        if (visible && document.hasFocus()) {
          update((s) => (s.viewVersion[item.id] = undefined));
          markSeen(slug, item.id, latest.version);
        }
      }, 700);
    };
    const observer = new IntersectionObserver(
      (entries) => {
        visible = entries.some((e) => e.isIntersecting);
        if (!visible) {
          clearTimeout(timer);
          timer = null;
        }
        check();
      },
      { root: scroller.current, threshold: 0.15 },
    );
    observer.observe(ref.current);
    window.addEventListener("focus", check);
    return () => {
      observer.disconnect();
      clearTimeout(timer);
      window.removeEventListener("focus", check);
    };
  }, [isUnseen, latest.version, slug, item.id]);

  return html`
    <div class="item answer ${isUpdate ? "is-updated" : ""}" id="answer-${item.id}" tabindex="0" ref=${ref} data-testid="answer" data-version=${answer.version}>
      <${ReplyButton} slug=${slug} agent=${agent} target=${answer.id} excerpt=${answer.text} />
      <${Markdown} text=${answer.text} slug=${slug} inlineImages=${true} />
      <div class="answer-foot">
        ${answers.length > 1 && html`
          <span class="version-nav">
            <button class="icon-btn" title="Older version" aria-label="Older version" disabled=${version <= 1} onClick=${() => step(-1)}><${Icon} name="left" /></button>
            <span class="label" data-testid="version-label">v${answer.version} of ${latest.version}</span>
            <button class="icon-btn" title="Newer version" aria-label="Newer version" disabled=${version >= latest.version} onClick=${() => step(1)}><${Icon} name="right" /></button>
          </span>`}
        ${isUpdate && html`<span class="pill updated" data-testid="updated-pill">Updated · v${latest.version}</span>`}
        ${version < latest.version && !isUpdate && html`<span class="older-note">An older version</span>`}
        <span title=${clock(answer.ts)}>${timeAgo(answer.ts)}</span>
      </div>
    </div>`;
}

function NoteDetail({ slug, agent, note }) {
  return html`
    <div class="activity-detail">
      ${note.triggers && note.triggers.length > 0 && html`
        <div class="triggers">
          ${note.triggers.map((t) => html`<div class="trigger"><b>${t.kind}</b> from ${t.from}: ${t.excerpt}</div>`)}
        </div>`}
      <${Markdown} text=${note.full || note.text} slug=${slug} />
      <div class="detail-foot">
        <a href=${turnPath(slug, agent, note.id)}>View turn</a>
        ${note.cost_usd > 0 && html`<span>${money(note.cost_usd, note.cost_is_estimate)}</span>`}
        ${note.tokens && html`<span>${tokens(note.tokens)}</span>`}
        <span>${clock(note.ts)}</span>
      </div>
    </div>`;
}

function NoteRow({ slug, agent, note }) {
  const [open, setOpen] = useState(false);
  return html`
    <div class="item activity ${note.state === "failed" ? "failed" : ""}" id="item-${note.id}" data-testid="activity">
      <${ReplyButton} slug=${slug} agent=${agent} target=${note.id} excerpt=${note.full || note.text} />
      <button class="activity-row" aria-expanded=${open} onClick=${() => setOpen(!open)}>
        <${Chevron} open=${open} />
        <span class="text">${note.text}</span>
        <span class="when" title=${clock(note.ts)}>${timeAgo(note.ts)}</span>
      </button>
      ${open && html`<${NoteDetail} slug=${slug} agent=${agent} note=${note} />`}
    </div>`;
}

function FoldedNotes({ slug, agent, notes }) {
  const [open, setOpen] = useState(false);
  return html`
    <div class="activity" data-testid="folded-activity">
      <button class="activity-row" aria-expanded=${open} onClick=${() => setOpen(!open)}>
        <${Chevron} open=${open} />
        <span class="text">${notes.length} background events</span>
        <span class="when">${timeAgo(notes[notes.length - 1].ts)}</span>
      </button>
      ${open && html`<div style="margin-left:14px">${notes.map((n) => html`<${NoteRow} key=${n.id} slug=${slug} agent=${agent} note=${n} />`)}</div>`}
    </div>`;
}

function RunningRow({ slug, agent, turn }) {
  useTick(5000);
  const trigger = turn.triggers && turn.triggers[0];
  const label = turn.kind === "side" ? "Answering on a side turn" : trigger ? `Working on ${trigger.kind} from ${trigger.from}` : "Working";
  return html`
    <div class="working-row" data-testid="running">
      <span class="spinner"></span>
      <span class="grow ellipsis">${turn.stopping ? "Stopping…" : label} · ${elapsed(turn.started)}</span>
      <a href=${turnPath(slug, agent, turn.id)}>View</a>
    </div>`;
}

/** Runs of activity rows: one stays a row, two or more fold together. */
function groupItems(items) {
  const out = [];
  let run = [];
  const flush = () => {
    if (run.length === 1) out.push({ type: "note", note: run[0], key: run[0].id });
    else if (run.length > 1) out.push({ type: "fold", notes: run, key: `fold-${run[0].id}` });
    run = [];
  };
  for (const item of items) {
    if (item.type === "note") run.push(item);
    else {
      flush();
      out.push({ type: "question", item, key: item.id });
    }
  }
  flush();
  return out;
}

function UpdatedMarkers({ slug, agent, items }) {
  const project = state.projects[slug];
  const updated = (project && project.unread.agents[agent] && project.unread.agents[agent].updated) || [];
  if (!updated.length) return null;
  return html`
    <div class="updated-markers">
      ${updated.map((u) => {
        const item = items.find((i) => i.id === u.question);
        return html`
          <button class="updated-marker" data-testid="updated-marker" onClick=${() => jumpTo(slug, agent, `${u.question}@v${u.version}`)}>
            <${Icon} name="up" />
            <span class="grow ellipsis">This answer was updated to v${u.version}${item ? `: ${excerptOf(item.text, 90)}` : ""}</span>
          </button>`;
      })}
    </div>`;
}

function Composer({ slug, agent, status }) {
  const key = chatKey(slug, agent);
  const [text, setText] = useState(() => recall(`draft.${key}`, ""));
  const [files, setFiles] = useState(() => recall(`draftFiles.${key}`, []));
  const [uploading, setUploading] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [sending, setSending] = useState(false);
  const box = useRef(null);
  const fileInput = useRef(null);
  const reply = state.replyTo[key];
  const busy = !!(status && status.main_turn);
  const isManager = !status || status.role === "manager";
  const archived = isArchived(slug);
  const name = agent === "manager" ? (slug === "home" ? "the home manager" : "the manager") : agent;

  useEffect(() => {
    setText(recall(`draft.${key}`, ""));
    setFiles(recall(`draftFiles.${key}`, []));
  }, [key]);

  useLayoutEffect(() => {
    if (!box.current) return;
    box.current.style.height = "auto";
    box.current.style.height = `${box.current.scrollHeight}px`;
  }, [text]);

  const change = (value) => {
    setText(value);
    remember(`draft.${key}`, value);
  };

  // Attachments belong to this chat's draft. They go through storage, not this component's
  // state, so an upload that finishes after the user has moved on still lands in the right chat.
  const changeFiles = (edit) => {
    const next = edit(recall(`draftFiles.${key}`, []));
    remember(`draftFiles.${key}`, next);
    setFiles(next);
  };

  const upload = async (list) => {
    const chosen = [...list];
    if (!chosen.length) return;
    setUploading((n) => n + chosen.length);
    const form = new FormData();
    for (const file of chosen) form.append("file", file, file.name || "screenshot.png");
    await attempt(async () => {
      const result = await api(`/api/projects/${encodeURIComponent(slug)}/attachments`, { method: "POST", body: form });
      changeFiles((existing) => [...existing, ...result.files]);
    });
    setUploading((n) => n - chosen.length);
  };

  const send = async (interrupt = false) => {
    if (sending || uploading) return;
    if (!text.trim() && !files.length && !interrupt) return;
    if (!text.trim() && !files.length) return;
    setSending(true);
    const body = { text, attachments: files, interrupt };
    if (reply) body.reply_to = reply;
    const sent = await attempt(() => api(`/api/projects/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}/messages`, { json: body }));
    setSending(false);
    if (!sent) return;
    // Only what was sent: anything typed, attached or replied to since then is still a draft.
    if (recall(`draft.${key}`, "") === text) change("");
    changeFiles((current) => current.filter((f) => !files.some((sentFile) => sentFile.path === f.path)));
    if (state.replyTo[key] === reply) setReply(slug, agent, null);
    refreshCurrent();
    document.dispatchEvent(new CustomEvent("am-sent"));
  };

  const onKeyDown = (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      send(false);
    }
  };

  const onPaste = (e) => {
    const pasted = [...(e.clipboardData ? e.clipboardData.files : [])];
    if (pasted.length) {
      e.preventDefault();
      upload(pasted);
    }
  };

  const hint = archived
    ? "Archived. Messages wait until this project is un-archived."
    : busy
    ? isManager
      ? "Working. New messages wait until this turn ends."
      : "Working. It answers messages right away on a side turn."
    : "";

  return html`
    <div class="composer-wrap">
      <div
        class="composer ${dragging ? "dragging" : ""}"
        onDragOver=${(e) => { e.preventDefault(); setDragging(true); }}
        onDragLeave=${() => setDragging(false)}
        onDrop=${(e) => { e.preventDefault(); setDragging(false); upload(e.dataTransfer.files); }}>
        ${reply && html`
          <div class="reply-chip" data-testid="reply-chip">
            <${Icon} name="reply" />
            <button class="go" title="Go to ${reply.id}" onClick=${() => jumpTo(slug, agent, reply.id)}>${reply.excerpt || reply.id}</button>
            <button class="icon-btn" title="Cancel reply" aria-label="Cancel reply" onClick=${() => setReply(slug, agent, null)}><${Icon} name="close" /></button>
          </div>`}
        ${(files.length > 0 || uploading > 0) && html`
          <div class="pending-files">
            ${files.map((f, i) => html`
              <span class="pending-file" key=${f.path}>
                ${(f.mime || "").startsWith("image/") ? html`<img src=${attachmentUrl(slug, f)} alt="" />` : null}
                <span class="ellipsis">${f.name}</span>
                <button class="icon-btn" title="Remove" aria-label="Remove ${f.name}" onClick=${() => changeFiles((current) => current.filter((other) => other.path !== f.path))}><${Icon} name="close" /></button>
              </span>`)}
            ${uploading > 0 && html`<span class="pending-file"><span class="spinner"></span> Uploading…</span>`}
          </div>`}
        <textarea
          ref=${box} rows="1" value=${text} placeholder="Message ${name}"
          aria-label="Message ${name}"
          onInput=${(e) => change(e.target.value)} onKeyDown=${onKeyDown} onPaste=${onPaste}></textarea>
        <div class="composer-bar">
          <button class="icon-btn" title="Attach files" aria-label="Attach files" onClick=${() => fileInput.current.click()}><${Icon} name="attach" /></button>
          <input ref=${fileInput} type="file" multiple hidden onChange=${(e) => { upload(e.target.files); e.target.value = ""; }} />
          <span class="composer-hint">${hint}</span>
          ${busy && !archived && html`
            <button class="btn small" disabled=${!text.trim() && !files.length} data-testid="interrupt-send" onClick=${() => send(true)} title="Stop the current turn and deliver this first">Interrupt and send</button>`}
          ${busy && html`
            <button class="icon-btn" title="Stop this turn" aria-label="Stop this turn" data-testid="stop-turn" onClick=${() => attempt(async () => {
              await api(`/api/projects/${encodeURIComponent(slug)}/agents/${encodeURIComponent(agent)}/interrupt`, { json: {} });
              toast("Stopping the turn");
              refreshCurrent();
            })}><${Icon} name="stop" /></button>`}
          <button class="icon-btn send-btn" title="Send" aria-label="Send" disabled=${(!text.trim() && !files.length) || sending || uploading > 0} data-testid="send" onClick=${() => send(false)}><${Icon} name="up" /></button>
        </div>
      </div>
    </div>`;
}

export function ChatView({ slug, agent }) {
  const key = chatKey(slug, agent);
  const chat = state.chats[key];
  const project = state.projects[slug];
  const status = project && project.agents.find((a) => a.name === agent && a.project === slug);
  const scroller = useRef(null);
  const stick = useRef(true);
  const items = (chat && chat.items) || [];
  const running = (chat && chat.running) || [];

  const jumpingUntil = useRef(0);

  const onScroll = useCallback(() => {
    // A jump starts near the bottom, so its first frames would look like wanting to stick there.
    if (Date.now() < jumpingUntil.current) return;
    const el = scroller.current;
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
  }, []);

  useEffect(() => {
    stick.current = true;
    if (!state.chats[key]) attempt(() => loadChat(slug, agent));
  }, [key]);

  // A picture that finishes loading grows the chat, which shouldn't leave the end of it off screen.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return undefined;
    const onLoad = () => { if (stick.current) el.scrollTop = el.scrollHeight; };
    el.addEventListener("load", onLoad, true);
    return () => el.removeEventListener("load", onLoad, true);
  }, []);

  useEffect(() => {
    const onSent = () => { stick.current = true; };
    const onJump = () => {
      stick.current = false;
      jumpingUntil.current = Date.now() + 1200;
    };
    document.addEventListener("am-sent", onSent);
    document.addEventListener("am-jump", onJump);
    return () => {
      document.removeEventListener("am-sent", onSent);
      document.removeEventListener("am-jump", onJump);
    };
  }, []);

  useLayoutEffect(() => {
    if (stick.current && scroller.current) scroller.current.scrollTop = scroller.current.scrollHeight;
  });

  const groups = groupItems(items);
  const isHome = slug === "home";
  return html`
    <div class="chat-column">
      <div class="chat-scroll" ref=${scroller} onScroll=${onScroll} data-testid="chat-scroll">
        <div class="chat-inner">
          ${chat && chat.hasMore && html`<button class="btn small" style="align-self:center" onClick=${() => loadEarlier(slug, agent)}>Load earlier</button>`}
          ${chat && chat.loaded && !items.length && !running.length && html`
            <div class="empty">
              ${agent !== "manager"
                ? html`<h2>Talk to ${agent}</h2><p>${(status && status.purpose) || "This worker takes its direction from the manager, but you can message it directly."} Its manager gets a copy of what you say and of its answer.</p>`
                : isHome
                  ? html`<h2>Welcome to Agent Mgr</h2><p>This is the home manager. Tell it what you're working on. It creates a project for each piece of work, and each project's manager delegates to workers.</p>`
                  : html`<h2>What is this project for?</h2><p>The manager is waiting for your first message. Say what you'd like to get done, and it will take it from there.</p>`}
            </div>`}
          ${groups.map((group) => {
            if (group.type === "note") return html`<${NoteRow} key=${group.key} slug=${slug} agent=${agent} note=${group.note} />`;
            if (group.type === "fold") return html`<${FoldedNotes} key=${group.key} slug=${slug} agent=${agent} notes=${group.notes} />`;
            const item = group.item;
            return html`
              <${QuestionBubble} key=${item.id} slug=${slug} agent=${agent} item=${item} agentBusy=${!!(status && status.main_turn)} archived=${isArchived(slug)} />
              ${item.answers.length > 0 && html`<${AnswerCard} key=${`a-${item.id}`} slug=${slug} agent=${agent} item=${item} scroller=${scroller} />`}
              ${item.status === "working" && item.turn && html`
                <div class="working-row" data-testid="working" key=${`w-${item.id}`}>
                  <span class="spinner"></span><span class="grow">Working…</span>
                  <a href=${turnPath(slug, agent, item.turn)}>View</a>
                </div>`}`;
          })}
          ${running.map((turn) => html`<${RunningRow} key=${turn.id} slug=${slug} agent=${agent} turn=${turn} />`)}
          <${UpdatedMarkers} slug=${slug} agent=${agent} items=${items} />
        </div>
      </div>
      <${Composer} slug=${slug} agent=${agent} status=${status} />
    </div>`;
}
