// Freebuff2API panel <-> extension bridge (content script, injected only on 127.0.0.1 / localhost)
// Purpose: announces the extension ID and version to the gateway control panel (/ui) via
// window.postMessage; once the panel has the ID, it can connect directly to the extension via
// chrome.runtime.sendMessage(extensionId, {type:'freebuff2api.import'}).
// Background: the content script and the page share the same window, so postMessage is the
// only communication channel available between them.

(function () {
  'use strict';

  var SOURCE_EXT = 'freebuff2api-extension';
  var SOURCE_PAGE = 'freebuff2api-page';

  function handshake() {
    try {
      // After the extension reloads, the old content script's runtime becomes invalid and
      // chrome.runtime.id throws -- silently ignore it
      window.postMessage(
        {
          source: SOURCE_EXT,
          version: chrome.runtime.getManifest().version,
          id: chrome.runtime.id,
        },
        location.origin
      );
    } catch (e) { /* extension context invalidated, wait for a page refresh */ }
  }

  window.addEventListener('message', function (ev) {
    // Only accept page messages from this window and this origin
    if (ev.source !== window) return;
    if (ev.origin !== location.origin) return;
    var d = ev.data;
    if (!d || typeof d !== 'object') return;
    if (d.source !== SOURCE_PAGE) return;
    // The panel may start listening before the bridge injects, so resend the handshake on every ping
    if (d.type === 'ping') handshake();
  });

  // Broadcast once as early as possible; if the page script hasn't registered its listener yet,
  // the ping mechanism above will retry
  handshake();
})();
