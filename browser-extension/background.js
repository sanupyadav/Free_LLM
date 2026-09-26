// Freebuff2API one-click login extension (MV3)
//
// Four capabilities:
//   1) Panel -> extension direct connect: the /ui panel triggers an import via
//      chrome.runtime.sendMessage(extensionId, {type:'freebuff2api.import'});
//      the extension ID is announced to the panel by bridge.js (content script) via postMessage.
//   2) Auto-navigate + wait for login: when not logged in, automatically opens freebuff.com,
//      polls the session-token every 2 seconds (up to 180 seconds), and auto-imports as soon as
//      login succeeds.
//   3) Clicking the extension icon = the same flow (import directly if readable, otherwise
//      auto-navigate and wait).
//   4) Multi-port probing: panel port > options-page port > default port list; only a
//      connection-layer failure switches ports — any HTTP response (including 4xx/5xx) stops
//      on that port and reports the error.
//   5) Gateway API key: panel-supplied > saved on the options page; when set, sends an
//      authorization: Bearer header, and reports 401 clearly.
//
// Security boundary: only reads cookies under the freebuff.com domain (the HttpOnly
// session-token is only readable by the extension), only sends to local 127.0.0.1,
// never touches account credentials.

const DEFAULT_PORTS = [47821, 47822, 8787];
const SESSION_COOKIE = '__Secure-next-auth.session-token';
const FREEBUFF_URL = 'https://freebuff.com/';
const COOKIE_URL = 'https://freebuff.com';
const FREEBUFF_TAB_MATCH = 'https://freebuff.com/*';
const POLL_INTERVAL_MS = 2000;
const LOGIN_TIMEOUT_MS = 180000;
const FETCH_TIMEOUT_MS = 8000;
const RESPONSE_GUARD_MS = 1500;

// ---------- Basic utilities ----------

/** System notification; any failure (permission off / missing icon) is silent and doesn't affect the main import flow */
function notify(title, message) {
  try {
    chrome.notifications.create(
      {
        type: 'basic',
        iconUrl: chrome.runtime.getURL('icons/icon128.png'),
        title: String(title || 'Freebuff2API'),
        message: String(message || ''),
      },
      function () { void chrome.runtime.lastError; } // must read this, otherwise the console reports an unchecked error
    );
  } catch (e) { /* silent when notifications are unavailable */ }
}

/** Callback-style chrome API -> Promise: always consumes lastError, returns null on any failure/exception, never throws uncaught */
function chromeCall(invoke) {
  return new Promise(function (resolve) {
    try {
      invoke(function (result) {
        const err = chrome.runtime.lastError;
        resolve(err ? null : result);
      });
    } catch (e) {
      resolve(null);
    }
  });
}

function sleep(ms) {
  return new Promise(function (r) { setTimeout(r, ms); });
}

function normalizePort(v) {
  const n = Number(v);
  return Number.isInteger(n) && n > 0 && n <= 65535 ? n : 0;
}

/** Normalizes the API key: string, strip newlines (prevents header injection), length-limited; returns '' if invalid/empty (meaning no Authorization header is sent) */
function normalizeApiKey(v) {
  if (typeof v !== 'string') return '';
  const s = v.replace(/[\r\n]/g, '').trim();
  return s.length > 0 && s.length <= 512 ? s : '';
}

/** Fallback source: the API key saved on the options page */
async function storedApiKey() {
  const r = await chromeCall(function (cb) {
    chrome.storage.local.get('apiKey', cb);
  });
  return normalizeApiKey(r && r.apiKey);
}

// ---------- Cookie ----------

/** Reads all freebuff.com cookies and joins them into a Cookie header (including HttpOnly) */
async function readFreebuffCookie() {
  const cookies = (await chromeCall(function (cb) {
    chrome.cookies.getAll({ url: COOKIE_URL }, cb);
  })) || [];
  if (cookies.length === 0) return { ok: false, reason: 'no_cookie' };
  const str = cookies
    .filter(function (c) { return c && c.value; })
    .map(function (c) { return c.name + '=' + c.value; })
    .join('; ');
  if (str.indexOf(SESSION_COOKIE) === -1) return { ok: false, reason: 'not_logged_in' };
  return { ok: true, cookie: str, count: cookies.length };
}

/** Whether a login session already exists (millisecond-level probe, used for onMessageExternal's needLogin field) */
async function hasSessionCookie() {
  const c = await chromeCall(function (cb) {
    chrome.cookies.get({ url: COOKIE_URL, name: SESSION_COOKIE }, cb);
  });
  return !!(c && c.value);
}

// ---------- Port probing ----------

/** Candidate ports: panel-supplied > options-page setting > default list, deduplicated */
async function candidatePorts(panelPort) {
  const list = [];
  const push = function (p) {
    const n = normalizePort(p);
    if (n && list.indexOf(n) === -1) list.push(n);
  };
  push(panelPort);
  const stored = await chromeCall(function (cb) {
    chrome.storage.local.get('gatewayPort', cb);
  });
  push(stored && stored.gatewayPort);
  DEFAULT_PORTS.forEach(push);
  return list;
}

// ---------- Import ----------

/**
 * POSTs to the given port.
 * reached=false means only a connection-layer failure (refused/timeout/aborted); that's the
 * only case where the next port should be tried.
 * Any HTTP response (regardless of status code) counts as "reached the gateway".
 */
async function postToGateway(port, cookie, apiKey) {
  const ctrl = new AbortController();
  const timer = setTimeout(function () { ctrl.abort(); }, FETCH_TIMEOUT_MS);
  try {
    const headers = { 'content-type': 'application/json' };
    if (apiKey) headers.authorization = 'Bearer ' + apiKey; // omitted when the gateway has no api_keys configured
    const resp = await fetch('http://127.0.0.1:' + port + '/api/tokens/import', {
      method: 'POST',
      headers: headers,
      body: JSON.stringify({ cookie: cookie }),
      signal: ctrl.signal,
    });
    const text = await resp.text();
    let body = null;
    try { body = JSON.parse(text); } catch (e) { /* non-JSON response */ }
    return { reached: true, httpOk: resp.ok, status: resp.status, body: body };
  } catch (e) {
    return { reached: false, error: String((e && e.message) || e) };
  } finally {
    clearTimeout(timer);
  }
}

async function importCookieToGateway(cookie, panelPort, panelApiKey) {
  const apiKey = normalizeApiKey(panelApiKey) || (await storedApiKey()); // panel takes priority, falls back to options page
  const ports = await candidatePorts(panelPort);
  let lastErr = '';
  for (const port of ports) {
    const r = await postToGateway(port, cookie, apiKey);
    if (!r.reached) { lastErr = r.error || 'connection failed'; continue; } // only try the next port on a connection failure
    if (r.httpOk && r.body && r.body.ok) {
      return { ok: true, port: port, added: Number(r.body.added) || 0, message: r.body.message || '' };
    }
    const msg = (r.body && (r.body.message || r.body.error)) || ('HTTP ' + r.status);
    return { ok: false, port: port, error: String(msg), unauthorized: r.status === 401 }; // got an HTTP response: stop and report on this port
  }
  return { ok: false, port: 0, error: lastErr || 'Failed to connect to the local gateway (connection refused)' };
}

function notifyImportResult(res) {
  if (res.ok) {
    if (res.added > 0) {
      notify(
        '✅ Automatically imported ' + res.added + ' credential(s)',
        'Source: freebuff.com login cookie (gateway port ' + res.port + '). Click "Refresh" on the panel to see the full account list.'
      );
    } else {
      notify('✅ Credential already exists, no need to re-import', 'Deduplicated automatically (gateway port ' + res.port + ').');
    }
    return;
  }
  if (res.unauthorized) {
    notify(
      '🔑 The gateway has API key validation enabled',
      'Enter the key shown on the panel into the extension options page (right-click the extension icon -> "Options" -> Gateway API Key), then retry. Gateway port ' + res.port + '.'
    );
    return;
  }
  if (res.port) {
    notify('Import failed (port ' + res.port + ')', String(res.error).slice(0, 180));
  } else {
    notify(
      'Failed to connect to the local gateway',
      'Please make sure Freebuff2API is running (default port 47821). If you changed the port: right-click the extension icon -> "Options" to set it, ' +
        'or just click "One-click login" on the gateway panel (it will pass along its own port automatically).' +
        (res.error ? ' Last error: ' + String(res.error).slice(0, 120) : '')
    );
  }
}

// ---------- Auto-navigate + wait for login ----------

/** Focuses and reuses an existing freebuff.com tab if present, otherwise opens a new login page */
async function openOrFocusFreebuff() {
  const tabs = await chromeCall(function (cb) {
    chrome.tabs.query({ url: FREEBUFF_TAB_MATCH }, cb);
  });
  if (tabs && tabs.length > 0) {
    const tab = tabs[0];
    const updated = await chromeCall(function (cb) {
      chrome.tabs.update(tab.id, { active: true }, cb);
    });
    if (updated && typeof tab.windowId === 'number' && tab.windowId >= 0) {
      await chromeCall(function (cb) {
        chrome.windows.update(tab.windowId, { focused: true }, cb);
      });
    }
    if (updated) return updated;
  }
  const created = await chromeCall(function (cb) {
    chrome.tabs.create({ url: FREEBUFF_URL }, cb);
  });
  if (!created) {
    notify('Could not open freebuff.com', 'Please manually open https://freebuff.com/ to log in, then click the extension icon or the panel\'s "One-click login" to import.');
  }
  return created;
}

/** Polls the session-token every 2 seconds, up to timeoutMs; returns true as soon as detected */
async function waitForSessionCookie(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const c = await chromeCall(function (cb) {
      chrome.cookies.get({ url: COOKIE_URL, name: SESSION_COOKIE }, cb);
    });
    if (c && c.value) return true;
    await sleep(POLL_INTERVAL_MS);
  }
  return false;
}

// ---------- Main flow (only one runs at a time; repeat triggers reuse the in-flight flow) ----------

let runningFlow = null;
let flowPanelPort = 0; // the in-flight flow reads the latest panel port: usable even if the panel triggers again while waiting for login
let flowApiKey = '';   // same as above: a panel-supplied API key takes priority, and can take effect even if added mid-flow

function startFlow(panelPort, panelApiKey) {
  const p = normalizePort(panelPort);
  if (p) flowPanelPort = p;
  const k = normalizeApiKey(panelApiKey);
  if (k) flowApiKey = k;
  if (runningFlow) return runningFlow;
  runningFlow = (async function () {
    try {
      const r = await readFreebuffCookie();
      if (r.ok) {
        const res = await importCookieToGateway(r.cookie, flowPanelPort, flowApiKey);
        notifyImportResult(res);
        return res;
      }
      // Not logged in: automatically open freebuff.com and wait for login
      notify(
        'Opened freebuff.com, please finish logging in',
        'Once logged in, the credential will be imported into the local gateway automatically, no manual copying needed.'
      );
      await openOrFocusFreebuff();
      const got = await waitForSessionCookie(LOGIN_TIMEOUT_MS);
      if (!got) {
        notify('⏱ Timed out waiting for login', 'Waited 3 minutes but no login credential was detected. After logging in, click the extension icon or the panel\'s "One-click login" again to import immediately.');
        return { ok: false, port: 0, error: 'login_timeout' };
      }
      const after = await readFreebuffCookie();
      if (!after.ok) return { ok: false, port: 0, error: after.reason };
      const res = await importCookieToGateway(after.cookie, flowPanelPort, flowApiKey);
      notifyImportResult(res);
      return res;
    } catch (e) {
      notify('Import flow error', String((e && e.message) || e).slice(0, 180));
      return { ok: false, port: 0, error: 'unexpected_error' };
    } finally {
      // flow ended (including on error): allow the next trigger to start a new flow; never rejects itself
      runningFlow = null;
      flowPanelPort = 0;
      flowApiKey = '';
    }
  })();
  return runningFlow;
}

// ---------- Panel -> extension direct connect ----------

function senderOrigin(sender) {
  try {
    if (sender && sender.origin) return String(sender.origin);
    if (sender && sender.url) return new URL(sender.url).origin;
  } catch (e) { /* ignored */ }
  return '';
}

/** Only allows the local gateway panel (http://127.0.0.1[:port] or http://localhost[:port]) */
function isLocalPanelOrigin(origin) {
  return origin === 'http://127.0.0.1' || origin.indexOf('http://127.0.0.1:') === 0 ||
         origin === 'http://localhost' || origin.indexOf('http://localhost:') === 0;
}

chrome.runtime.onMessageExternal.addListener(function (msg, sender, sendResponse) {
  const origin = senderOrigin(sender);
  if (!isLocalPanelOrigin(origin)) {
    try { sendResponse({ ok: false, error: 'forbidden_origin', message: 'Origin not allowed (only the local gateway panel can use this)' }); } catch (e) { /* channel already closed */ }
    return false;
  }
  if (!msg || msg.type !== 'freebuff2api.import') {
    try { sendResponse({ ok: false, error: 'unknown_type', message: 'Unknown message type' }); } catch (e) { /* channel already closed */ }
    return false;
  }
  const panelPort = normalizePort(msg.gatewayPort);
  const panelApiKey = normalizeApiKey(msg.apiKey);
  (async function () {
    // Wait once for a millisecond-level login-state probe (with a 1.5s safety margin);
    // never wait for the full 3-minute login/import flow.
    const loggedIn = (await Promise.race([
      hasSessionCookie(),
      sleep(RESPONSE_GUARD_MS).then(function () { return null; }),
    ])) === true;
    // Synchronous completion path: if already logged in, run the import to completion so the
    // first response carries done/added and the panel doesn't need to poll.
    // guard = 1.5s login probe + 3s flow fallback: a timeout means the gateway is very slow or
    // multi-port probing is underway; fall back to started and let the panel poll to converge.
    if (loggedIn) {
      let doneRes = null;
      try {
        doneRes = await Promise.race([
          startFlow(panelPort, panelApiKey),
          sleep(RESPONSE_GUARD_MS * 2).then(function () { return null; }),
        ]);
      } catch (e) { /* edge case: fall back to started */ }
      if (doneRes && doneRes.ok) {
        try {
          sendResponse({
            ok: true,
            phase: 'done',
            done: true,
            added: doneRes.added || 0,
            needLogin: false,
          });
        } catch (e) { /* panel already gone */ }
        return;
      }
      // doneRes is null (timeout) or failed: the flow already fired a failure notification;
      // fall back to started here.
      // The flow already started on the synchronous path (finished or failed), so don't call
      // startFlow again (or the port sequence would run twice); the panel's started polling
      // will simply see an unchanged credential count and converge naturally.
      try {
        sendResponse({ ok: true, phase: 'started', needLogin: false });
      } catch (e) { /* panel already gone */ }
      return;
    }
    // Not logged in: respond immediately and start the full flow (auto-navigate -> wait for
    // login -> auto-import)
    try {
      sendResponse({ ok: true, phase: 'started', needLogin: true });
    } catch (e) { /* panel already gone, the flow keeps running */ }
    startFlow(panelPort, panelApiKey);
  })();
  return true; // keep the channel open until sendResponse is called above (milliseconds)
});

// ---------- Click the extension icon: same flow (API key falls back to the options page) ----------

chrome.action.onClicked.addListener(function () {
  startFlow(0, '');
});
