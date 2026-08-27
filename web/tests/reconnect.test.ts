import { describe, expect, it } from 'vitest';
import { backoffDelay } from '../src/client.js';

describe('reconnect backoff', () => {
  it('starts at 1s and doubles until 30s', () => {
    expect(backoffDelay(1)).toBe(1000);
    expect(backoffDelay(2)).toBe(2000);
    expect(backoffDelay(3)).toBe(4000);
    expect(backoffDelay(4)).toBe(8000);
    expect(backoffDelay(5)).toBe(16000);
    expect(backoffDelay(6)).toBe(30000);
    expect(backoffDelay(20)).toBe(30000);
  });
});
