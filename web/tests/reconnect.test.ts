import { describe, expect, it } from 'vitest';
import { backoffDelay, shouldReconnectAfterHidden } from '../src/client.js';
import { shouldReconfigureDecoder } from '../src/video.js';

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

describe('idle / background tab', () => {
  it('does not tear down the transport for a short tab switch', () => {
    expect(shouldReconnectAfterHidden(5_000)).toBe(false);
    expect(shouldReconnectAfterHidden(59_999)).toBe(false);
  });

  it('reconnects after a minute in the background so a frozen decoder cannot stick', () => {
    expect(shouldReconnectAfterHidden(60_000)).toBe(true);
    expect(shouldReconnectAfterHidden(5 * 60_000)).toBe(true);
  });

  it('rebuilds the VideoDecoder after a few seconds hidden or if it already died', () => {
    expect(shouldReconfigureDecoder(0, 'configured')).toBe(false);
    expect(shouldReconfigureDecoder(4_999, 'configured')).toBe(false);
    expect(shouldReconfigureDecoder(5_000, 'configured')).toBe(true);
    expect(shouldReconfigureDecoder(0, 'closed')).toBe(true);
    expect(shouldReconfigureDecoder(0, null)).toBe(true);
  });
});
