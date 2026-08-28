// Sample Termland page: sidebar chrome, connecting overlay, size fields.
// Edit this file (and index.html) by hand — it is not compiled.
// The reusable protocol client is web/src/ (built to ./dist/).

import { TermlandClient, VideoPipeline, InputCapture } from './dist/index.js';

const SIDE_KEY = 'termland-sidebar-side';
const OFFSET_KEY = 'termland-sidebar-offset';
const DRAG_THRESHOLD = 8;
const MIN_W = 320;
const MAX_W = 7680;
const MIN_H = 240;
const MAX_H = 4320;

const urlEl = document.getElementById('url');
const hashEl = document.getElementById('hash');
const userEl = document.getElementById('user');
const passEl = document.getElementById('pass');
const widthEl = document.getElementById('width');
const heightEl = document.getElementById('height');
const remoteSizeEl = document.getElementById('remote-size');
const out = document.getElementById('out');
const sessionsEl = document.getElementById('sessions');
const canvas = document.getElementById('screen');
const banner = document.getElementById('banner');
const connectingEl = document.getElementById('connecting');
const connectingText = document.getElementById('connecting-text');
const connectForm = document.getElementById('connect-form');
const newBtn = document.getElementById('new-session');
const applySizeBtn = document.getElementById('apply-size');
const lockBtn = document.getElementById('pointer-lock');

let client = null;
let video = null;
let input = null;
let sessionAttached = false;
/** When true, width/height track the browser window until the user edits them. */
let followWindow = true;

function log(s) {
  out.textContent = s;
}

function clamp(n, min, max) {
  return Math.max(min, Math.min(max, n));
}

function windowSize() {
  return {
    width: clamp(Math.round(window.innerWidth), MIN_W, MAX_W),
    height: clamp(Math.round(window.innerHeight), MIN_H, MAX_H),
  };
}

function configuredSize() {
  const fallback = windowSize();
  const w = Number.parseInt(widthEl.value, 10);
  const h = Number.parseInt(heightEl.value, 10);
  return {
    width: Number.isFinite(w) ? clamp(w, MIN_W, MAX_W) : fallback.width,
    height: Number.isFinite(h) ? clamp(h, MIN_H, MAX_H) : fallback.height,
  };
}

function syncSizeFields(width, height) {
  widthEl.value = String(width);
  heightEl.value = String(height);
}

function setRemoteSizeLabel(width, height) {
  const strong = remoteSizeEl.querySelector('strong');
  if (!strong) return;
  strong.textContent = width != null && height != null ? `${width}×${height}` : '—';
}

function setConnecting(message) {
  if (message) {
    connectingText.textContent = message;
    connectingEl.hidden = false;
    connectingEl.classList.add('visible');
    connectingEl.setAttribute('aria-busy', 'true');
    connectingEl.setAttribute('aria-hidden', 'false');
  } else {
    connectingEl.hidden = true;
    connectingEl.classList.remove('visible');
    connectingEl.setAttribute('aria-busy', 'false');
    connectingEl.setAttribute('aria-hidden', 'true');
  }
}

function initSizeFields() {
  const size = windowSize();
  syncSizeFields(size.width, size.height);
  setRemoteSizeLabel(null, null);
  followWindow = true;

  const commitFields = () => {
    followWindow = false;
    const next = configuredSize();
    syncSizeFields(next.width, next.height);
  };

  widthEl.addEventListener('change', commitFields);
  heightEl.addEventListener('change', commitFields);

  window.addEventListener('resize', () => {
    if (!followWindow || sessionAttached) return;
    const next = windowSize();
    syncSizeFields(next.width, next.height);
  });
}

function onEvent(ev) {
  switch (ev.type) {
    case 'status':
      log(ev.message);
      banner.hidden = true;
      break;
    case 'hello':
      setConnecting(null);
      log(
        `Connected to ${ev.server_name}\n  protocol handshake ok\n  auth required: ${ev.auth_required}`,
      );
      newBtn.disabled = false;
      break;
    case 'session-list':
      renderSessions(ev.sessions);
      break;
    case 'session-ready':
      setConnecting(null);
      sessionAttached = true;
      applySizeBtn.disabled = false;
      setRemoteSizeLabel(ev.width, ev.height);
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
      setConnecting(null);
      log(`Session ended: ${ev.reason}`);
      video?.close();
      banner.hidden = true;
      sessionAttached = false;
      applySizeBtn.disabled = true;
      setRemoteSizeLabel(null, null);
      break;
    case 'error':
      setConnecting(null);
      log(`Error: ${ev.error}`);
      break;
  }
}

function renderSessions(sessions) {
  if (sessions.length === 0) {
    sessionsEl.innerHTML = '<p class="note">No resumable sessions.</p>';
    return;
  }
  sessionsEl.innerHTML = sessions
    .map(
      (s) =>
        `<div class="session">` +
        `<code>${s.session_id}</code> ${s.mode} ${s.width}x${s.height} ` +
        `<button type="button" data-attach="${s.session_id}">Attach</button>` +
        `<button type="button" data-close="${s.session_id}">Close</button></div>`,
    )
    .join('');
}

async function connect() {
  client?.stop();
  input?.detach();
  video?.close();
  sessionAttached = false;
  applySizeBtn.disabled = true;
  setRemoteSizeLabel(null, null);
  setConnecting('Connecting…');
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
  try {
    await client.start();
  } catch (e) {
    setConnecting(null);
    log(`Error: ${e instanceof Error ? e.message : String(e)}`);
  }
}

connectForm.addEventListener('submit', (e) => {
  e.preventDefault();
  void connect();
});

newBtn.addEventListener('click', () => {
  followWindow = false;
  const { width, height } = configuredSize();
  syncSizeFields(width, height);
  setConnecting('Starting session…');
  client?.createSession(width, height);
});

applySizeBtn.addEventListener('click', () => {
  if (!sessionAttached || !client) return;
  followWindow = false;
  const { width, height } = configuredSize();
  syncSizeFields(width, height);
  // SessionResize does not emit a fresh SessionReady; treat the requested
  // (clamped) size as current until the server reports otherwise.
  input?.setRemoteSize(width, height);
  setRemoteSizeLabel(width, height);
  client.send({ type: 'SessionResize', width, height });
});

lockBtn.addEventListener('click', () => {
  canvas.requestPointerLock();
});

sessionsEl.addEventListener('click', (e) => {
  const t = e.target;
  if (!(t instanceof HTMLElement)) return;
  const attach = t.getAttribute('data-attach');
  const close = t.getAttribute('data-close');
  if (attach) {
    followWindow = false;
    setConnecting('Starting session…');
    client?.attachSession(attach);
  }
  if (close) client?.closeSession(close);
});

canvas.addEventListener('click', () => canvas.focus());

function initChrome() {
  const chrome = document.getElementById('chrome');
  const tab = document.getElementById('tab');
  const hideBtn = document.getElementById('hide');
  const panel = document.getElementById('panel');

  let side = localStorage.getItem(SIDE_KEY) === 'right' ? 'right' : 'left';
  let offset = Number.parseFloat(localStorage.getItem(OFFSET_KEY) ?? '16');
  if (!Number.isFinite(offset)) offset = 16;

  const apply = () => {
    chrome.classList.toggle('side-left', side === 'left');
    chrome.classList.toggle('side-right', side === 'right');
    chrome.style.setProperty('--offset', `${Math.round(offset)}px`);
  };

  const persist = () => {
    localStorage.setItem(SIDE_KEY, side);
    localStorage.setItem(OFFSET_KEY, String(Math.round(offset)));
  };

  const clampOffset = () => {
    offset = clamp(offset, 0, Math.max(0, window.innerHeight - chrome.offsetHeight));
    chrome.style.setProperty('--offset', `${Math.round(offset)}px`);
  };

  const setOpen = (open) => {
    chrome.classList.toggle('open', open);
    tab.setAttribute('aria-expanded', String(open));
    tab.title = open ? 'Drag to move controls' : 'Termland controls';
    panel.hidden = !open;
    requestAnimationFrame(clampOffset);
  };

  apply();
  setOpen(false);
  requestAnimationFrame(clampOffset);

  hideBtn.addEventListener('click', (e) => {
    e.stopPropagation();
    setOpen(false);
  });

  let dragging = false;
  let moved = false;
  let startX = 0;
  let startY = 0;
  let grabY = 0;

  const onDragMove = (e) => {
    if (!dragging) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!moved && dx * dx + dy * dy < DRAG_THRESHOLD * DRAG_THRESHOLD) return;
    moved = true;
    chrome.classList.add('dragging');
    side = e.clientX < window.innerWidth / 2 ? 'left' : 'right';
    offset = e.clientY - grabY;
    apply();
    clampOffset();
  };

  const onDragEnd = () => {
    if (!dragging) return;
    dragging = false;
    chrome.classList.remove('dragging');
    window.removeEventListener('pointermove', onDragMove);
    window.removeEventListener('pointerup', onDragEnd);
    window.removeEventListener('pointercancel', onDragEnd);
    if (moved) {
      persist();
      return;
    }
    if (!chrome.classList.contains('open')) {
      setOpen(true);
      urlEl.focus();
    }
  };

  // Window-level move/up so drag keeps working after the cursor leaves the tab
  // (setPointerCapture is unreliable under some automation drivers).
  tab.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || dragging) return;
    e.preventDefault();
    dragging = true;
    moved = false;
    startX = e.clientX;
    startY = e.clientY;
    grabY = e.clientY - chrome.getBoundingClientRect().top;
    window.addEventListener('pointermove', onDragMove);
    window.addEventListener('pointerup', onDragEnd);
    window.addEventListener('pointercancel', onDragEnd);
  });

  window.addEventListener('resize', () => {
    clampOffset();
    persist();
  });
}

initSizeFields();
initChrome();
