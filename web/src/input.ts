//! Browser input → Termland messages.
//!
//! Keyboard: `event.code` (layout-independent) maps to evdev scancodes, the
//! same table the desktop client uses. Printable IME output goes as
//! `TextInput`. On blur or when the tab is hidden, every currently-pressed
//! key *and* mouse button is released so a lost key-up or mouse-up cannot
//! leave labwc with a stuck modifier or BTN_LEFT (clicks then do nothing).
//!
//! Pointer: canvas coordinates are scaled to the remote framebuffer.
//! Pointer-lock uses `movementX/Y` with `absolute: false`.

import type { Message } from './messages.js';

const CODE_TO_EVDEV: Record<string, number> = {
  Escape: 1,
  Digit1: 2, Digit2: 3, Digit3: 4, Digit4: 5, Digit5: 6,
  Digit6: 7, Digit7: 8, Digit8: 9, Digit9: 10, Digit0: 11,
  Minus: 12, Equal: 13, Backspace: 14, Tab: 15,
  KeyQ: 16, KeyW: 17, KeyE: 18, KeyR: 19, KeyT: 20, KeyY: 21,
  KeyU: 22, KeyI: 23, KeyO: 24, KeyP: 25,
  BracketLeft: 26, BracketRight: 27, Enter: 28, ControlLeft: 29,
  KeyA: 30, KeyS: 31, KeyD: 32, KeyF: 33, KeyG: 34, KeyH: 35,
  KeyJ: 36, KeyK: 37, KeyL: 38, Semicolon: 39, Quote: 40, Backquote: 41,
  ShiftLeft: 42, Backslash: 43,
  KeyZ: 44, KeyX: 45, KeyC: 46, KeyV: 47, KeyB: 48, KeyN: 49, KeyM: 50,
  Comma: 51, Period: 52, Slash: 53, ShiftRight: 54,
  AltLeft: 56, Space: 57, CapsLock: 58,
  F1: 59, F2: 60, F3: 61, F4: 62, F5: 63, F6: 64,
  F7: 65, F8: 66, F9: 67, F10: 68, F11: 87, F12: 88,
  ControlRight: 97, AltRight: 100,
  Home: 102, ArrowUp: 103, PageUp: 104,
  ArrowLeft: 105, ArrowRight: 106, End: 107, ArrowDown: 108, PageDown: 109,
  Insert: 110, Delete: 111,
  MetaLeft: 125, MetaRight: 126,
};

export function codeToEvdev(code: string): number | undefined {
  return CODE_TO_EVDEV[code];
}

const BTN = { left: 0x110, right: 0x111, middle: 0x112, back: 0x113, forward: 0x114 };

export function mouseButtonToLinux(button: number): number | undefined {
  if (button === 0) return BTN.left;
  if (button === 1) return BTN.middle;
  if (button === 2) return BTN.right;
  if (button === 3) return BTN.back;
  if (button === 4) return BTN.forward;
  return undefined;
}

export class InputCapture {
  private pressed = new Set<number>();
  private buttons = new Set<number>();
  private remoteW = 1;
  private remoteH = 1;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly send: (msg: Message) => void,
  ) {}

  setRemoteSize(w: number, h: number): void {
    this.remoteW = Math.max(1, w);
    this.remoteH = Math.max(1, h);
  }

  attach(): void {
    const el = this.canvas;
    el.tabIndex = 0;
    el.addEventListener('keydown', this.onKeyDown);
    el.addEventListener('keyup', this.onKeyUp);
    el.addEventListener('mousedown', this.onMouseDown);
    el.addEventListener('mouseup', this.onMouseUp);
    el.addEventListener('mousemove', this.onMouseMove);
    el.addEventListener('wheel', this.onWheel, { passive: false });
    el.addEventListener('contextmenu', this.onContextMenu);
    el.addEventListener('blur', this.releaseAll);
    el.addEventListener('compositionend', this.onComposition);
    window.addEventListener('blur', this.releaseAll);
    // mouseup on the canvas is lost if the pointer leaves the tab or the
    // page is hidden mid-click — labwc then keeps BTN_LEFT down and the
    // desktop stops receiving ordinary clicks.
    window.addEventListener('mouseup', this.onWindowMouseUp);
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', this.onVisibility);
    }
  }

  detach(): void {
    const el = this.canvas;
    el.removeEventListener('keydown', this.onKeyDown);
    el.removeEventListener('keyup', this.onKeyUp);
    el.removeEventListener('mousedown', this.onMouseDown);
    el.removeEventListener('mouseup', this.onMouseUp);
    el.removeEventListener('mousemove', this.onMouseMove);
    el.removeEventListener('wheel', this.onWheel);
    el.removeEventListener('contextmenu', this.onContextMenu);
    el.removeEventListener('blur', this.releaseAll);
    el.removeEventListener('compositionend', this.onComposition);
    window.removeEventListener('blur', this.releaseAll);
    window.removeEventListener('mouseup', this.onWindowMouseUp);
    if (typeof document !== 'undefined') {
      document.removeEventListener('visibilitychange', this.onVisibility);
    }
    this.releaseAll();
  }

  private onKeyDown = (e: KeyboardEvent): void => {
    e.preventDefault();
    if (e.repeat) return;
    const scancode = codeToEvdev(e.code);
    if (scancode !== undefined) {
      this.pressed.add(scancode);
      this.send({
        type: 'KeyEvent',
        scancode,
        keysym: 0,
        state: 'Pressed',
        modifiers: 0,
      });
      return;
    }
    // Unmapped physical keys (and some international layouts) still produce a
    // Unicode character. Sending both KeyEvent and TextInput for mapped keys
    // would double-type, so this is only the fallback.
    if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      this.send({ type: 'TextInput', text: e.key });
    }
  };

  private onKeyUp = (e: KeyboardEvent): void => {
    e.preventDefault();
    const scancode = codeToEvdev(e.code);
    if (scancode === undefined) return;
    this.pressed.delete(scancode);
    this.send({
      type: 'KeyEvent',
      scancode,
      keysym: 0,
      state: 'Released',
      modifiers: 0,
    });
  };

  private onComposition = (e: CompositionEvent): void => {
    if (e.data) this.send({ type: 'TextInput', text: e.data });
  };

  private onVisibility = (): void => {
    if (typeof document !== 'undefined' && document.hidden) this.releaseAll();
  };

  private releaseAll = (): void => {
    for (const scancode of this.pressed) {
      this.send({
        type: 'KeyEvent',
        scancode,
        keysym: 0,
        state: 'Released',
        modifiers: 0,
      });
    }
    this.pressed.clear();
    for (const button of this.buttons) {
      this.send({ type: 'MouseButton', button, state: 'Released' });
    }
    this.buttons.clear();
  };

  private scale(e: MouseEvent): { x: number; y: number } {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((e.clientX - rect.left) * this.remoteW) / Math.max(1, rect.width);
    const y = ((e.clientY - rect.top) * this.remoteH) / Math.max(1, rect.height);
    return { x, y };
  }

  private onMouseMove = (e: MouseEvent): void => {
    if (document.pointerLockElement === this.canvas) {
      this.send({
        type: 'MouseMove',
        x: e.movementX,
        y: e.movementY,
        absolute: false,
      });
      return;
    }
    const { x, y } = this.scale(e);
    this.send({ type: 'MouseMove', x, y, absolute: true });
  };

  private onMouseDown = (e: MouseEvent): void => {
    e.preventDefault();
    this.canvas.focus();
    const button = mouseButtonToLinux(e.button);
    if (button === undefined) return;
    this.buttons.add(button);
    this.send({ type: 'MouseButton', button, state: 'Pressed' });
  };

  private onMouseUp = (e: MouseEvent): void => {
    e.preventDefault();
    this.releaseButton(e.button);
  };

  private onWindowMouseUp = (e: MouseEvent): void => {
    this.releaseButton(e.button);
  };

  private releaseButton(domButton: number): void {
    const button = mouseButtonToLinux(domButton);
    if (button === undefined || !this.buttons.has(button)) return;
    this.buttons.delete(button);
    this.send({ type: 'MouseButton', button, state: 'Released' });
  };

  private onWheel = (e: WheelEvent): void => {
    e.preventDefault();
    // Match the desktop client: positive dy is scroll down.
    this.send({ type: 'MouseScroll', dx: e.deltaX, dy: e.deltaY });
  };

  private onContextMenu = (e: Event): void => {
    e.preventDefault();
  };
}
