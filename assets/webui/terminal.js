// Browser terminals: xterm.js over a websocket to a pty.
import { html, useEffect, useRef, useState } from "./lib.js";
import { Icon } from "./ui.js";

const THEME = {
  background: "#14151a",
  foreground: "#dfe0e4",
  cursor: "#dfe0e4",
  selectionBackground: "#3b437a",
  black: "#14151a", brightBlack: "#5c5e66",
  red: "#ef6f7c", brightRed: "#f58c97",
  green: "#7cc97c", brightGreen: "#97dd97",
  yellow: "#e0b95e", brightYellow: "#ecca7c",
  blue: "#7c87f5", brightBlue: "#9cb0f8",
  magenta: "#c893e0", brightMagenta: "#d6aeea",
  cyan: "#6fc6cf", brightCyan: "#8fd6dd",
  white: "#dfe0e4", brightWhite: "#ffffff",
};

function wsUrl(path) {
  return `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${path}`;
}

/** Attach a new xterm to a websocket path. Returns a function that tears it down. */
function attach(host, path, { onExit, onError } = {}) {
  const term = new window.Terminal({
    theme: THEME,
    fontFamily: 'ui-monospace, "SF Mono", SFMono-Regular, Menlo, Consolas, monospace',
    fontSize: 12,
    cursorBlink: true,
    scrollback: 5000,
    allowProposedApi: false,
  });
  const fit = new window.FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(host);
  fit.fit();
  const encoder = new TextEncoder();
  const separator = path.includes("?") ? "&" : "?";
  const socket = new WebSocket(wsUrl(`${path}${separator}cols=${term.cols}&rows=${term.rows}`));
  socket.binaryType = "arraybuffer";
  let closedByUs = false;
  let reportedExit = false;

  const sendResize = () => {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
  };
  socket.onopen = () => {
    sendResize();
    term.focus();
  };
  socket.onmessage = (event) => {
    if (typeof event.data === "string") {
      let control;
      try {
        control = JSON.parse(event.data);
      } catch {
        return;
      }
      if (control.type === "error" && onError) onError(control.message);
      if (control.type === "exit") {
        reportedExit = true;
        if (onExit) onExit(control.code);
      }
      return;
    }
    term.write(new Uint8Array(event.data));
  };
  socket.onclose = () => {
    if (!closedByUs && !reportedExit && onExit) onExit(null);
  };
  const input = term.onData((data) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(encoder.encode(data));
  });
  term.onResize(sendResize);
  const observer = new ResizeObserver(() => {
    try {
      fit.fit();
    } catch {
      // The host can be hidden mid-resize; the next observation fixes it.
    }
  });
  observer.observe(host);
  return () => {
    closedByUs = true;
    observer.disconnect();
    input.dispose();
    socket.close();
    term.dispose();
  };
}

export function TerminalView({ path, label }) {
  const host = useRef(null);
  const [message, setMessage] = useState(null);
  const [generation, setGeneration] = useState(0);
  useEffect(() => {
    setMessage(null);
    if (!host.current) return undefined;
    return attach(host.current, path, {
      onError: (text) => setMessage({ text, final: true }),
      onExit: () => setMessage((current) => current || { text: "Disconnected. The session is still running." }),
    });
  }, [path, generation]);
  return html`
    <div class="chat-column" data-testid="terminal">
      ${message && html`
        <div class="terminal-message">
          ${message.text}
          ${!message.final && html` <button class="btn small" onClick=${() => setGeneration((g) => g + 1)}>Reconnect</button>`}
        </div>`}
      <div class="terminal-host" ref=${host} aria-label=${label}></div>
    </div>`;
}

/** The Run, Fix and Connect buttons: a command the server holds, in a terminal. An attention
 * item names its own; a login, or a storage provider in `connect`, is asked for by name, and
 * the server knows what to run. */
export function RunModal({ item, login, connect, onClose }) {
  const host = useRef(null);
  const [exit, setExit] = useState(undefined);
  const command = connect ? connect.command : login ? login.fix : item.run;
  useEffect(() => {
    if (!host.current) return undefined;
    const path = connect
      ? `/api/storage/connect/${encodeURIComponent(connect.provider)}`
      : login
      ? `/api/health/logins/${encodeURIComponent(login.name)}/fix`
      : `/api/projects/${encodeURIComponent(item.project)}/attention/${item.id}/run`;
    return attach(host.current, path, {
      onExit: (code) => setExit(code),
    });
  }, []);
  return html`
    <div class="modal-scrim" data-testid="run-modal">
      <div class="modal" role="dialog" aria-label="Run ${command}">
        <div class="modal-head">
          <span class="grow ellipsis mono">${command}</span>
          ${exit !== undefined && html`<span class="pill ${exit === 0 ? "" : "failed"}" data-testid="run-exit">${exit === 0 ? "Finished" : exit === null ? "Disconnected" : `Exit ${exit}`}</span>`}
          <button class="icon-btn" title="Close" aria-label="Close" data-testid="run-close" onClick=${onClose}><${Icon} name="close" /></button>
        </div>
        <div class="run-terminal" ref=${host}></div>
      </div>
    </div>`;
}
