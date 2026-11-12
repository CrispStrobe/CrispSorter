import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import { connectionHealth, monitorConnection } from '../../scripts/mobile_ocr_connection_health.mjs';

function driver() {
  const browser = Object.assign(new EventEmitter(), { isConnected: () => true });
  const page = Object.assign(new EventEmitter(), { isClosed: () => false });
  return { browser, page };
}

test('renderer crash, page closure and CDP loss fail closed without replacing the original page', () => {
  for (const event of ['crash', 'close', 'disconnected']) {
    const { browser, page } = driver(), events = [];
    const monitor = monitorConnection(browser, events, () => 'timestamp');
    monitor.attachPage(page);
    monitor.assertHealthy();
    (event === 'disconnected' ? browser : page).emit(event);
    assert.throws(() => monitor.assertHealthy(), /lost during execution/);
    assert.equal(events[0].phase, 'execution');
    monitor.beginCleanup();
    assert.throws(() => monitor.assertHealthy(), /lost during execution/);
    assert.throws(() => monitor.attachPage(driver().page), /must not replace/);
  }
});

test('intentional driver cleanup is distinguishable from an earlier loss', () => {
  const { browser, page } = driver(), events = [];
  const monitor = monitorConnection(browser, events);
  monitor.attachPage(page);
  monitor.beginCleanup();
  browser.emit('disconnected');
  page.emit('close');
  assert(events.every(event => event.phase === 'cleanup'));
  monitor.assertHealthy();
});

function probes(overrides = {}) {
  const calls = [];
  const replies = { 'get-state': 'device', 'shell pidof test.app': '123',
    'forward --list': 'selected-device tcp:4321 localabstract:webview_devtools_remote_123\nother-device tcp:4444 localabstract:private-other',
    'shell cat /proc/net/unix': 'flags @webview_devtools_remote_123\nflags @unrelated-private-socket',
    ...overrides };
  return { calls, args: { serial: 'selected-device', packageName: 'test.app', pid: '123',
    socket: 'webview_devtools_remote_123', forwardPort: '4321', ...driver(),
    adbText: async args => { calls.push(args); const value = replies[args.join(' ')]; if (value instanceof Error) throw value; return value; },
    runAs: async args => { calls.push(args); return { stdout: 'Name:\ttest.app\nPid:\t123\n' }; },
  } };
}

test('a live original PID does not turn a disconnected driver into a healthy renderer claim', async () => {
  const p = probes();
  p.args.browser.isConnected = () => false;
  p.args.page.isClosed = () => true;
  const health = await connectionHealth(p.args);
  assert.equal(health.original_app_process_present, true);
  assert.equal(health.browser_connected, false);
  assert.equal(health.page_closed, true);
  assert.equal(health.expected_forward_present, true);
  assert.equal(health.expected_socket_present, true);
  assert(!JSON.stringify(health).includes('private-other'));
  assert(!JSON.stringify(health).includes('unrelated-private-socket'));
  assert(p.calls.every(args => !args.some(value => /reconnect|force-stop|start-server|kill-server/.test(value))));
});

test('missing or replaced process/forward/socket are retained as observations, never retried', async () => {
  const p = probes({ 'shell pidof test.app': '456', 'forward --list': '', 'shell cat /proc/net/unix': '' });
  const health = await connectionHealth(p.args);
  assert.equal(health.original_app_process_present, false);
  assert.equal(health.expected_forward_present, false);
  assert.equal(health.expected_socket_present, false);
  assert.deepEqual(health.current_app_pids, ['456']);
  assert.equal(p.calls.length, 5);
});

test('failed diagnostic reads remain unknown rather than inventing process death', async () => {
  const p = probes({ 'shell pidof test.app': new Error('transport unavailable'), 'forward --list': new Error('unavailable') });
  const health = await connectionHealth(p.args);
  assert.equal(health.original_app_process_present, null);
  assert.equal(health.expected_forward_present, null);
  assert.match(health.errors.app_pids, /transport unavailable/);
});

test('unsafe original process identifiers are rejected before any probe', async () => {
  const p = probes();
  p.args.pid = '../other';
  await assert.rejects(connectionHealth(p.args), /verified application PID/);
  assert.equal(p.calls.length, 0);
});
