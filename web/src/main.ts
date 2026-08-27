import { TermlandClient, type ClientEvent } from './client.js';
import { InputCapture } from './input.js';
import { VideoPipeline } from './video.js';
import type { SessionInfo } from './messages.js';

const urlEl = document.getElementById('url') as HTMLInputElement;
const hashEl = document.getElementById('hash') as HTMLInputElement;
const userEl = document.getElementById('user') as HTMLInputElement;
const passEl = document.getElementById('pass') as HTMLInputElement;
const out = document.getElementById('out') as HTMLElement;
const sessionsEl = document.getElementById('sessions') as HTMLElement;
const canvas = document.getElementById('screen') as HTMLCanvasElement;
const banner = document.getElementById('banner') as HTMLElement;
const connectBtn = document.getElementById('connect') as HTMLButtonElement;
const newBtn = document.getElementById('new-session') as HTMLButtonElement;
const lockBtn = document.getElementById('pointer-lock') as HTMLButtonElement;

let client: TermlandClient | null = null;
let video: VideoPipeline | null = null;
let input: InputCapture | null = null;
let remoteW = 1280;
let remoteH = 720;
let resizeTimer: ReturnType<typeof setTimeout> | null = null;

function log(s: string): void {
  out.textContent = s;
}

function onEvent(ev: ClientEvent): void {
  switch (ev.type) {
    case 'status':
      log(ev.message);
      banner.hidden = true;
      break;
    case 'hello':
      log(
        `Connected to ${ev.server_name}\n  protocol handshake ok\n  auth required: ${ev.auth_required}`,
      );
      newBtn.disabled = false;
      break;
    case 'session-list':
      renderSessions(ev.sessions);
      break;
    case 'session-ready':
      remoteW = ev.width;
      remoteH = ev.height;
      log(`Session ${ev.session_id} ready ${ev.width}x${ev.height} codec=${ev.codec ?? '?'}`);
      sessionsEl.innerHTML = '';
      video?.close();
      if (client && ev.codec) {
        video = new VideoPipeline(canvas, client.probed());
        video.configure(ev.codec, ev.width, ev.height);
      }
      input?.setRemoteSize(ev.width, ev.height);
      canvas.focus();
      banner.hidden = true;
      break;
    case 'video':
      video?.push(ev.frame);
      break;
    case 'reconnecting':
      banner.hidden = false;
      banner.textContent = `Reconnecting… (attempt ${ev.attempt})`;
      break;
    case 'session-end':
      log(`Session ended: ${ev.reason}`);
      video?.close();
      banner.hidden = true;
      break;
    case 'error':
      log(`Error: ${ev.error}`);
      break;
  }
}

function renderSessions(sessions: SessionInfo[]): void {
  if (sessions.length === 0) {
    sessionsEl.innerHTML = '<p class="note">No resumable sessions.</p>';
    return;
  }
  sessionsEl.innerHTML = sessions
    .map(
      (s) =>
        `<div class="session">` +
        `<code>${s.session_id}</code> ${s.mode} ${s.width}x${s.height} ` +
        `<button data-attach="${s.session_id}">Attach</button>` +
        `<button data-close="${s.session_id}">Close</button></div>`,
    )
    .join('');
}

connectBtn.addEventListener('click', async () => {
  client?.stop();
  input?.detach();
  video?.close();
  client = new TermlandClient(
    {
      url: urlEl.value.trim(),
      certHashHex: hashEl.value.trim() || undefined,
      username: userEl.value.trim() || undefined,
      password: passEl.value || undefined,
    },
    onEvent,
  );
  input = new InputCapture(canvas, (msg) => client?.send(msg));
  input.attach();
  await client.start();
});

newBtn.addEventListener('click', () => {
  const w = Math.max(320, window.innerWidth);
  const h = Math.max(240, window.innerHeight - 180);
  client?.createSession(w, h);
});

lockBtn.addEventListener('click', () => {
  canvas.requestPointerLock();
});

sessionsEl.addEventListener('click', (e) => {
  const t = e.target as HTMLElement;
  const attach = t.getAttribute('data-attach');
  const close = t.getAttribute('data-close');
  if (attach) client?.attachSession(attach, remoteW, remoteH);
  if (close) client?.closeSession(close);
});

window.addEventListener('resize', () => {
  if (resizeTimer) clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    const w = Math.max(320, Math.min(7680, window.innerWidth));
    const h = Math.max(240, Math.min(4320, window.innerHeight - 180));
    client?.send({ type: 'SessionResize', width: w, height: h });
  }, 250);
});

canvas.addEventListener('click', () => canvas.focus());
