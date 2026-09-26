// Freebuff2API extension options page logic (separate file: MV3 extension page CSP forbids inline scripts)
'use strict';

const portEl = document.getElementById('port');
const apiKeyEl = document.getElementById('apiKey');
const msgEl = document.getElementById('msg');
const saveEl = document.getElementById('save');
const resetEl = document.getElementById('reset');

function setMsg(text, ok) {
  msgEl.style.color = ok ? '#3fb950' : '#f85149';
  msgEl.textContent = text;
}

function getStored(cb) {
  chrome.storage.local.get(['gatewayPort', 'apiKey'], function (r) {
    void chrome.runtime.lastError; // must read this to avoid an unchecked error
    cb(r || {});
  });
}

/** Writes/clears config: a null value means delete that key */
function writeConfig(setObj, removeKeys, cb) {
  chrome.storage.local.set(setObj, function () {
    void chrome.runtime.lastError;
    if (!removeKeys.length) { cb(); return; }
    chrome.storage.local.remove(removeKeys, function () {
      void chrome.runtime.lastError;
      cb();
    });
  });
}

getStored(function (r) {
  if (r.gatewayPort) portEl.value = r.gatewayPort;
  if (r.apiKey) apiKeyEl.value = r.apiKey;
});

saveEl.addEventListener('click', function () {
  const rawPort = portEl.value.trim();
  const rawKey = apiKeyEl.value.trim();

  let portValue = null; // null = left blank = clear the custom port
  if (rawPort !== '') {
    const v = parseInt(rawPort, 10);
    if (!v || v < 1 || v > 65535) {
      setMsg('Invalid port: enter an integer between 1 and 65535', false);
      return;
    }
    portValue = v;
  }
  if (rawKey.length > 512) {
    setMsg('API key is too long (512 characters max)', false);
    return;
  }

  const setObj = {};
  const removeKeys = [];
  if (portValue !== null) setObj.gatewayPort = portValue; else removeKeys.push('gatewayPort');
  if (rawKey) setObj.apiKey = rawKey; else removeKeys.push('apiKey');

  writeConfig(setObj, removeKeys, function () {
    const parts = [];
    parts.push(portValue !== null ? 'port ' + portValue : 'port set back to auto-probe (47821 -> 47822 -> 8787)');
    parts.push(rawKey ? 'API key saved' : 'No API key set (not needed when the gateway has no api_keys configured)');
    setMsg('Saved: ' + parts.join('; '), true);
  });
});

resetEl.addEventListener('click', function () {
  portEl.value = '';
  apiKeyEl.value = '';
  writeConfig({}, ['gatewayPort', 'apiKey'], function () {
    setMsg('Cleared: port reset to auto-probe (47821 -> 47822 -> 8787), no more Authorization header sent', true);
  });
});
