import { describe, expect, it } from 'vitest';
import { codeToEvdev, mouseButtonToLinux } from '../src/input.js';

describe('evdev mapping', () => {
  it('maps physical codes the same way the desktop client does', () => {
    expect(codeToEvdev('Escape')).toBe(1);
    expect(codeToEvdev('KeyA')).toBe(30);
    expect(codeToEvdev('F11')).toBe(87);
    expect(codeToEvdev('F12')).toBe(88);
    expect(codeToEvdev('MetaLeft')).toBe(125);
    expect(codeToEvdev('NotAKey')).toBeUndefined();
  });

  it('maps mouse buttons to Linux BTN_* codes', () => {
    expect(mouseButtonToLinux(0)).toBe(0x110);
    expect(mouseButtonToLinux(2)).toBe(0x111);
    expect(mouseButtonToLinux(1)).toBe(0x112);
  });
});
