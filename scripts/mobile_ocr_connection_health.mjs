import assert from 'node:assert/strict';

/** Observe loss; never reopen a page, reconnect CDP or retry an app command. */
export function monitorConnection(browser, events, now = () => new Date().toISOString()) {
  let phase = 'execution', page;
  const record = type => { if (events.length < 100) events.push({ at: now(), phase, type }); };
  browser.on('disconnected', () => record('browser-disconnected'));
  return {
    attachPage(value) {
      assert(!page, 'The native smoke must not replace its original WebView page.');
      page = value;
      page.on('close', () => record('page-closed'));
      page.on('crash', () => record('page-crashed'));
    },
    assertHealthy() {
      const loss = events.find(event => event.phase === 'execution');
      assert(!loss, 'Original WebView/CDP lost during execution: ' + loss?.type);
      assert(browser.isConnected(), 'Original CDP connection is closed.');
      assert(!page?.isClosed(), 'Original WebView page is closed.');
    },
    beginCleanup() { phase = 'cleanup'; },
  };
}

/** Read-only observations do not equate a live app PID with a healthy renderer. */
export async function connectionHealth({ serial, packageName, pid, socket, forwardPort, browser, page, adbText, runAs }) {
  assert(/^\d+$/.test(pid), 'Connection health requires the original verified application PID.');
  assert(/^webview_devtools_remote_[A-Za-z0-9_]+$/.test(socket), 'Connection health requires the verified WebView socket.');
  assert(/^\d+$/.test(forwardPort), 'Connection health requires the original ADB forward.');
  const probes = await Promise.allSettled([
    adbText(['get-state']),
    adbText(['shell', 'pidof', packageName]),
    adbText(['forward', '--list']),
    adbText(['shell', 'cat', '/proc/net/unix']),
    runAs(['cat', '/proc/' + pid + '/status']),
  ]);
  const names = ['device_state', 'app_pids', 'forwards', 'sockets', 'original_process_status'];
  const values = {}, errors = {};
  probes.forEach((probe, index) => {
    if (probe.status === 'fulfilled') values[names[index]] = typeof probe.value === 'string' ? probe.value : probe.value.stdout;
    else errors[names[index]] = String(probe.reason).slice(0, 2000);
  });
  const forwarded = values.forwards?.split(/\r?\n/).map(line => line.trim().split(/\s+/))
    .filter(fields => fields[0] === serial && fields[1] === 'tcp:' + forwardPort) ?? null;
  const sockets = values.sockets?.split(/\r?\n/).filter(line => line.trim().endsWith('@' + socket)) ?? null;
  const currentPids = values.app_pids?.trim().split(/\s+/).filter(Boolean) ?? null;
  return {
    device_state: values.device_state?.trim() ?? null,
    original_app_pid: pid, current_app_pids: currentPids,
    original_app_process_present: currentPids === null ? null : currentPids.includes(pid),
    browser_connected: browser?.isConnected() ?? null, page_closed: page?.isClosed() ?? null,
    expected_forward_present: forwarded === null ? null : forwarded.some(fields => fields[2] === 'localabstract:' + socket),
    expected_socket_present: sockets === null ? null : sockets.length > 0,
    original_process_status: values.original_process_status ?? null, errors,
    scope: 'Synthetic-test process/transport observations before driver cleanup; no reconnection, retry or crash diagnosis implied.',
  };
}
