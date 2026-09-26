//! Built-in web control panel (Overview / Accounts / Skills / Logs / Diagnostics / Integration Guide)
//!
//! Lightweight, no build step: single HTML + vanilla JS + CSS, embedded directly as a Rust string.
//! Design principles:
//! - Fixed container + innerHTML rebuild (avoids DOM buildup)
//! - In-tab switching (no more jumping to raw JSON pages)
//! - Empty states have guidance, failures have clear messages (no ambiguity)
//! - SSE live logs + request detail drawer + system diagnostics

pub const INDEX_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Freebuff2API Control Panel</title>
<style>
:root {
  /* Color palette (branded dark theme: primary color #4f8cff family + semantic status colors) */
  --bg:#0d1117; --card:#161b22; --border:#30363d; --text:#e6edf3; --muted:#8b949e;
  --accent:#4f8cff; --accent-soft:#132a4a; --ok:#3fb950; --warn:#d29922; --err:#f85149;
  /* Spacing tokens */
  --sp-1:4px; --sp-2:8px; --sp-3:12px; --sp-4:16px; --sp-5:20px; --sp-6:24px;
  /* Radius tokens */
  --r-sm:6px; --r-md:10px; --r-lg:14px;
  /* Shadow tokens */
  --sh-card:0 1px 2px rgba(0,0,0,.4); --sh-float:0 8px 24px rgba(0,0,0,.5);
  /* Motion tokens */
  --dur-fast:120ms; --dur-norm:200ms; --ease-out:cubic-bezier(.16,1,.3,1);
}
* { box-sizing:border-box; margin:0; padding:0; }
body { background:var(--bg); color:var(--text); font-family:-apple-system,'Segoe UI',Roboto,'Microsoft YaHei',sans-serif; min-height:100vh; }
/* Respect reduced-motion preference */
@media (prefers-reduced-motion: reduce) {
  * { animation:none !important; transition:none !important; }
}
header { display:flex; align-items:center; justify-content:space-between; padding:var(--sp-3) var(--sp-6); border-bottom:1px solid var(--border); background:var(--card); position:sticky; top:0; z-index:10; }
header h1 { font-size:17px; font-weight:600; }
header .dot { display:inline-block; width:8px; height:8px; border-radius:50%; background:var(--muted); margin-right:var(--sp-2); vertical-align:middle; }
header .dot.ok { background:var(--ok); } header .dot.err { background:var(--err); }
/* Memory layer master toggle (toggle switch) */
.switch { position:relative; display:inline-block; width:42px; height:24px; flex:none; }
.switch input { opacity:0; width:0; height:0; }
.switch .slider { position:absolute; cursor:pointer; inset:0; background:#2d333b; border-radius:24px; transition:var(--dur-fast) var(--ease-out); }
.switch .slider::before { content:''; position:absolute; height:18px; width:18px; left:3px; bottom:3px; background:#e6edf3; border-radius:50%; transition:var(--dur-fast) var(--ease-out); }
.switch input:checked + .slider { background:var(--accent); }
.switch input:checked + .slider::before { transform:translateX(18px); }
.hstat { display:flex; gap:var(--sp-4); font-size:12px; color:var(--muted); }
.hstat b { color:var(--text); }
main { max-width:1240px; margin:0 auto; padding:var(--sp-5) var(--sp-6) 60px; }
nav { display:flex; gap:var(--sp-1); margin-bottom:var(--sp-5); border-bottom:1px solid var(--border); flex-wrap:wrap; }
nav button { background:transparent; border:none; color:var(--muted); padding:10px 16px; cursor:pointer; font-size:14px; border-bottom:2px solid transparent; border-radius:0; }
nav button.active { color:var(--text); border-bottom-color:var(--accent); }
nav button:hover { color:var(--text); }
/* Focus visible ring (accessibility) */
button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible, a:focus-visible, [tabindex]:focus-visible { outline:2px solid #79b8ff; outline-offset:2px; }
.sr-only { position:absolute; width:1px; height:1px; padding:0; margin:-1px; overflow:hidden; clip:rect(0 0 0 0); white-space:nowrap; border:0; }
.seg { display:inline-flex; border:1px solid var(--border); border-radius:var(--r-sm); overflow:hidden; }
.seg button { background:transparent; color:var(--muted); padding:6px 12px; border-radius:0; font-size:12px; }
.seg button.active { background:var(--accent-soft); color:var(--text); }
#cred-warn .banner { border-color:var(--warn); background:linear-gradient(135deg,#3a2c0a,#161b22); }
#cred-warn .banner a, #cred-warn .banner button { color:var(--warn); }
.cards { display:grid; grid-template-columns:repeat(auto-fit,minmax(170px,1fr)); gap:14px; margin-bottom:var(--sp-5); }
.card { background:var(--card); border:1px solid var(--border); border-radius:var(--r-md); padding:14px 16px; box-shadow:var(--sh-card); transition:transform var(--dur-fast) var(--ease-out), border-color var(--dur-fast); }
.card:hover { transform:translateY(-1px); border-color:#3a4250; }
.card .num { font-size:26px; font-weight:700; margin-top:var(--sp-1); }
.card .lbl { color:var(--muted); font-size:13px; }
.grid { display:grid; grid-template-columns:1fr 1fr; gap:var(--sp-5); }
@media(max-width:920px){ .grid{grid-template-columns:1fr} }
.panel { background:var(--card); border:1px solid var(--border); border-radius:var(--r-md); padding:var(--sp-4); margin-bottom:var(--sp-5); box-shadow:var(--sh-card); }
.panel h2 { font-size:15px; margin-bottom:var(--sp-3); color:var(--muted); font-weight:600; }
.panel h1 { font-size:18px; margin-bottom:var(--sp-3); }
table { width:100%; border-collapse:collapse; font-size:13px; }
th,td { text-align:left; padding:7px 10px; border-bottom:1px solid var(--border); }
th { color:var(--muted); font-weight:500; }
tr.click { cursor:pointer; } tr.click:hover { background:#1c2129; }
.badge { display:inline-block; padding:2px 8px; border-radius:20px; font-size:12px; }
.badge.ok { background:#1a5e2a; color:var(--ok); }
.badge.warn { background:#5a4a1a; color:var(--warn); }
.badge.err { background:#5e1a1a; color:var(--err); }
.badge.dim { background:#21262d; color:var(--muted); }
.chip { display:inline-block; background:#21262d; border:1px solid var(--border); border-radius:var(--r-sm); padding:2px 8px; margin:2px; font-size:12px; }
button { background:var(--accent); color:#fff; border:none; border-radius:var(--r-sm); padding:6px 14px; cursor:pointer; font-size:13px; transition:filter var(--dur-fast); }
button:hover { filter:brightness(1.12); }
button:disabled { opacity:.5; cursor:not-allowed; }
button.ghost { background:transparent; border:1px solid var(--border); color:var(--text); }
button.ghost:hover { border-color:var(--accent); color:var(--accent); }
button.sm { padding:3px 10px; font-size:12px; }
input,textarea,select { background:#0d1117; border:1px solid var(--border); color:var(--text); border-radius:var(--r-sm); padding:8px 10px; font-size:13px; width:100%; font-family:inherit; transition:border-color var(--dur-fast); }
input:focus,textarea:focus,select:focus { border-color:var(--accent); }
textarea { min-height:90px; resize:vertical; }
label { display:block; color:var(--muted); font-size:12px; margin:var(--sp-2) 0 var(--sp-1); }
.row { display:flex; gap:var(--sp-2); align-items:center; flex-wrap:wrap; }
.empty { color:var(--muted); font-size:13px; padding:14px 4px; }
.empty b { color:var(--text); }
#toast { position:fixed; bottom:24px; left:50%; transform:translateX(-50%); background:var(--card); border:1px solid var(--accent); padding:10px 20px; border-radius:var(--r-md); display:none; z-index:100; font-size:13px; box-shadow:var(--sh-float); animation:toastIn var(--dur-norm) var(--ease-out); }
@keyframes toastIn { from { opacity:0; transform:translate(-50%,8px); } to { opacity:1; transform:translate(-50%,0); } }
.logs { max-height:460px; overflow:auto; font-family:ui-monospace,Consolas,monospace; font-size:12px; background:#0a0d12; border:1px solid var(--border); border-radius:var(--r-md); padding:var(--sp-2); }
.logs div { padding:2px 4px; border-bottom:1px dashed #1c2129; white-space:pre-wrap; word-break:break-all; }
.chat { height:min(62vh,600px); overflow:auto; background:#0a0d12; border:1px solid var(--border); border-radius:var(--r-md); padding:var(--sp-3); display:flex; flex-direction:column; gap:var(--sp-3); }
.chat .empty { margin:auto; color:var(--muted); text-align:center; font-size:13px; }
.msg { max-width:85%; padding:10px 12px; border-radius:var(--r-lg); line-height:1.55; font-size:14px; overflow-wrap:anywhere; }
.msg.user { align-self:flex-end; background:var(--accent-soft); border:1px solid #1f4a80; white-space:pre-wrap; }
.msg.assistant { align-self:flex-start; background:var(--card); border:1px solid var(--border); }
.msg.error { align-self:stretch; max-width:100%; border:1px solid var(--err); color:var(--err); }
.msg .who { font-size:11px; color:var(--muted); margin-bottom:4px; display:flex; align-items:center; gap:8px; }
.msg .who button { margin-left:auto; padding:0 6px; font-size:11px; }
.msg pre { background:var(--bg); border:1px solid var(--border); border-radius:var(--r-sm); padding:8px 10px; overflow:auto; margin:6px 0; white-space:pre; }
.msg code { font-family:ui-monospace,Consolas,monospace; font-size:12.5px; background:var(--bg); padding:1px 4px; border-radius:4px; }
.msg pre code { background:none; padding:0; }
.msg.typing::after { content:'\25CF\25CF\25CF'; letter-spacing:3px; color:var(--muted); animation:typing 1s infinite; }
@keyframes typing { 50% { opacity:.3; } }
@media (prefers-reduced-motion: reduce) { .msg.typing::after { animation:none; } }
.composer { display:flex; gap:8px; align-items:flex-end; margin-top:10px; }
.composer textarea { flex:1; min-height:44px; max-height:200px; resize:none; }
.logs .lv-warn { color:var(--warn); } .logs .lv-error { color:var(--err); } .logs .lv-info { color:var(--muted); }
#drawer { position:fixed; top:0; right:-560px; width:560px; max-width:92vw; height:100vh; background:var(--card); border-left:1px solid var(--border); transition:right var(--dur-norm) var(--ease-out); overflow:auto; padding:var(--sp-5); z-index:50; box-shadow:var(--sh-float); }
#drawer.open { right:0; }
#drawer h3 { margin-bottom:10px; }
.kv { font-size:13px; margin:4px 0; } .kv b { color:var(--muted); font-weight:500; display:inline-block; min-width:110px; }
pre { background:#0a0d12; border:1px solid var(--border); border-radius:var(--r-md); padding:10px; overflow:auto; font-size:12px; }
details { margin:6px 0; } summary { cursor:pointer; color:var(--muted); font-size:13px; }
.banner { background:linear-gradient(135deg,var(--accent-soft),#161b22); border:1px solid var(--accent); border-radius:var(--r-lg); padding:var(--sp-4); margin-bottom:var(--sp-5); }
.banner h2 { color:var(--text); margin-bottom:var(--sp-2); }
.banner ol { margin-left:var(--sp-5); font-size:13px; color:var(--muted); line-height:2; }
.banner code { background:#0a0d12; padding:2px 6px; border-radius:var(--r-sm); }
.doctor-item { display:flex; gap:10px; padding:10px 0; border-bottom:1px solid var(--border); font-size:13px; align-items:flex-start; }
.doctor-item .st { min-width:56px; }
.tok { color:var(--ok); } .twarn { color:var(--warn); } .terr { color:var(--err); }
#login-wizard { scroll-margin-top:70px; }
@keyframes wizardFlash { 0%,100% { box-shadow:0 0 0 0 rgba(79,140,255,0); } 50% { box-shadow:0 0 0 4px rgba(79,140,255,.5); } }
.wizard-flash { animation:wizardFlash .8s ease-in-out 2; }
/* Narrow-screen nav horizontal scroll (v0.8 accessibility) */
@media(max-width:640px){ nav{ flex-wrap:nowrap; overflow-x:auto; } nav button{ flex:none; min-height:44px; } button:not(.sm), input, select, textarea { min-height:44px; } }
</style>
</head>
<body>
<header>
  <h1><span class="dot" id="dot"></span>Freebuff2API Control Panel <span style="color:var(--muted);font-size:12px" id="ver"></span></h1>
  <div class="hstat" id="hstat"></div>
  <input id="api-key-input" type="password" placeholder="API Key (only needed if api_keys is configured)" title="When api_keys is configured, panel requests must include this key; stored only in this browser" style="width:200px;font-size:12px" onchange="setApiKey(this.value)">
</header>
<main>
  <div id="cred-warn" role="status" aria-live="polite"></div>
  <div id="banner"></div>
  <div class="cards" id="cards"></div>
  <div id="cost-line" style="font-size:13px;color:var(--muted);margin:-8px 0 16px 2px"></div>
  <nav role="tablist" aria-label="Panel navigation">
    <button role="tab" id="tab-btn-overview" data-tab="overview" class="active" aria-selected="true" aria-controls="tab-overview" tabindex="0" onclick="showTab('overview')">Overview</button>
    <button role="tab" id="tab-btn-play" data-tab="play" aria-selected="false" aria-controls="tab-play" tabindex="-1" onclick="showTab('play')">Playground</button>
    <button role="tab" id="tab-btn-account" data-tab="account" aria-selected="false" aria-controls="tab-account" tabindex="-1" onclick="showTab('account')">Accounts</button>
    <button role="tab" id="tab-btn-skills" data-tab="skills" aria-selected="false" aria-controls="tab-skills" tabindex="-1" onclick="showTab('skills')">Skills</button>
    <button role="tab" id="tab-btn-memory" data-tab="memory" aria-selected="false" aria-controls="tab-memory" tabindex="-1" onclick="showTab('memory')">Memory</button>
    <button role="tab" id="tab-btn-logs" data-tab="logs" aria-selected="false" aria-controls="tab-logs" tabindex="-1" onclick="showTab('logs')">Live Logs</button>
    <button role="tab" id="tab-btn-teach" data-tab="teach" aria-selected="false" aria-controls="tab-teach" tabindex="-1" onclick="showTab('teach')">How it works</button>
    <button role="tab" id="tab-btn-doctor" data-tab="doctor" aria-selected="false" aria-controls="tab-doctor" tabindex="-1" onclick="showTab('doctor')">Diagnostics</button>
    <button role="tab" id="tab-btn-guide" data-tab="guide" aria-selected="false" aria-controls="tab-guide" tabindex="-1" onclick="showTab('guide')">Integration Guide</button>
    <button role="tab" id="tab-btn-settings" data-tab="settings" aria-selected="false" aria-controls="tab-settings" tabindex="-1" onclick="showTab('settings')">Settings</button>
    <button role="tab" id="tab-btn-about" data-tab="about" aria-selected="false" aria-controls="tab-about" tabindex="-1" onclick="showTab('about')">About</button>
  </nav>

  <section role="tabpanel" aria-labelledby="tab-btn-overview" id="tab-overview" tabindex="0">
    <!-- Start making requests immediately: address + key + one-click copy (answers "what after importing credentials?") -->
    <div class="panel" id="connect-panel">
      <div class="row" style="margin-bottom:10px">
        <h2 style="margin:0">Start making requests now</h2>
        <span style="flex:1"></span>
        <span id="connect-ready" style="font-size:12px;color:var(--muted)"></span>
      </div>
      <div class="grid" style="gap:16px">
        <div>
          <label>Endpoint address (Base URL)</label>
          <div class="row"><input id="c-base" readonly style="flex:1"><button class="ghost sm" onclick="copyText(document.getElementById('c-base').value)">Copy</button></div>
          <label style="margin-top:8px">OpenAI protocol address (Cursor / LobeChat / SDK)</label>
          <div class="row"><input id="c-openai" readonly style="flex:1"><button class="ghost sm" onclick="copyText(document.getElementById('c-openai').value)">Copy</button></div>
          <label style="margin-top:8px">Anthropic protocol address (Claude Code)</label>
          <div class="row"><input id="c-anthropic" readonly style="flex:1"><button class="ghost sm" onclick="copyText(document.getElementById('c-anthropic').value)">Copy</button></div>
        </div>
        <div>
          <label>API Key</label>
          <div class="row"><input id="c-key" readonly style="flex:1"><button class="ghost sm" onclick="copyText(document.getElementById('c-key').value)">Copy</button></div>
          <div class="row" style="margin-top:8px">
            <button class="sm" onclick="genApiKey()">Generate and enable key</button>
            <button class="ghost sm" onclick="clearApiKey()">Clear key</button>
          </div>
          <div id="c-key-hint" style="font-size:12px;color:var(--muted);margin-top:8px"></div>
          <div id="c-model-hint" style="font-size:12px;color:var(--muted);margin-top:6px"></div>
        </div>
      </div>
      <div style="margin-top:12px;font-size:13px;color:var(--muted)">Fill the two fields above into your client to get started -- ready-made config snippets are in the
        <a href="#" onclick="showTab('guide');return false" style="color:var(--accent)">Integration Guide</a>.</div>
    </div>

    <div class="grid">
      <div class="panel"><h2>Account health</h2><div id="acc-wrap"></div></div>
      <div class="panel"><h2>Usage over the last 7 days</h2><div id="daily-wrap"></div></div>
    </div>
    <div class="panel"><h2>Recent requests <span style="font-weight:400">(click a row for details)</span></h2><div id="reqs-wrap"></div></div>
    <div class="panel"><h2>Available models (<span id="model-count">...</span>)</h2><div id="models-wrap"></div></div>
    <div class="panel" id="balance-panel" style="display:none"><h2>Account credits</h2><div id="balance-wrap"></div></div>
    <div class="panel" id="recommend-panel" style="display:none"><h2>Today's recommendations</h2><div id="recommend-wrap"><div class="empty">Loading...</div></div></div>
  </section>

  <section role="tabpanel" aria-labelledby="tab-btn-account" id="tab-account" tabindex="0" style="display:none">
    <!-- Account overview (identity / usage / plan / credits) -->
    <div class="panel">
      <div class="row" style="margin-bottom:10px">
        <h2 style="margin:0">Account overview</h2>
        <span style="flex:1"></span>
        <button class="ghost sm" onclick="refreshAccountOverview()">Refresh</button>
        <button class="ghost sm" onclick="refreshCredential()">Keep-alive check</button>
      </div>
      <div id="overview-wrap"><div class="empty">Click "Refresh" to fetch account info (identity / consecutive usage days / token consumption / plan / credits remaining today)</div></div>
    </div>

    <!-- Credential health dashboard (v0.9 section 2.1) -->
    <div class="panel">
      <div class="row" style="margin-bottom:8px"><h2 style="margin:0">Credential health dashboard</h2><span style="flex:1"></span><button class="ghost sm" onclick="refreshHealth()">Refresh</button></div>
      <div id="health-wrap"><div class="empty">Loading...</div></div>
    </div>

    <!-- Add account (one-click login wizard / paste import) -->
    <div class="panel">
      <h2>Add account</h2>
      <div class="row" style="margin-bottom:10px">
        <button onclick="oneClickLogin()">One-click login</button>
        <span id="ext-status" class="badge dim">Detecting extension...</span>
        <button class="ghost sm" onclick="downloadExtension()">Download extension</button>
        <button class="ghost sm" onclick="pingExtension(true)">Re-detect</button>
      </div>

      <div id="login-wizard" style="display:none;border:1px solid var(--accent);border-radius:8px;padding:14px;margin-bottom:12px;background:linear-gradient(135deg,#132a4a,#161b22)">
        <div class="row" style="margin-bottom:8px"><b>Browser one-click login</b><span style="flex:1"></span><button class="ghost sm" onclick="document.getElementById('login-wizard').style.display='none'">Collapse</button></div>
        <div id="wizard-embed" style="font-size:13px;margin-bottom:10px;padding:8px;background:#0d1117;border-radius:6px">
          <b>Option A (recommended - zero install, zero copying): embedded login window</b> <span class="badge ok">Done once you log in</span>
          <div style="margin:6px 0;color:var(--muted);line-height:1.9">Clicking the button below opens a built-in browser window (the system's WebView2 component) -> log in to GitHub normally inside that window -> the gateway <b>automatically</b> captures the credentials, stores them, and closes the window. No extension install or copying required at all.</div>
          <button onclick="openEmbedLogin()">Open embedded login window</button>
          <span id="embed-status" style="font-size:12px;color:var(--muted);margin-left:8px"></span>
        </div>
        <div id="wizard-clip" style="font-size:13px;margin-bottom:10px;padding:8px;background:#0d1117;border-radius:6px">
          <b>Option B (recommended - no install needed): clipboard auto-detect</b> <span class="badge ok">About 30 seconds</span>
          <div style="margin:6px 0;color:var(--muted);line-height:1.9">Go to freebuff.com and log in -> press <b>F12</b> -> <b>Network</b> -> click any request -> find the line starting with <code>Cookie:</code> under <b>Headers</b> and copy the whole line (Ctrl+C) -> come back and click the button below, the rest happens automatically.</div>
          <button onclick="importFromClipboard()">Auto-detect clipboard</button>
          <span id="clip-status" style="font-size:12px;color:var(--muted);margin-left:8px"></span>
        </div>
        <div id="wizard-ext" style="font-size:13px;margin-bottom:10px;padding:8px;background:#0d1117;border-radius:6px">
          <b>Option C (fully automatic - install once, never think about it again): Chrome / Edge extension</b> <span class="badge dim">About 2 minutes the first time</span>
          <ol style="margin:6px 0 0 20px;color:var(--muted);line-height:1.9">
            <li>Click "Download extension" above to get a zip -> extract it to any directory (or use the <code>browser-extension/</code> directory in the project directly)</li>
            <li>Open <code>chrome://extensions</code> (<code>edge://extensions</code> for Edge) -> turn on "Developer mode" -> "Load unpacked" -> select the directory you just extracted</li>
            <li>Come back to this page and click "Re-detect" -> once the status becomes <span class="badge ok">Ready</span>, click "One-click login" for a <b>fully automatic</b> flow: it opens freebuff.com automatically -> you complete the GitHub login -> credentials are stored automatically (including the HttpOnly cookie, which page JS can't read but the extension can)</li>
          </ol>
        </div>
        <b style="font-size:13px">Manual paste (fallback): <span style="color:var(--muted);font-weight:400">supports cookie string / cURL / HAR</span></b> <span class="badge dim">About 1 minute</span>
        <ol style="margin:6px 0 10px 20px;font-size:13px;color:var(--muted);line-height:2">
          <li><button class="ghost sm" onclick="window.open('https://freebuff.com/','_blank','noopener')">Open freebuff.com and log in</button> (GitHub login works)</li>
          <li>Press <b>F12</b> -> <b>Network</b> -> refresh -> click any request -> find <code>Cookie:</code> under <b>Headers</b> and copy the whole line (or copy the value of <code>__Secure-next-auth.session-token</code> from Application -> Cookies)</li>
          <li>Come back to this page, paste it into the input box below -> click "Import" (validity is verified automatically after import)</li>
        </ol>
        <div style="font-size:12px;color:var(--muted)">Why can't this be fully automatic on the web? The upstream login cookie is marked HttpOnly (browsers block page scripts from reading it) -- the "copy" step in Option B is done by your own hand (Ctrl+C can copy anything, HttpOnly doesn't block the clipboard), the page script only reads the clipboard and imports; the extension can legitimately read the cookie directly; the desktop version is read by the Electron main process, so the desktop version is a fully-automatic one-click tray action.</div>
      </div>

      <textarea id="import-text" placeholder="Paste any of the following:
1) Browser cookie string (including __Secure-next-auth.session-token=...)
2) cURL (bash) command copied from DevTools
3) JSON content from an exported HAR file
Tip: you can also copy the whole Cookie line in DevTools, then use 'Auto-detect clipboard' in the wizard to finish in one step"></textarea>
      <div class="row" style="margin-top:10px">
        <button onclick="doImport()">Import</button>
        <span id="import-result" style="font-size:13px;color:var(--muted)"></span>
      </div>
      <div id="import-verify" style="display:none;margin-top:10px;font-size:13px"></div>
      <details style="margin-top:12px"><summary>How do I get the cookie? (click to expand detailed instructions)</summary>
        <ol style="margin:10px 0 0 20px;font-size:13px;color:var(--muted);line-height:1.9">
          <li>Log in to freebuff.com in your browser</li>
          <li>Press F12 to open DevTools -> Network tab</li>
          <li>Refresh the page, click any request -> Headers -> find the line starting with <code>Cookie:</code></li>
          <li>Copy the whole line, paste it into the box above, click "Import"</li>
          <li>You can also copy items individually from Application -> Cookies -> https://freebuff.com (must include <code>__Secure-next-auth.session-token</code>)</li>
        </ol>
      </details>
    </div>

    <!-- Credential list -->
    <div class="panel">
      <h2>Stored credentials <span id="cred-count" style="font-weight:400;color:var(--muted);font-size:12px"></span></h2>
      <div id="tokens-wrap"></div>
    </div>

    <!-- Usage history (quota/consumption snapshot per account, browsable history) -->
    <div class="panel">
      <div class="row" style="margin-bottom:10px">
        <h2 style="margin:0">Usage history</h2>
        <span style="flex:1"></span>
        <select id="hist-cred" style="width:auto" onchange="loadHistory()"><option value="">All accounts</option></select>
        <button class="ghost sm" onclick="loadHistory()">Refresh</button>
      </div>
      <div id="hist-wrap"><div class="empty">Every "Refresh account overview" or "Check" records an entry -- click "Refresh" to view</div></div>
    </div>
  </section>

  <section role="tabpanel" aria-labelledby="tab-btn-skills" id="tab-skills" tabindex="0" style="display:none">
    <div class="panel">
      <h2>Skill library <span style="font-weight:400;color:var(--muted);font-size:12px">(when enabled, injected as a system prefix into the conversation; roster mode only injects name and description)</span></h2>
      <div class="row" style="margin-bottom:10px">
        <button onclick="newSkill()">+ New skill</button>
        <span id="roster-info" style="font-size:12px;color:var(--muted)"></span>
      </div>
      <div id="skills-wrap"></div>
    </div>
    <div class="panel" id="skill-editor" style="display:none">
      <h2 id="skill-editor-title">Edit skill</h2>
      <label>Name</label><input id="sk-name" placeholder="e.g. Weekly report assistant">
      <label>Description (trigger instructions for the AI: when to use this skill)</label><input id="sk-desc" placeholder="Use when the user wants to write a weekly report...">
      <label>Instruction body (Markdown)</label><textarea id="sk-body" style="min-height:200px" placeholder="Full instruction content for the skill..."></textarea>
      <div class="row" style="margin-top:10px">
        <button onclick="saveSkill()">Save</button>
        <button class="ghost" onclick="document.getElementById('skill-editor').style.display='none'">Cancel</button>
        <span id="skill-save-result" style="font-size:13px;color:var(--muted)"></span>
      </div>
      <details style="margin-top:10px"><summary>Quality gate check (flags issues before saving)</summary><div id="gate-result" style="font-size:13px;color:var(--muted);margin-top:6px"></div></details>
    </div>
  </section>

  <section role="tabpanel" aria-labelledby="tab-btn-memory" id="tab-memory" tabindex="0" style="display:none">
    <div class="panel">
      <h2>Memory store <span style="font-weight:400;color:var(--muted);font-size:12px">(the AI learns your preferences and corrections from here; zero-LLM rule-based recording, fully local)</span></h2>
      <div id="mem-toggle-row" style="display:flex;align-items:center;gap:10px;margin-bottom:10px;padding:10px 12px;border:1px solid var(--border);border-radius:8px;background:#161b22">
        <div style="flex:1">
          <div style="font-size:14px;font-weight:600">Memory layer <span id="mem-toggle-state" style="font-size:12px;font-weight:400;color:var(--muted)"></span></div>
          <div style="font-size:12px;color:var(--muted);margin-top:2px">When enabled, automatically records frequently used models / reasoning effort / your corrections, and injects them into relevant conversations (disabled by default, suitable for users who don't need memory)</div>
        </div>
        <label class="switch" title="Memory layer master toggle (written back to config.json, takes effect immediately, no restart needed)">
          <input type="checkbox" id="mem-toggle" onchange="toggleMemoryEnabled()">
          <span class="slider"></span>
        </label>
      </div>
      <div id="mem-stats" style="margin-bottom:10px;font-size:13px;color:var(--muted)"></div>
      <div class="row" style="margin-bottom:10px">
        <button onclick="newMemory()">+ Add manually</button>
        <span style="font-size:12px;color:var(--muted)">Auto-recorded: frequently used models / reasoning effort downgrades / your corrections ("remember...", "don't...again", "always/never")</span>
      </div>
      <div id="mem-editor" style="display:none;border:1px solid var(--border);border-radius:8px;padding:12px;margin-bottom:12px">
        <label>Type</label>
        <select id="mem-kind">
          <option value="preference">Preference</option><option value="correction">Correction</option>
          <option value="habit">Habit</option><option value="project">Project</option><option value="feedback">Feedback</option>
        </select>
        <label>Title</label><input id="mem-title" placeholder="e.g. Prefer Chinese answers / frequently used model glm-5.3-flash">
        <label>Content</label><textarea id="mem-content" style="min-height:80px" placeholder="Specific content (injected as a low-authority system prefix during relevant conversations)"></textarea>
        <div class="row" style="margin-top:10px">
          <button onclick="saveMemory()">Save</button>
          <button class="ghost" onclick="document.getElementById('mem-editor').style.display='none'">Cancel</button>
          <span id="mem-save-result" style="font-size:13px;color:var(--muted)"></span>
        </div>
      </div>
      <div id="mem-wrap"></div>
    </div>
  </section>

  <section role="tabpanel" aria-labelledby="tab-btn-teach" id="tab-teach" tabindex="0" style="display:none">
    <div class="panel">
      <h2>How it works at a glance <span style="font-weight:400;color:var(--muted);font-size:12px">(what happens behind this gateway)</span></h2>
      <details open><summary><b>1. After a request comes in</b></summary>
        <p style="font-size:13px;color:var(--muted);line-height:1.9;margin-top:6px">
        The client (Claude Code / Cursor) sends an OpenAI- or Claude-format request to the local port <code>47821</code>.
        The gateway first parses the model name -> picks a healthy account from the account pool (score + circuit-breaker state) -> ensures that account has an active session upstream
        -> rewrites the request into upstream format (injects run metadata, adjusts the reasoning effort per model, appends prompts/skills/memory)
        -> forwards it upstream, then streams the response back in real time in the client's format.</p></details>
      <details><summary><b>2. How multi-account "rotation" works</b></summary>
        <p style="font-size:13px;color:var(--muted);line-height:1.9;margin-top:6px">
        Each account has its own health score and circuit breaker (three states: Closed / Open / HalfOpen): 4 consecutive failures trips it automatically,
        the cooldown grows exponentially with trip count (capped at 10 minutes); once cooldown ends it enters half-open state to allow probe requests, and recovers after 2 consecutive successes.
        A failed request automatically retries with a different account (up to 3 times) -- but only retries before any bytes have been written to the client,
        it will never write you a half-finished response.</p></details>
      <details><summary><b>3. How prompts / skills / memory get injected</b></summary>
        <p style="font-size:13px;color:var(--muted);line-height:1.9;margin-top:6px">
        Injection order: base prompt -> enabled prompts -> skill roster (name + description) -> memory block (low authority),
        concatenated into one system message placed at the front. Skills and memory have strict budgets (2000 / 512 tokens by default); anything over budget is dropped entirely,
        to avoid "the more you add, the more it costs". Memory is retrieved based on your current question (local trigram full-text index, supports Chinese).</p></details>
      <details><summary><b>4. Black box: why was this slow / why did it fail</b></summary>
        <p style="font-size:13px;color:var(--muted);line-height:1.9;margin-top:6px">
        Every request is recorded: routing decision (requested model -> actual model -> account), upstream status code, time to first byte, total duration, token usage,
        error type and snippet. Click any row under "Overview -> Recent requests" to see details and a plain-language explanation; the "Live Logs" page streams
        what's happening in real time (auto-resumes after disconnects).</p></details>
      <details><summary><b>5. Free quota and ad-based keep-alive</b></summary>
        <p style="font-size:13px;color:var(--muted);line-height:1.9;margin-top:6px">
        The upstream free tier maintains quota through "session + ad refresh": the gateway sends a heartbeat every 45 seconds, and triggers an ad refresh to extend the session when it's running low.
        Seeing <code>waiting_room_queued</code> means upstream is queuing -- not a gateway failure; wait and retry, or add more accounts to increase concurrency.</p></details>
      <details><summary><b>6. Automatic upstream session cleanup</b></summary>
        <p style="font-size:13px;color:var(--muted);line-height:1.9;margin-top:6px">
        Every conversation generates a thread upstream. The gateway records the threads it creates itself, and cleans up sessions older than
        <b>24 hours</b> every <b>hour</b> (<code class="ok">thread_cleanup_interval_sec</code> /
        <code class="ok">thread_max_age_hours</code> are configurable; set the interval to 0 to disable) -- this avoids the proxy piling up long-lived sessions and putting pressure on upstream.
        You can also dry-run this manually via the API described on the "How it works" page: <code>POST /api/threads/cleanup {"dry_run":true}</code>.</p></details>
      <details><summary><b>7. Where is the data stored</b></summary>
        <p style="font-size:13px;color:var(--muted);line-height:1.9;margin-top:6px">
        All local: <code>data/freebuff2api.sqlite</code> (usage), <code>data/telemetry.sqlite</code> (request details),
        <code>data/memory.sqlite</code> (memory), <code>data/skills/</code> (skill Markdown, source of truth),
        <code>data/tokens.json</code> (credentials). Back up or delete entirely to reset.</p></details>
    </div>
  </section>

  <section role="tabpanel" aria-labelledby="tab-btn-logs" id="tab-logs" tabindex="0" style="display:none">
    <div class="panel">
      <div class="row" style="margin-bottom:10px">
        <h2 style="margin:0">Live logs <span id="log-err-count" class="badge err" style="display:none" title="Number of error-level log entries currently buffered">0</span></h2>
        <span style="flex:1"></span>
        <span role="status" aria-live="polite" class="sr-only" id="log-sr"></span>
        <div class="seg" id="log-level-seg" role="group" aria-label="Log level filter">
          <button class="active" data-level="" onclick="setLogLevel(this)">All</button>
          <button data-level="info" onclick="setLogLevel(this)">info</button>
          <button data-level="warn" onclick="setLogLevel(this)">warn</button>
          <button data-level="error" onclick="setLogLevel(this)">error</button>
        </div>
        <select id="log-filter" style="width:auto" onchange="renderLogs()" aria-label="Log level selector">
          <option value="">All levels</option><option value="info">info</option><option value="warn">warn</option><option value="error">error</option>
        </select>
        <button class="ghost sm" id="log-pause-btn" onclick="toggleLogPause()">Pause scrolling</button>
        <button class="ghost sm" onclick="exportLogs()">Export current</button>
        <button class="ghost sm" onclick="clearLogs()">Clear display</button>
      </div>
      <div class="logs" id="logbox" aria-live="polite" aria-relevant="additions"><div class="empty">Waiting for logs... (start a conversation to see the request flow)</div></div>
    </div>
  </section>

  <section role="tabpanel" aria-labelledby="tab-btn-doctor" id="tab-doctor" tabindex="0" style="display:none">
    <div class="panel">
      <h2>System diagnostics <span style="font-weight:400;color:var(--muted);font-size:12px">(results are signals, not verdicts; "not checked" just means not checked)</span></h2>
      <button class="ghost sm" onclick="refreshDoctor()">Re-check</button>
      <div id="doctor-wrap" style="margin-top:10px"><div class="empty">Click "Re-check" to start</div></div>
    </div>
  </section>

  <section role="tabpanel" aria-labelledby="tab-btn-guide" id="tab-guide" tabindex="0" style="display:none">
    <div class="panel">
      <h2>Connect this gateway to your AI client</h2>
      <div style="background:#0d1117;border:1px solid var(--border);border-radius:8px;padding:12px;margin:10px 0 16px;font-size:13px;line-height:2">
        <b>Three steps:</b>
        1. Import credentials on the "Accounts" page (or one-click login) ->
        2. Confirm the status light above is green (gateway running) ->
        3. Configure your client using any of the methods below to start chatting.
      </div>
      <p style="font-size:13px;color:var(--muted);margin-bottom:10px">Gateway address: <code id="guide-base">http://127.0.0.1:47821</code> (append <code>/v1</code> for the OpenAI protocol; not needed for the Anthropic protocol)</p>
      <p style="font-size:13px;color:var(--muted);margin-bottom:10px"><b>What do I put for API Key?</b> <span id="guide-key-hint">If api_keys isn't configured in config.json, any string works (e.g. <code>sk-local</code>)</span></p>

      <h2 style="margin-top:16px">Claude Code (Anthropic protocol)</h2>
      <pre id="g-claude"></pre><button class="ghost sm" onclick="copyText(document.getElementById('g-claude').textContent)">Copy</button>

      <h2 style="margin-top:16px">Cursor / Continue / generic OpenAI client</h2>
      <pre id="g-openai"></pre><button class="ghost sm" onclick="copyText(document.getElementById('g-openai').textContent)">Copy</button>

      <h2 style="margin-top:16px">OpenAI SDK (Python)</h2>
      <pre id="g-py"></pre><button class="ghost sm" onclick="copyText(document.getElementById('g-py').textContent)">Copy</button>

      <h2 style="margin-top:16px">OpenAI SDK (Node.js)</h2>
      <pre id="g-node"></pre><button class="ghost sm" onclick="copyText(document.getElementById('g-node').textContent)">Copy</button>

      <h2 style="margin-top:16px">curl quick test</h2>
      <pre id="g-curl"></pre><button class="ghost sm" onclick="copyText(document.getElementById('g-curl').textContent)">Copy</button>

      <h2 style="margin-top:16px">LobeChat / NextChat / Cherry Studio</h2>
      <pre id="g-lobe"></pre><button class="ghost sm" onclick="copyText(document.getElementById('g-lobe').textContent)">Copy</button>
    </div>
  </section>

  <!-- Chat playground (added in v0.8 / v0.9: multi-turn + images + effort) -->
  <section role="tabpanel" aria-labelledby="tab-btn-play" id="tab-play" tabindex="0" style="display:none">
    <div class="panel">
      <h2>Chat playground</h2>
      <div class="row" style="margin-bottom:10px;flex-wrap:wrap;gap:8px">
        <select id="play-model" style="max-width:280px;flex:1;min-width:160px" aria-label="Model"></select>
        <select id="play-effort" style="max-width:180px;display:none" title="Reasoning effort (tiered per model)" aria-label="Reasoning effort"></select>
        <button class="ghost sm" onclick="loadModelsIntoPlay()">Refresh models</button>
        <span style="flex:1"></span>
        <button class="ghost sm" onclick="playNewSession()">New chat</button>
        <button class="ghost sm" onclick="playCopyOut()">Copy last reply</button>
        <button class="ghost sm" onclick="playExportMd()">Export Markdown</button>
      </div>
      <details style="margin-bottom:10px"><summary style="cursor:pointer;font-size:13px;color:var(--muted)">System prompt (optional)</summary>
        <textarea id="play-system" placeholder="e.g.: You are a senior Rust engineer" style="min-height:40px;margin-top:6px"></textarea>
      </details>
      <div id="play-output" class="chat" aria-live="polite" ondragover="event.preventDefault()" ondrop="playDrop(event)"><div class="empty">Send a message to start chatting.<br>Multi-turn context is kept on this page.</div></div>
      <div id="play-images" class="row" style="gap:6px;margin:8px 0 0"></div>
      <div id="play-drop" class="composer" ondragover="event.preventDefault()" ondrop="playDrop(event)">
        <button class="ghost" title="Attach images (or paste / drag them here)" aria-label="Attach images" onclick="document.getElementById('play-file').click()">&#128206;</button>
        <input type="file" id="play-file" accept="image/*" multiple style="display:none" onchange="playAddFiles(this.files)">
        <textarea id="play-input" rows="1" placeholder="Message the model..." aria-label="Message" onkeydown="if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();playSend();}" oninput="this.style.height='auto';this.style.height=Math.min(this.scrollHeight,200)+'px'" onpaste="playPaste(event)"></textarea>
        <button onclick="playSend()">Send</button>
        <button id="play-stop" class="ghost" onclick="playStop()" style="display:none">Stop</button>
      </div>
      <div class="row" style="margin-top:6px;font-size:12px;color:var(--muted)"><span id="play-status"></span><span style="flex:1"></span><span>Enter to send &middot; Shift+Enter for a new line &middot; paste or drag images</span></div>
    </div>
  </section>

  </section>

  <!-- Settings page (added in v0.8) -->
  <section role="tabpanel" aria-labelledby="tab-btn-settings" id="tab-settings" tabindex="0" style="display:none">
    <div class="panel">
      <h2>Settings</h2>
      <p style="font-size:13px;color:var(--muted);margin-bottom:12px">Changes are written back to <code>config.json</code> (atomic write, doesn't overwrite other settings). <b>The listen address and some other settings require a restart to take effect.</b></p>
      <div id="settings-wrap"><div class="empty">Loading...</div></div>
    </div>
      <h2>Data migration (export / import)</h2>
      <h2>Data migration (export / import)</h2>
      <p style="font-size:13px;color:var(--muted);margin-bottom:10px">One click packages config (sanitized, no plaintext api_keys/auth_tokens) + credentials + skill enable states + memory toggle, for migrating to a new machine. Automatically backed up to <code>data/backup-&lt;timestamp&gt;/</code> before import.</p>
      <div class="row" style="gap:8px;flex-wrap:wrap">
        <button onclick="exportConfig()">Export config</button>
        <button onclick="document.getElementById('import-file').click()">Import config</button>
        <input type="file" id="import-file" accept=".json,application/json" style="display:none" onchange="importConfig(this.files)">
        <span id="migrate-status" style="font-size:12px;color:var(--muted)"></span>
      </div>
    </div>
  </section>

  <!-- About page (added in v0.8) -->
  <section role="tabpanel" aria-labelledby="tab-btn-about" id="tab-about" tabindex="0" style="display:none">
    <div class="panel">
      <h2>ℹ️ About Freebuff2API</h2>
      <div id="about-wrap"><div class="empty">Loading…</div></div>
    </div>
  </section>
</main>

<div id="drawer"><div class="row"><h3 id="dr-title">Request Details</h3><span style="flex:1"></span><button class="ghost sm" onclick="closeDrawer()">Close</button></div><div id="dr-body"></div></div>
<div id="toast"></div>
<script>
// ---------- Basic utilities ----------
const $ = (id) => document.getElementById(id);
// Optional API Key (when api_keys is configured, panel requests must include Authorization)
function apiKey() { try { return localStorage.getItem('freebuff_api_key') || ''; } catch (e) { return ''; } }
function setApiKey(v) {
  try {
    localStorage.setItem('freebuff_api_key', v.trim());
    toast(v.trim() ? 'API Key saved, reloading…' : 'API Key cleared, reloading…');
    authWarned = false;
    loadGuide();
    refreshOverview();
  } catch (e) {}
}
async function api(url, opt) {
  opt = opt || {};
  opt.headers = Object.assign({}, opt.headers || {});
  const k = apiKey();
  if (k) opt.headers['authorization'] = 'Bearer ' + k;
  const r = await fetch(url, opt);
  const text = await r.text();
  if (!r.ok) { let m = text; try { m = JSON.parse(text).message || JSON.parse(text).error?.message || text; } catch (e) {} throw new Error(m); }
  try { return JSON.parse(text); } catch (e) { return text; }
}
function toast(msg, ms) { const t = $('toast'); t.textContent = msg; t.style.display = 'block'; clearTimeout(t._h); t._h = setTimeout(() => t.style.display = 'none', ms || 2600); }
function esc(s) { return String(s == null ? '' : s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }
function badge(text, cls) { return `<span class="badge ${cls || 'dim'}">${esc(text)}</span>`; }
function copyText(t) { navigator.clipboard?.writeText(t).then(() => toast('Copied')).catch(() => toast('Copy failed, please select manually')); }
function fmtTime(ts) { try { return new Date(ts).toLocaleTimeString('en-GB', { hour12: false }); } catch (e) { return ts || '—'; } }

// ---------- Tab ----------
function showTab(name) {
  document.querySelectorAll('nav button').forEach(b => { const on = b.dataset.tab === name; b.classList.toggle('active', on); b.setAttribute('aria-selected', on ? 'true' : 'false'); b.setAttribute('tabindex', on ? '0' : '-1'); });
  for (const t of ['overview','play','account','skills','memory','logs','teach','doctor','guide','settings','about']) {
    const el = $('tab-' + t); if (el) el.style.display = (t === name) ? '' : 'none';
  }
  if (name === 'skills') refreshSkills();
  if (name === 'memory') refreshMemory();
  if (name === 'doctor') refreshDoctor();
  if (name === 'account') { pingExtension(); refreshTokens(); loadHistory(); refreshHealth(); }
  if (name === 'overview') { loadBalance(); loadRecommend(); loadGuide(); }
  if (name === 'logs') initLogs();
  if (name === 'guide') loadGuide();
  if (name === 'play') loadModelsIntoPlay();
  if (name === 'settings') loadSettings();
  if (name === 'about') loadAbout();
}

// ---------- Keyboard tab navigation (accessibility) ----------
function initTabKeyboard() {
  const nav = document.querySelector('nav[role="tablist"]');
  if (!nav) return;
  nav.addEventListener('keydown', (e) => {
    const keys = ['ArrowLeft', 'ArrowRight', 'Home', 'End'];
    if (!keys.includes(e.key)) return;
    const tabs = Array.from(nav.querySelectorAll('button[role="tab"]'));
    const idx = tabs.indexOf(document.activeElement);
    if (idx < 0) return;
    e.preventDefault();
    let ni = idx;
    if (e.key === 'ArrowLeft') ni = (idx - 1 + tabs.length) % tabs.length;
    else if (e.key === 'ArrowRight') ni = (idx + 1) % tabs.length;
    else if (e.key === 'Home') ni = 0;
    else if (e.key === 'End') ni = tabs.length - 1;
    const t = tabs[ni];
    t.focus();
    showTab(t.dataset.tab);
  });
}

// ---------- Chat playground (v0.8; v0.9: multi-turn + images + effort + copy/export) ----------
let playAbort = null;
let playHistory = [];      // [{role:'user'|'assistant', content}]
let playImages = [];       // {name, dataUrl}
const playModelMeta = {};  // id -> /v1/models meta

const PLAY_IMG_TYPES = new Set(['image/jpeg', 'image/png', 'image/gif', 'image/webp']);
function playAddFiles(files, fromPaste) {
  for (const f of Array.from(files || [])) {
    if (!f) continue;
    if (!f.size) { toast('Blocked empty file: ' + (f.name || 'unknown file'), 3000); continue; }
    if (!PLAY_IMG_TYPES.has(String(f.type || '').toLowerCase())) {
      const msg = 'Only JPEG/PNG/GIF/WebP images supported: ' + (f.name || 'unknown file') + ' (' + (f.type || 'unknown type') + ')';
      if (!fromPaste) toast(msg, 3500); continue;
    }
    if (f.size > 20 * 1024 * 1024) { toast('Image exceeds 20MB limit, skipped: ' + f.name, 3500); continue; }
    const reader = new FileReader();
    reader.onload = () => { playImages.push({ name: f.name || 'paste-' + playImages.length + '.png', dataUrl: String(reader.result) }); renderPlayImages(); };
    reader.readAsDataURL(f);
  }
}
function playDrop(ev) { ev.preventDefault(); playAddFiles(ev.dataTransfer && ev.dataTransfer.files); }
function playPaste(ev) {
  const files = ev.clipboardData && ev.clipboardData.files;
  if (files && files.length) { ev.preventDefault(); playAddFiles(files, true); }
}
function renderPlayImages() {
  const w = $('play-images'); if (!w) return;
  if (!playImages.length) { w.innerHTML = ''; return; }
  w.innerHTML = playImages.map((im, i) =>
    `<span style="position:relative;display:inline-block"><img src="${esc(im.dataUrl)}" style="width:56px;height:56px;border-radius:8px;object-fit:cover;border:1px solid var(--border)" alt=""><button class="sm" style="position:absolute;top:-8px;right:-8px;padding:1px 6px" onclick="playRemoveImage(${i})">✕</button></span>`
  ).join('') + `<span style="font-size:12px;color:var(--muted)">${playImages.length} image(s)</span>`;
}
function playRemoveImage(i) { playImages.splice(i, 1); renderPlayImages(); }
async function loadModelsIntoPlay() {
  const sel = $('play-model'); if (!sel) return;
  try {
    const m = await api('/v1/models');
    const list = (m && m.data || []).map(x => x.id);
    if (Array.isArray(m.meta)) m.meta.forEach(mm => { if (mm && mm.id) playModelMeta[mm.id] = mm; });
    const cur = sel.value;
    sel.innerHTML = list.length ? list.map(id => `<option value="${esc(id)}" ${id===cur?'selected':''}>${esc(id)}</option>`).join('')
      : '<option value="">(no models)</option>';
    if (!list.includes(cur)) sel.value = list[0] || '';
    renderPlayEffort();
  } catch (e) { sel.innerHTML = `<option value="z-ai/glm-5.3-flash">z-ai/glm-5.3-flash (failed to load, using default)</option>`; }
}
function renderPlayEffort() {
  const sel = $('play-effort'); if (!sel) return;
  const model = $('play-model') ? $('play-model').value : '';
  const meta = playModelMeta[model];
  const efforts = meta && Array.isArray(meta.efforts) && meta.efforts.length ? meta.efforts : null;
  const cur = sel.value;
  sel.innerHTML = efforts ? ['<option value="">Reasoning effort (default)</option>'].concat(efforts.map(e => `<option value="${esc(e)}">${esc(e)}</option>`)).join('') : '';
  sel.style.display = efforts ? '' : 'none';
  if (cur && efforts && efforts.includes(cur)) sel.value = cur;
  if ($('play-model')) $('play-model').onchange = renderPlayEffort;
}
// Minimal markdown: fenced code blocks, inline code, bold. Input is escaped first, so it stays XSS-safe.
function mdLite(src) {
  return String(src).split('```').map((part, i) => {
    if (i % 2) {
      const nl = part.indexOf('\n');
      const code = nl >= 0 && /^[\w+#.-]*$/.test(part.slice(0, nl).trim()) ? part.slice(nl + 1) : part;
      return '<pre><code>' + esc(code.replace(/\n$/, '')) + '</code></pre>';
    }
    // blank lines next to a code block would add big gaps (the <pre> already has margins)
    return esc(part.replace(/^\n+|\n+$/g, '')).replace(/`([^`\n]+)`/g, '<code>$1</code>').replace(/\*\*([^*\n]+)\*\*/g, '<b>$1</b>').replace(/\n/g, '<br>');
  }).join('');
}
function msgHtml(role, content, idx) {
  if (role === 'user') return `<div class="msg user">${esc(content)}</div>`;
  const copy = idx == null ? '' : `<button class="ghost sm" onclick="playCopyMsg(${idx})">Copy</button>`;
  return `<div class="msg assistant"><div class="who">${esc($('play-model') ? $('play-model').value : 'assistant')}${copy}</div>${mdLite(content)}</div>`;
}
function renderConversation() {
  return playHistory.map((h, i) => msgHtml(h.role, h.content, i)).join('') || '<div class="empty">Send a message to start chatting.<br>Multi-turn context is kept on this page.</div>';
}
function playCopyMsg(i) {
  const h = playHistory[i]; if (!h) return;
  navigator.clipboard?.writeText(h.content).then(() => toast('Copied')).catch(() => toast('Copy failed, please select manually'));
}
function playBusy(on) {
  const send = document.querySelector('#tab-play button[onclick="playSend()"]');
  if (send) send.style.display = on ? 'none' : '';
  if ($('play-stop')) $('play-stop').style.display = on ? '' : 'none';
}
async function playSend() {
  const out = $('play-output'); if (!out) return;
  const model = $('play-model').value || 'z-ai/glm-5.3-flash';
  const text = $('play-input').value.trim();
  const sys = $('play-system').value.trim();
  if (!text && !playImages.length) { toast('Please enter a message', 3000); return; }
  if (playAbort) playAbort.abort();
  playAbort = new AbortController();
  const effort = $('play-effort') ? $('play-effort').value : '';
  playBusy(true);
  // Show the user's message and a typing indicator right away; restore the input if the request fails
  const shown = text + (playImages.length ? `${text ? '\n' : ''}[${playImages.length} image(s)]` : '');
  out.innerHTML = renderConversation().replace(/<div class="empty">[\s\S]*<\/div>$/, '') + msgHtml('user', shown) + '<div class="msg assistant typing"></div>';
  out.scrollTop = out.scrollHeight;
  $('play-input').value = ''; $('play-input').style.height = '';
  $('play-status').textContent = playImages.length ? 'Uploading images…' : 'Waiting for the model…';
  const started = Date.now();
  try {
    // Images: try POST /v1/uploads for storageId first (multimodal path), fall back to base64 text on failure
    const imgRefs = [];
    for (const im of playImages) {
      try {
        const b64 = (im.dataUrl.split(',')[1] || '');
        const bytes = base64ToBytes(b64);
        const r = await fetch('/v1/uploads', {
          method: 'POST',
          headers: Object.assign({ 'x-file-name': encodeURIComponent(im.name) }, apiKey() ? { 'authorization': 'Bearer ' + apiKey() } : {}),
          body: bytes, signal: playAbort.signal,
        });
        if (r.ok) { const j = await r.json().catch(() => ({})); imgRefs.push(j.storageId || j.url || j.data || j.data_url || j.path || im.dataUrl); }
        else { imgRefs.push(im.dataUrl); toast('Image upload failed, falling back to base64', 3000); }
      } catch (e2) { imgRefs.push(im.dataUrl); }
    }
    const userContent = imgRefs.length ? `${text ? text + '\n' : ''}[Image] ${imgRefs.join(' ')}` : text;
    const msgs = [];
    if (sys) msgs.push({ role: 'system', content: sys });
    playHistory.forEach(h => msgs.push(h));
    msgs.push({ role: 'user', content: userContent });
    const body = { model, messages: msgs, stream: true };
    if (effort) body.reasoning_effort = effort;
    const resp = await fetch('/v1/chat/completions', {
      method: 'POST',
      headers: Object.assign({ 'content-type': 'application/json' }, apiKey() ? { 'authorization': 'Bearer ' + apiKey() } : {}),
      body: JSON.stringify(body), signal: playAbort.signal,
    });
    if (!resp.ok || !resp.body) {
      const err = await resp.text().catch(() => '');
      let msg = 'HTTP ' + resp.status;
      try { msg = JSON.parse(err).error?.message || msg; } catch (e3) {}
      throw new Error(msg);
    }
    $('play-status').textContent = 'Streaming…';
    const reader = resp.body.getReader();
    const dec = new TextDecoder();
    let buf = '', content = '', asstEl = null;
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let idx;
      while ((idx = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, idx).trim(); buf = buf.slice(idx + 1);
        if (!line.startsWith('data:')) continue;
        const data = line.slice(5).trim();
        if (data === '[DONE]') { buf = ''; break; }
        try {
          const j = JSON.parse(data);
          const delta = (j.choices && j.choices[0] && j.choices[0].delta && (j.choices[0].delta.content || '')) || '';
          if (delta) {
            content += delta;
            if (!asstEl) { asstEl = out.querySelector('.msg.typing'); if (asstEl) asstEl.classList.remove('typing'); }
            if (asstEl) asstEl.innerHTML = mdLite(content);
            out.scrollTop = out.scrollHeight;
          }
        } catch (e4) { /* ignore intermediate chunk */ }
      }
    }
    playHistory.push({ role: 'user', content: userContent });
    if (content) playHistory.push({ role: 'assistant', content });
    out.innerHTML = renderConversation(); out.scrollTop = out.scrollHeight;
    $('play-status').textContent = content ? `Done in ${((Date.now()-started)/1000).toFixed(1)}s · ${content.length} characters` : 'Done (no content)';
    playImages = []; renderPlayImages();
  } catch (e) {
    if (!$('play-input').value) $('play-input').value = text;
    if (e.name === 'AbortError') { out.innerHTML = renderConversation(); $('play-status').textContent = 'Stopped'; }
    else { out.innerHTML = renderConversation().replace(/<div class="empty">[\s\S]*<\/div>$/, '') + `<div class="msg error">Request failed: ${esc(e.message)}</div>`; $('play-status').textContent = 'Failed after ' + ((Date.now()-started)/1000).toFixed(1) + 's'; }
    out.scrollTop = out.scrollHeight;
  } finally { if (playAbort) playAbort = null; playBusy(false); }
}
function playStop() { if (playAbort) playAbort.abort(); }
function playNewSession() {
  if (playAbort) playAbort.abort();
  playHistory = []; playImages = [];
  $('play-output').innerHTML = renderConversation();
  $('play-status').textContent = ''; $('play-input').value = ''; $('play-system').value = '';
  renderPlayImages();
}
function playClear() { playNewSession(); }
function playCopyOut() {
  const last = [...playHistory].reverse().find(h => h.role === 'assistant');
  if (!last) { toast('Nothing to copy', 2500); return; }
  navigator.clipboard?.writeText(last.content).then(() => toast('Copied')).catch(() => toast('Copy failed, please select manually'));
}
function playExportMd() {
  const md = playHistory.map(h => `**${h.role}**\n\n${h.content}`).join('\n\n---\n\n');
  if (!md.trim()) { toast('Conversation is empty, nothing to export', 2500); return; }
  const blob = new Blob([md], { type: 'text/markdown' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob); a.download = 'freebuff2api-chat.md'; a.click();
  URL.revokeObjectURL(a.href); toast('Markdown exported');
}
function base64ToBytes(b64) {
  const bin = atob(b64);
  const arr = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
  return arr;
}
// ---------- Settings page (v0.8) ----------
// Settings item render spec (key -> label/type/hint)
const SETTINGS_SPEC = [
  { key: 'listen_addr', label: 'Listen address', type: 'text', hint: 'Requires gateway restart to take effect (e.g. 127.0.0.1:47821)' },
  { key: 'memory_enabled', label: 'Memory layer (off by default)', type: 'switch', hint: 'When enabled, automatically records frequently used models/corrections and injects a system prefix' },
  { key: 'token_saver', label: 'Token saver (compress overly long tool_result)', type: 'switch', hint: '' },
  { key: 'redact_logs', label: 'Log/telemetry redaction', type: 'switch', hint: 'Replaces Cookie/Bearer/authorization values with ***' },
  { key: 'skills_inject_mode', label: 'Skill injection mode', type: 'select', options: ['roster', 'full'], hint: 'roster = inject name+description only; full = concatenate everything' },
  { key: 'max_roster_tokens', label: 'Roster injection token budget', type: 'number', hint: '' },
  { key: 'http_proxy', label: 'HTTP proxy', type: 'text', hint: 'Starts with http(s):// or socks5://, leave empty = direct connection' },
  { key: 'thread_cleanup_interval_sec', label: 'Upstream session cleanup interval (seconds)', type: 'number', hint: '0 = disable automatic cleanup' },
  { key: 'thread_max_age_hours', label: 'Maximum session retention time (hours)', type: 'number', hint: '' },
  { key: 'concurrency_free_slots', label: 'Free tier concurrency slots', type: 'number', hint: 'Dual-bucket semaphore: free {slots, normal}' },
  { key: 'concurrency_free_multi', label: 'Free tier concurrency (normal)', type: 'number', hint: '' },
  { key: 'concurrency_sub_slots', label: 'Subscription tier concurrency slots', type: 'number', hint: 'Subscription {slots, normal}' },
  { key: 'concurrency_sub_multi', label: 'Subscription tier concurrency (normal)', type: 'number', hint: '' },
];
async function loadSettings() {
  const w = $('settings-wrap'); if (!w) return;
  try {
    const g = await api('/api/config');
    const e = g.editable || {};
    w.innerHTML = '<div class="grid" style="gap:14px">' + SETTINGS_SPEC.map(s => {
      const val = e[s.key];
      let ctrl = '';
      if (s.type === 'switch') {
        ctrl = `<label class="switch"><input type="checkbox" ${val ? 'checked' : ''} onchange="saveSetting('${s.key}', this.checked)"><span class="slider"></span></label>`;
      } else if (s.type === 'select') {
        ctrl = `<select onchange="saveSetting('${s.key}', this.value)">${s.options.map(o => `<option value="${o}" ${val===o?'selected':''}>${o}</option>`).join('')}</select>`;
      } else if (s.type === 'number') {
        ctrl = `<input type="number" value="${val ?? ''}" onchange="saveSetting('${s.key}', this.value)">`;
      } else {
        ctrl = `<input type="text" value="${esc(String(val ?? ''))}" onchange="saveSetting('${s.key}', this.value)">`;
      }
      return `<div class="panel" style="margin:0"><div class="row"><div style="flex:1"><div style="font-size:13px">${esc(s.label)}</div>${s.hint ? `<div style="font-size:12px;color:var(--muted);margin-top:2px">${esc(s.hint)}</div>` : ''}</div>${ctrl}</div></div>`;
    }).join('') + '</div>';
  } catch (e) { w.innerHTML = `<div class="empty">Settings failed to load: ${esc(e.message)}</div>`; }
}
async function saveSetting(key, value) {
  try {
    const r = await api('/api/config', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key, value }) });
    toast((r.message || 'Saved') + (r.persisted === false ? ' (config.json not found, effective for this run only)' : ''), 4000);
    if (key === 'memory_enabled') loadSettings(); // refresh toggle state
  } catch (e) { toast('Save failed: ' + e.message, 5000); }
}

// ---------- About page (v0.8) ----------
async function loadAbout() {
  const w = $('about-wrap'); if (!w) return;
  try {
    const h = await api('/healthz');
    const ver = h.version || 'unknown';
    w.innerHTML = `
      <div class="kv"><b>Version</b><span id="about-ver">v${esc(ver)}</span></div>
      <div class="kv"><b>Uptime</b><span>${fmtUp(h.uptime_sec || 0)}</span></div>
      <div class="kv"><b>Listen address</b><span>${esc(location.host)}</span></div>
      <div class="kv"><b>Upstream</b><span>freebuff.com (reverse-engineered free tier)</span></div>
      <div class="kv"><b>Model count</b><span>${h.model_count ?? '—'}</span></div>
      <div style="margin-top:14px;padding-top:12px;border-top:1px solid var(--border);font-size:13px;color:var(--muted);line-height:1.8">
        Freebuff2API reverse-engineers Freebuff's free tier into a local OpenAI/Anthropic-compatible API gateway.
        This project has no official affiliation with OpenAI, Codebuff, or Freebuff. Provided for discussion, experimentation, and learning only, "as is", at the user's own risk (MIT license).
      </div>
      <div style="margin-top:10px;font-size:12px;color:var(--muted)">Security note: log/telemetry redaction is enabled by default; credentials are stored locally only. Do not expose the listen address to the public internet (unless api_keys is configured).</div>`;
  } catch (e) { w.innerHTML = `<div class="empty">About info failed to load: ${esc(e.message)}</div>`; }
}
function fmtUp(secs) {
  if (secs < 60) return secs + ' sec';
  if (secs < 3600) return Math.floor(secs/60) + ' min';
  if (secs < 86400) return Math.floor(secs/3600) + ' hr ' + Math.floor((secs%3600)/60) + ' min';
  return Math.floor(secs/86400) + ' days';
}

// ---------- Connection info (address / key / client config) ----------
let guideCache = null;
/**
 * Fetches /api/guide and renders the "Start requesting now" card + connection guide.
 * Both address and key are provided by the server, to avoid frontend hardcoding mismatching the actual listen address.
 */
async function loadGuide() {
  let g = null;
  try { g = await api('/api/guide'); } catch (e) { g = null; }
  // Distinguish two kinds of "can't get it": the API explicitly says not configured vs the request itself failed (the latter is most commonly because a key is enabled but not stored locally)
  const unknown = !g || !g.ok;
  if (unknown) {
    g = { listen_addr: location.host, openai_base_url: '/v1', anthropic_base_url: '/', api_keys: null, api_key_hint: '', models_count: null, models_sample: [], data_plane_ready: null };
  }
  guideCache = g;
  const base = location.origin;
  if ($('c-base')) {
    $('c-base').value = base;
    $('c-openai').value = base + '/v1';
    $('c-anthropic').value = base;
    const configured = !!(g.api_keys && g.api_keys.configured);
    const local = apiKey();
    $('c-key').value = unknown
      ? (local || '(Failed to load connection info -- please fill in the API Key at the top right of the page)')
      : (configured
        ? (local || '(Validation enabled -- click "Generate & Enable Key" or paste an existing key into the top-right input box)')
        : 'sk-local');
    $('c-key-hint').innerHTML = unknown
      ? `⚠️ Failed to load connection info${local ? ' (a key is stored locally; if it still fails, the key is incorrect)' : ''} -- if you configured api_keys in config.json, please paste it into the input box at the top right of the page.`
      : (configured
        ? `🔒 API Key validation enabled (${g.api_keys.count}: ${(g.api_keys.masked || []).map(esc).join(', ')}). Clients must supply the correct key.`
        : `🔓 No API Key configured -- accessible only from this machine, clients can fill in any non-empty string (e.g. <code>sk-local</code>).`);
    $('c-model-hint').innerHTML = unknown
      ? 'The model list and count require authentication to read.'
      : `<b>${g.models_count}</b> models available${(g.models_sample || []).length ? ', e.g. ' + g.models_sample.slice(0, 3).map(esc).join(', ') + ' ...' : ''} (full list: <code>${esc(base)}/v1/models</code>)`;
    $('connect-ready').innerHTML = g.data_plane_ready === true
      ? '<span class="tok">✅ Credentials ready, you can start making requests</span>'
      : g.data_plane_ready === false
        ? '<span class="twarn">⚠️ No credentials yet -- go to the "Account" tab and one-click login first</span>'
        : '<span style="color:var(--muted)">Status unknown (authentication required)</span>';
  }
  fillGuide(g);
}
async function genApiKey() {
  if (!confirm('Generate a new API Key and apply it immediately?\n\nAfter generating, your client must use this new key (this panel will remember it automatically).')) return;
  try {
    const r = await api('/api/config/api-key', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ action: 'generate' }) });
    if (r.key) { try { localStorage.setItem('freebuff_api_key', r.key); } catch (e) {} }
    toast(r.message || 'Generated', 6000);
    loadGuide();
  } catch (e) { toast('Generation failed: ' + e.message, 7000); }
}
async function clearApiKey() {
  if (!confirm('Clear the API Key?\n\nAfter clearing, the gateway reverts to "local access only, any key accepted" mode.')) return;
  try {
    const r = await api('/api/config/api-key', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ action: 'clear' }) });
    try { localStorage.removeItem('freebuff_api_key'); } catch (e) {}
    toast(r.message || 'Cleared', 6000);
    loadGuide();
  } catch (e) { toast('Clear failed: ' + e.message, 7000); }
}
function fillGuide(g) {
  const base = location.origin;
  const key = (g && g.api_keys && g.api_keys.configured) ? (apiKey() || '<the key you generated in the panel>') : 'sk-local';
  const model = (g && (g.models_sample || [])[0]) || 'z-ai/glm-5.3-flash';
  const gb = $('guide-base'); if (gb) gb.textContent = base;
  const gk = $('guide-key-hint');
  if (gk) gk.innerHTML = (g && g.api_keys && g.api_keys.configured)
    ? 'API Key generated and enabled in the panel -- clients must supply this key (already remembered at the top right of this page)'
    : 'When api_keys is not configured in config.json, any string works (e.g. <code>sk-local</code>)';
  $('g-claude').textContent = `# macOS / Linux\nexport ANTHROPIC_BASE_URL=${base}\nexport ANTHROPIC_API_KEY=${key}\n\n# Windows PowerShell\n$env:ANTHROPIC_BASE_URL="${base}"\n$env:ANTHROPIC_API_KEY="${key}"\n\n# Then start Claude Code as usual (the claude command)`;
  $('g-openai').textContent = `Base URL: ${base}/v1\nAPI Key:  ${key}\nModel:    pick one from ${base}/v1/models (${(g && g.models_count) || '?'} total)`;
  $('g-py').textContent = `from openai import OpenAI\nclient = OpenAI(base_url="${base}/v1", api_key="${key}")\nresp = client.chat.completions.create(model="${model}", messages=[{"role":"user","content":"Hello"}])\nprint(resp.choices[0].message.content)`;
  $('g-node').textContent = `import OpenAI from "openai";\nconst client = new OpenAI({ baseURL: "${base}/v1", apiKey: "${key}" });\nconst r = await client.chat.completions.create({ model: "${model}", messages: [{ role: "user", content: "Hello" }] });\nconsole.log(r.choices[0].message.content);`;
  $('g-curl').textContent = `curl ${base}/v1/chat/completions \\\n  -H "content-type: application/json" \\\n  -H "authorization: Bearer ${key}" \\\n  -d "{\\"model\\":\\"${model}\\",\\"messages\\":[{\\"role\\":\\"user\\",\\"content\\":\\"Say something to prove the connection works\\"}]}"`;
  $('g-lobe').textContent = `Endpoint: ${base}/v1\nAPI Key:  ${key}\nModel:    manually enter a value from the /v1/models list (e.g. ${model})`;
}

// ---------- Overview ----------
let lastHealth = null;
let authWarned = false;      // only warn once on auth failure, avoid flashing repeatedly
let overviewTimer = null;    // overview auto-refresh handle (paused on auth failure, resumed after key is filled in)
async function refreshOverview() {
  try {
    const health = await api('/healthz');
    lastHealth = health;
    $('dot').className = 'dot ok';
    $('ver').textContent = 'v' + (health.version || '');
    const accs = health.accounts || [];
    const alive = accs.filter(a => a.healthy).length;
    $('hstat').innerHTML = `Accounts <b>${alive}/${accs.length}</b> · Uptime <b>${Math.floor((health.uptime_sec || 0) / 60)}m</b>`;
    const totals = await api('/api/usage/totals');
    const cards = [
      ['Total requests', totals.total_requests ?? 0],
      ['Total tokens', (totals.total_tokens ?? 0).toLocaleString()],
      ['Errors', totals.errors ?? 0],
      ['Accounts', `${alive}/${accs.length}`],
    ];
    $('cards').innerHTML = cards.map(([l, n]) => `<div class="card"><div class="lbl">${l}</div><div class="num">${n}</div></div>`).join('');
    // Rate and error rate (free tier has no monetary cost, honestly label the source)
    try {
      const cost = await api('/api/usage/cost');
      $('cost-line').innerHTML = `Last ${cost.window_minutes} min: <b>${cost.requests_30m}</b> requests · error rate <b>${((cost.error_rate_30m || 0) * 100).toFixed(0)}%</b> · avg latency <b>${((cost.avg_latency_ms_30m || 0) / 1000).toFixed(1)}s</b> · approx. <b>${cost.requests_per_hour}</b> requests/hour <span style="opacity:.7">(${esc(cost.cost_source || '')})</span>`;
    } catch (e) { $('cost-line').textContent = ''; }
    // Credential cooldown warning (v0.10 §2.2): circuit-broken account notice + one-click jump to account page
    const cw = $('cred-warn');
    if (cw) {
      try {
        const h = await api('/api/accounts/health');
        const cooling = (h.accounts || []).filter(a => a.circuit_state === 'open' || a.circuit_state === 'half_open');
        if (cooling.length) {
          const secs = cooling.map(a => parseCooldownSec(a.cooldown_until)).filter(n => n >= 0);
          const secText = secs.length ? `, fastest recovery in about ${Math.round(Math.min(...secs))}s` : '';
          cw.innerHTML = `<div class="banner"><b>⚠️ ${cooling.length} account(s) cooling down${secText}</b> <span style="font-size:12px;color:var(--muted)">(triggered by 401/403 or repeated failures; recovers automatically on expiry)</span> <button class="ghost sm" onclick="showTab('account')">Go to Account tab</button></div>`;
        } else { cw.innerHTML = ''; }
      } catch (e5) { cw.innerHTML = ''; }
    }
    // No-account onboarding
    if (accs.length === 0) {
      $('banner').innerHTML = `<div class="banner"><h2>👋 Get started in three steps</h2><ol>
        <li><b>Add an account</b>: switch to the "Account" tab and paste a Cookie, or use "One-click login" in the desktop tray app</li>
        <li><b>Connect a client</b>: switch to the "Connection Guide" tab and copy the config into Claude Code / Cursor, etc.</li>
        <li><b>Start chatting</b>: come back here to see requests, tokens, logs, and health</li></ol></div>`;
    } else { $('banner').innerHTML = ''; }
    // Account table
    $('acc-wrap').innerHTML = accs.length ? `<table><thead><tr><th>Account</th><th>Status</th><th>Score</th><th>Session</th><th>Error</th></tr></thead><tbody>${
      accs.map(a => {
        const st = a.session?.status || 'unknown';
        const cls = (st === 'active' || a.healthy) ? 'ok' : (st === 'queued' ? 'warn' : 'err');
        return `<tr><td>${esc(a.name)}</td><td>${badge(st, cls)}</td><td>${Math.round(a.score ?? 0)}</td><td>${a.session?.instance_id ? esc(String(a.session.instance_id).slice(0, 8)) + '…' : '—'}</td><td>${esc(a.last_error || a.session?.last_error || '')}</td></tr>`;
      }).join('')}</tbody></table>` : '<div class="empty">No accounts yet -- add one on the "Account" tab</div>';
    // Usage
    const daily = await api('/api/usage/daily');
    $('daily-wrap').innerHTML = (daily && daily.length) ? `<table><thead><tr><th>Date</th><th>Model</th><th>Requests</th><th>Input tok</th><th>Output tok</th><th>Errors</th></tr></thead><tbody>${
      daily.slice(0, 30).map(d => `<tr><td>${esc(d.date)}</td><td>${esc(d.model)}</td><td>${d.requests}</td><td>${d.prompt_tokens}</td><td>${d.completion_tokens}</td><td>${d.errors}</td></tr>`).join('')}</tbody></table>` : '<div class="empty">No usage yet -- data will appear here after your first conversation</div>';
    // Requests
    const reqs = await api('/api/usage/requests');
    $('reqs-wrap').innerHTML = (reqs && reqs.length) ? `<table><thead><tr><th>Time</th><th>Account</th><th>Model</th><th>Status</th><th>Latency</th><th>Tokens</th></tr></thead><tbody>${
      reqs.slice(0, 20).map(r => {
        const cls = r.status < 400 ? 'ok' : (r.status < 500 ? 'warn' : 'err');
        const tk = (r.prompt_tokens || 0) + (r.completion_tokens || 0);
        return `<tr class="click" onclick="openDrawer(${r.id})"><td>${fmtTime(r.ts)}</td><td>${esc(r.account)}</td><td>${esc(r.model)}</td><td>${badge(r.status, cls)}</td><td>${(r.latency_ms / 1000).toFixed(2)}s</td><td>${tk || '—'}</td></tr>`;
      }).join('')}</tbody></table>` : '<div class="empty">No request records yet</div>';
    // Models
    const models = await api('/api/usage/models');
    $('model-count').textContent = (models || []).length;
    $('models-wrap').innerHTML = (models || []).map(m => `<span class="chip">${esc(m)}</span>`).join('') || '<div class="empty">Model list is empty (check upstream connectivity)</div>';
    if (authWarned) { authWarned = false; $('banner').innerHTML = ''; }
    if (!overviewTimer) startOverviewTimer();
  } catch (e) {
    $('dot').className = 'dot err';
    const m = String(e.message || '');
    // The gateway has API Key enabled but the browser hasn't filled it in yet: show a one-time hint and pause auto-refresh (otherwise the red dot + toast would flash every 6 seconds)
    if (/unauthorized|api key|鉴权|认证/i.test(m)) {
      if (!authWarned) {
        authWarned = true;
        toast('This gateway has API Key validation enabled -- please fill in the key in the input box at the top right of the page', 8000);
        $('banner').innerHTML = `<div class="banner"><h2>🔑 Authentication required</h2>
          <p style="font-size:13px;color:var(--muted);margin-top:6px">The gateway has <code>api_keys</code> configured. Fill in the key in the <b>input box at the top right of the page</b> (stored only in this browser); this will resume automatically once done.</p></div>`;
        if (overviewTimer) { clearInterval(overviewTimer); overviewTimer = null; }
      }
    } else {
      toast('Load failed: ' + m);
    }
  }
}
function parseCooldownSec(v) {
  if (!v) return Infinity;
  if (typeof v === 'string' && /^\d+s$/.test(v)) return parseInt(v, 10);
  const t = Date.parse(v);
  if (!Number.isNaN(t)) return Math.max(0, Math.round((t - Date.now()) / 1000));
  const n = parseInt(String(v).replace(/[^0-9]/g, ''), 10);
  return Number.isFinite(n) ? n : Infinity;
}
function startOverviewTimer() {
  if (overviewTimer) clearInterval(overviewTimer);
  overviewTimer = setInterval(() => { if ($('tab-overview').style.display !== 'none') refreshOverview(); }, 6000);
}

async function loadBalance() {
  try {
    const r = await fetch('/api/account/balance', { headers: apiKey() ? { authorization: 'Bearer ' + apiKey() } : {} });
    if (!r.ok) {
      $('balance-panel').style.display = '';
      $('balance-wrap').innerHTML = '<div class="empty">A web Cookie credential is required to check balance (import a Cookie under "Add Account" first)</div>';
      return;
    }
    const bal = await r.json();
    if (bal && bal.ok !== false) {
      $('balance-panel').style.display = '';
      const d = bal.freebucks?.daily || {};
      let html = `<p style="font-size:13px;color:var(--muted)">Plan <b>${esc(bal.subscription?.tierId || 'Free')}</b> · Tier ${esc(bal.access_tier || '—')}${bal.country_block_reason ? ' · ⚠️ Region restricted(' + esc(bal.country_block_reason) + ')' : ''}</p>`;
      if (d.limit != null) html += `<p style="font-size:13px;color:var(--muted)">Today's credits <b class="tok">${d.remaining ?? '—'}</b> / ${d.limit ?? '—'} (used ${d.spent ?? 0})</p>`;
      const mr = bal.model_remaining || {};
      const rows = Object.entries(mr).slice(0, 30).map(([m, v]) => `<tr><td>${esc(m)}</td><td>${v.price === 0 ? '<b class="tok">Free</b>' : v.price}</td><td>${v.usable_today === -1 ? 'Unlimited' : v.usable_today}</td></tr>`).join('');
      if (rows) html += `<table style="margin-top:8px"><thead><tr><th>Model</th><th>Credit price</th><th>Remaining today</th></tr></thead><tbody>${rows}</tbody></table>`;
      $('balance-wrap').innerHTML = html;
    } else {
      $('balance-panel').style.display = '';
      $('balance-wrap').innerHTML = '<div class="empty">A web Cookie credential is required to check balance (import a Cookie under "Add Account" first)</div>';
    }
  } catch (e) { /* Silent when there's no Cookie */ }
}

// ---------- Request details ----------
async function openDrawer(id) {
  $('drawer').classList.add('open');
  $('dr-title').textContent = 'Request #' + id;
  $('dr-body').innerHTML = '<div class="empty">Loading…</div>';
  try {
    const d = await api('/api/usage/requests/' + id);
    const r = d.request || {};
    const events = d.events || [];
    let html = '';
    html += `<div class="kv"><b>Time</b>${esc(r.ts)}</div>`;
    html += `<div class="kv"><b>Endpoint</b>${esc(r.endpoint || '—')}</div>`;
    html += `<div class="kv"><b>Account</b>${esc(r.account)}</div>`;
    html += `<div class="kv"><b>Requested model</b>${esc(r.requested_model || r.model)}</div>`;
    html += `<div class="kv"><b>Resolved model</b>${esc(r.resolved_model || r.model)}</div>`;
    html += `<div class="kv"><b>Status</b>${r.status} ${esc(r.error_kind ? '(' + r.error_kind + ')' : '')}</div>`;
    html += `<div class="kv"><b>Latency</b>${(r.latency_ms / 1000).toFixed(2)}s${r.ttft_ms ? ' (first byte ' + r.ttft_ms + 'ms)' : ''}</div>`;
    if (r.latency_ms) {
      const ttftPct = r.ttft_ms != null ? Math.max(0, Math.min(100, Math.round(r.ttft_ms / r.latency_ms * 100))) : null;
      html += `<div style="margin-top:10px"><b style="font-size:12px">⏱ Timing timeline</b>` +
        `<div style="position:relative;height:8px;background:#21262d;border-radius:4px;margin-top:6px">` +
        `<div style="position:absolute;left:0;top:0;height:8px;border-radius:4px;background:var(--accent);width:${ttftPct == null ? 100 : ttftPct}%"></div>` +
        (ttftPct != null ? `<div style="position:absolute;left:${ttftPct}%;width:2px;height:8px;background:var(--warn)"></div>` : '') +
        `</div><div style="display:flex;font-size:11px;color:var(--muted);margin-top:4px"><span>First byte ${r.ttft_ms != null ? r.ttft_ms + 'ms' : '—'}</span><span style="flex:1"></span><span>Total time ${(r.latency_ms / 1000).toFixed(2)}s</span></div></div>`;
    }
    html += `<div class="kv"><b>Tokens</b>input ${r.prompt_tokens || 0} / output ${r.completion_tokens || 0}</div>`;
    if (r.route_reason) html += `<div class="kv"><b>Route reason</b>${esc(r.route_reason)}</div>`;
    if (r.error_excerpt) html += `<details open><summary>Error details</summary><pre>${esc(r.error_excerpt)}</pre></details>`;
    html += `<div class="kv" style="margin-top:10px"><b>Explanation</b>${esc(explain(r))}</div>`;
    if (events.length) html += `<details open><summary>Event chain (${events.length})</summary>${events.map(e => `<div class="kv" style="font-size:12px"><b>${fmtTime(e.ts)} ${esc(e.kind)}</b>${esc(e.detail)}</div>`).join('')}</details>`;
    $('dr-body').innerHTML = html;
  } catch (e) { $('dr-body').innerHTML = `<div class="empty">Load failed: ${esc(e.message)}</div>`; }
}
function closeDrawer() { $('drawer').classList.remove('open'); }
function explain(r) {
  const k = r.error_kind || '';
  if (k === 'waiting_room') return 'Upstream free queue in progress — this is not a gateway fault, just retry shortly.';
  if (k === 'no_account') return 'No available account — add one on the "Account" page.';
  if (k === 'upstream_4xx') return 'Upstream rejected this request (usually an expired credential or unavailable model). See error details.';
  if (k === 'upstream_5xx') return 'Upstream server error — usually just retry, the gateway will switch accounts automatically.';
  if (k === 'network' || k === 'timeout') return 'Network timeout/interruption — check proxy settings and upstream connectivity (testable on the health check page).';
  if (r.status >= 200 && r.status < 300) return 'Request succeeded.' + (r.ttft_ms ? `First byte ${r.ttft_ms}ms, ` : '') + `total time ${(r.latency_ms / 1000).toFixed(2)}s.`;
  return 'No explanation data available.';
}

// ---------- Balance card ----------
// ---------- Browser extension bridge (panel <-> extension direct connection) ----------
// The extension's bridge.js content script broadcasts its own id via postMessage;
// once this page has the id, it can use chrome.runtime.sendMessage to directly tell the extension to read the Cookie (true one-click login).
let extId = null, extVersion = '';
function pingExtension(showToast) {
  try { window.postMessage({ source: 'freebuff2api-page', type: 'ping' }, location.origin); } catch (e) {}
  if (showToast) setTimeout(() => {
    toast(extId ? `Extension ready (v${extVersion || '?'})` : 'Extension not detected — click "⬇ Download extension" to install, or use the manual wizard', 5000);
  }, 600);
}
window.addEventListener('message', (e) => {
  if (e.source !== window) return;
  const d = e.data;
  if (!d || d.source !== 'freebuff2api-extension' || typeof d.id !== 'string') return;
  const isNew = extId !== d.id;
  extId = d.id; extVersion = d.version || '';
  renderExtStatus();
  if (isNew && $('tab-account') && $('tab-account').style.display !== 'none') { /* Silent on first entry */ }
});
function renderExtStatus() {
  const el = $('ext-status');
  if (!el) return;
  if (extId) { el.className = 'badge ok'; el.textContent = `Extension ready v${extVersion || '?'}`; }
  else { el.className = 'badge dim'; el.textContent = 'Extension not detected (you can import by pasting manually)'; }
}
function extensionAvailable() {
  return !!extId && typeof chrome !== 'undefined' && chrome.runtime && typeof chrome.runtime.sendMessage === 'function';
}
function sendToExtension(msg) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Extension did not respond within the time limit (it may have been reclaimed by the browser, please refresh and retry)')), 20000);
    try {
      chrome.runtime.sendMessage(extId, msg, (resp) => {
        clearTimeout(timer);
        if (chrome.runtime.lastError) { reject(new Error(chrome.runtime.lastError.message)); return; }
        resolve(resp);
      });
    } catch (e) { clearTimeout(timer); reject(e); }
  });
}
function downloadExtension() {
  const k = apiKey();
  window.open('/api/extension/bundle' + (k ? '?key=' + encodeURIComponent(k) : ''), '_blank');
}

// ---------- Account / import ----------
// ---------- Embedded WebView2 login window (gateway-spawned --login-window subprocess) ----------
/**
 * Pop up the embedded login window: the backend spawns `current-exe --login-window` (a separate process, tao+WebView2 event loop),
 * after the user completes GitHub login in the window, the backend automatically captures the Cookie (including HttpOnly) and stores it.
 * The result is detected by the panel polling /api/tokens (a credential count increase means success).
 */
async function openEmbedLogin() {
  const st = $('embed-status');
  if (st) st.textContent = 'Opening window…';
  try {
    const r = await api('/api/login/embed', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' });
    if (!r.ok) {
      if (st) st.innerHTML = '<span class="terr">' + esc(r.message || 'not supported in this environment') + '</span>';
      toast('Embedded window unavailable: ' + (r.message || 'please use plan B/C instead'), 8000);
      return;
    }
    toast('Embedded login window opened — complete GitHub login in the window and it will be stored automatically', 8000);
    if (st) st.innerHTML = '<span class="tok">Window opened, waiting for login…</span>';
    const before = await tokenCount();
    // Dual-channel detection: credential count increase (success) or result file reports failure (subprocess exit code non-zero)
    const deadline = Date.now() + 600000;
    while (Date.now() < deadline) {
      await new Promise(res => setTimeout(res, 3000));
      const now = await tokenCount();
      if (before >= 0 && now > before) {
        toast('✅ Embedded window login succeeded, credential stored automatically', 6000);
        if (st) st.innerHTML = '<span class="tok">✅ Stored</span>';
        refreshTokens(); refreshAccountOverview(); loadHistory();
        return;
      }
      // Failure result file (written by backend when the subprocess exits abnormally)
      try {
        const fr = await api('/api/login/result');
        if (fr && fr.ok === false && fr.message) {
          if (st) st.innerHTML = '<span class="terr">' + esc(fr.message) + '</span>';
          toast('Embedded login window exited: ' + fr.message, 9000);
          return;
        }
      } catch (e2) { /* Result-file API failure does not block the main polling loop */ }
    }
    if (st) st.innerHTML = '<span class="twarn">Wait timed out (10 minutes)</span>';
    toast('Login wait timed out — please retry or use plan B/C instead', 8000);
  } catch (e) {
    if (st) st.innerHTML = '<span class="terr">' + esc(e.message) + '</span>';
    toast('Popup failed: ' + e.message + ' — please use plan B/C instead', 8000);
  }
}

async function oneClickLogin() {
  // [Main-control wiring] Embedded window dispatch will be inserted here (embedded WebView2 login window takes priority, implemented by main control)
  // Path 1: Desktop Electron (main process can read HttpOnly Cookie directly)
  if (window.freebuffDesktop && window.freebuffDesktop.openLogin) {
    window.freebuffDesktop.openLogin();
    toast('Login window opened, the Cookie will be stored automatically after logging into freebuff.com');
    return;
  }
  // Path 2: Direct browser extension connection -- fully automatic (auto-opens freebuff.com -> wait for login -> auto store)
  if (extensionAvailable()) {
    await oneClickViaExtension();
    return;
  }
  // Path 3: Fall back to manual wizard (highlight "clipboard auto-detect" -- the manual path with the fewest steps)
  const wiz = $('login-wizard');
  wiz.style.display = '';
  wiz.scrollIntoView({ behavior: 'smooth', block: 'start' });
  const clipCard = $('wizard-clip');
  if (clipCard) {
    clipCard.classList.remove('wizard-flash');
    // Force reflow so the animation can retrigger
    void clipCard.offsetWidth;
    clipCard.classList.add('wizard-flash');
  }
  window.open('https://freebuff.com/', '_blank', 'noopener');
  toast('Browser extension not detected — freebuff.com has been opened, we recommend using the wizard "Plan B: auto-detect clipboard" (just copy and click)', 9000);
}
/**
 * Clipboard auto-import: read clipboard text -> fill into the input box -> automatically trigger import.
 * The clipboard API requires a secure context, and some browsers require the page to be focused first; on failure, clearly prompt to use manual paste instead (a fallback path always exists).
 */
async function importFromClipboard() {
  const st = $('clip-status');
  if (st) st.textContent = 'Reading clipboard… (if the browser asks for permission, please allow it)';
  let text = '';
  try {
    if (!navigator.clipboard || typeof navigator.clipboard.readText !== 'function') {
      throw new Error('This browser does not support reading the clipboard (or the page is not in an HTTPS/localhost secure context)');
    }
    text = (await navigator.clipboard.readText()).trim();
  } catch (e) {
    if (st) st.innerHTML = `<span class="twarn">Failed to read clipboard: ${esc(e.message)}</span> — please paste manually into the input box below (Ctrl+V), it works exactly the same`;
    toast('Could not read clipboard — please paste manually into the input box then click "Import"', 7000);
    $('import-text').focus();
    return;
  }
  if (!text) {
    if (st) st.innerHTML = '<span class="twarn">Clipboard is empty — first go to freebuff.com DevTools and copy the entire Cookie line (Ctrl+C)</span>';
    return;
  }
  if (st) st.textContent = 'Content retrieved from clipboard, starting automatic import…';
  $('import-text').value = text;
  toast('Content read from clipboard, importing automatically…', 4000);
  await doImport('clipboard');
}
/**
 * Instant verification after successful import: find the most recently stored credential -> call the upstream check endpoint -> show the result directly in the wizard.
 * Silently skip when the id cannot be matched (list refresh is already handled by doImport, does not block the main flow).
 */
async function verifyNewCredential() {
  const box = $('import-verify');
  if (!box) return;
  box.style.display = '';
  box.innerHTML = '<span style="color:var(--muted)">⏳ Verifying the just-imported credential with upstream… (takes a few seconds)</span>';
  try {
    const r = await api('/api/tokens');
    const list = r.tokens || [];
    // The list API does not guarantee ordering, compare stored time to pick the latest one
    const newest = list.slice().sort((a, b) => String(b.added_at || '').localeCompare(String(a.added_at || '')))[0];
    if (!newest || !newest.id) { box.style.display = 'none'; return; }
    const c = await api('/api/tokens/check', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id: newest.id }) });
    if (c.ok && c.valid) {
      const m = c.meta || {};
      const who = m.email || m.name || 'Account info updated';
      box.innerHTML = `<span class="tok">✅ Verified: ${esc(who)}</span> <span style="color:var(--muted)">(${esc([m.name, m.email].filter(Boolean).join(' · ') || 'Credential valid')})</span>`;
    } else {
      box.innerHTML = `<span class="twarn">⚠️ Cookie recorded but upstream verification failed (may not be logged in or has expired), please copy again</span>${c.message ? `<div style="font-size:12px;color:var(--muted);margin-top:4px">Upstream says: ${esc(c.message)}</div>` : ''}`;
    }
  } catch (e) {
    box.innerHTML = `<span class="twarn">⚠️ Automatic verification failed: ${esc(e.message)}</span> <span style="color:var(--muted)">— credential stored, you can manually verify later by clicking "Check" in the credential list</span>`;
  }
}
async function oneClickViaExtension() {
  toast('Notifying extension…', 3000);
  const before = await tokenCount();
  let resp = null;
  try {
    resp = await sendToExtension({ type: 'freebuff2api.import', gatewayPort: Number(location.port || 47821), apiKey: apiKey() });
  } catch (e) {
    toast('Failed to call extension: ' + e.message, 7000);
    return;
  }
  if (resp && resp.ok === false) { toast('Extension returned: ' + (resp.message || 'import failed'), 8000); return; }
  if (resp && resp.done) {
    // Extension completed synchronously (already logged in and imported / or already existed and was deduplicated) -- no polling needed
    if (resp.added > 0) {
      toast(`✅ Extension imported ${resp.added} credential(s)`, 6000);
      refreshTokens(); refreshAccountOverview(); loadHistory();
    } else {
      toast('This account credential is already stored (identical values are auto-deduplicated, no need to import again)', 7000);
      refreshTokens();
    }
    return;
  }
  if (resp && resp.needLogin) {
    toast('freebuff.com opened automatically — the credential will be stored automatically after GitHub login completes (wait up to 3 minutes)', 9000);
  } else {
    toast('Extension has started importing, waiting for the credential to be stored…', 6000);
  }
  pollForNewCredential(before, 180000);
}
async function tokenCount() {
  try { const r = await api('/api/tokens'); return (r.tokens || []).length; } catch (e) { return -1; }
}
/** Poll waiting for the extension to write the credential into the gateway (the extension is a separate process, can only converge via polling) */
async function pollForNewCredential(before, timeoutMs) {
  // When the baseline was not obtained (before<0), fetch it once first, to avoid "any successful request being falsely reported as stored"
  if (before < 0) before = await tokenCount();
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    await new Promise(r => setTimeout(r, 3000));
    const now = await tokenCount();
    if (before >= 0 && now > before) {
      toast('✅ Credential stored automatically, fetching account info…', 6000);
      refreshTokens(); refreshAccountOverview(); loadHistory();
      return;
    }
  }
  toast('Login wait timed out — after completing login click the extension icon, or come back to this page and click "Refresh"', 8000);
}
async function doImport(source) {
  const text = $('import-text').value.trim();
  if (!text) { toast('Please paste content first'); return; }
  $('import-result').textContent = 'Importing…';
  const verifyBox = $('import-verify');
  if (verifyBox) { verifyBox.style.display = 'none'; verifyBox.innerHTML = ''; }
  try {
    const r = await api('/api/tokens/import', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ cookie: text }) });
    if (r.added > 0) {
      $('import-result').innerHTML = `<span class="tok">✅ Successfully imported ${r.added} credential(s)${source === 'clipboard' ? ' (source: clipboard)' : ''}</span>`;
      $('import-text').value = '';
      $('login-wizard').style.display = 'none';
      toast(source === 'clipboard' ? '✅ Clipboard content imported successfully, verifying credential…' : 'Import succeeded, fetching account info…');
      refreshTokens();
      refreshAccountOverview();
      loadHistory();
      // Instant verification: right after a successful import, verify the new credential with upstream and show the result near the wizard
      verifyNewCredential();
    } else {
      $('import-result').innerHTML = `<span class="twarn">This credential already exists (identical values are auto-deduplicated, not stored again)</span>`;
      refreshTokens();
    }
  } catch (e) { $('import-result').innerHTML = `<span class="terr">Import failed: ${esc(e.message)}</span>`; }
}

// ---------- Credential list (account details / stored time / check / delete) ----------
let tokenCache = [];
async function refreshTokens() {
  try {
    const r = await api('/api/tokens');
    const list = r.tokens || [];
    tokenCache = list;
    $('cred-count').textContent = list.length ? `(${list.length} · same-value auto-dedup)` : '';
    fillHistoryCredOptions(list);
    $('tokens-wrap').innerHTML = list.length ? `<table><thead><tr>
        <th>Account</th><th>Type</th><th>Credential</th><th>Plan</th><th>Remaining today</th><th>Added</th><th>Actions</th>
      </tr></thead><tbody>${
      list.map((t, i) => {
        const m = t.meta || null;
        const acct = m
          ? `<div style="font-weight:600">${esc(m.name || '—')}</div><div style="font-size:12px;color:var(--muted)">${esc(m.email || '')}</div>`
          : `<span style="color:var(--muted)">Not checked</span>`;
        const validBadge = m ? (m.valid ? badge('Valid', 'ok') : badge('Possibly invalid', 'err')) : '';
        const tier = m ? (m.tier_id ? badge(m.tier_id, 'ok') : badge('Free', 'dim')) : '—';
        const remain = (m && m.daily_remaining != null) ? `<b>${m.daily_remaining}</b> / ${m.daily_limit ?? '—'}` : '—';
        const added = t.added_at ? new Date(t.added_at).toLocaleString('en-GB', { hour12: false }) : '—';
        return `<tr>
          <td>${acct} ${validBadge}</td>
          <td>${t.kind === 'web-cookie' ? badge('Web Cookie', 'ok') : badge('Bearer', 'dim')}</td>
          <td><code>${esc(t.token_masked)}</code><div style="font-size:11px;color:var(--muted)">${esc(t.source || '')}${t.host ? ' · ' + esc(t.host) : ''}</div></td>
          <td>${tier}</td>
          <td style="font-size:12px">${remain}</td>
          <td style="font-size:12px">${esc(added)}</td>
          <td class="row">
            <button class="ghost sm" onclick="checkCredAt(${i})">Check</button>
            <button class="ghost sm" onclick="openCredDetail(${i})">Details</button>
            <button class="ghost sm" onclick="deleteCredAt(${i})">Delete</button>
          </td></tr>`;
      }).join('')}</tbody></table>` : '<div class="empty">No credentials imported yet — use "One-click login" above or paste to import</div>';
  } catch (e) { $('tokens-wrap').innerHTML = `<div class="empty">Load failed: ${esc(e.message)}</div>`; }
}
function checkCredAt(i) { const t = tokenCache[i]; if (t) checkCred(t.id); }
function deleteCredAt(i) { const t = tokenCache[i]; if (t) deleteCred(t.id, (t.meta && t.meta.email) || t.token_masked); }
function openCredDetail(i) { const t = tokenCache[i]; if (t) renderCredDetail(t); }

async function checkCred(id) {
  toast('Checking this credential… (takes a few seconds)');
  try {
    const r = await api('/api/tokens/check', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id }) });
    toast(r.message || (r.ok ? 'Credential valid' : 'Credential may be invalid'), 7000);
    refreshTokens();
    loadHistory();
  } catch (e) { toast('Check failed: ' + e.message, 7000); }
}
async function deleteCred(id, label) {
  if (!confirm(`Delete credential ${label || ''}?\n\nThis will also remove it from the running account pool; you'll need to log in and import again to restore it.`)) return;
  try {
    const r = await api('/api/tokens/delete', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id }) });
    toast(r.message || 'Deleted', 5000);
    refreshTokens();
    loadHistory();
  } catch (e) { toast('Delete failed: ' + e.message, 7000); }
}
function renderCredDetail(t) {
  const m = t.meta;
  $('drawer').classList.add('open');
  $('dr-title').textContent = 'Credential details';
  let html = '';
  html += `<div class="kv"><b>Credential</b><code>${esc(t.token_masked)}</code></div>`;
  html += `<div class="kv"><b>Type</b>${t.kind === 'web-cookie' ? 'Web Cookie (website login)' : 'Bearer Token'}</div>`;
  html += `<div class="kv"><b>Source</b>${esc(t.source || '—')}${t.host ? ' · ' + esc(t.host) : ''}</div>`;
  html += `<div class="kv"><b>Added</b>${t.added_at ? new Date(t.added_at).toLocaleString('en-GB', { hour12: false }) : 'Unknown (legacy data)'}</div>`;
  if (!m) {
    html += `<div class="empty" style="margin-top:12px">This credential has not been checked yet — click "Check" in the credential list to fetch account details.</div>`;
    $('dr-body').innerHTML = html;
    return;
  }
  html += `<div class="kv"><b>Checked</b>${m.checked_at ? new Date(m.checked_at).toLocaleString('en-GB', { hour12: false }) : '—'} ${m.valid ? badge('Valid', 'ok') : badge('Possibly invalid', 'err')}</div>`;
  html += '<div style="border-top:1px solid var(--border);margin:10px 0;padding-top:8px"><b>Account</b></div>';
  if (m.image) html += `<img src="${esc(m.image)}" style="width:40px;height:40px;border-radius:50%;vertical-align:middle" onerror="this.style.display='none'">`;
  html += `<div class="kv"><b>Nickname</b>${esc(m.name || '—')}</div>`;
  html += `<div class="kv"><b>Email</b>${esc(m.email || '—')}</div>`;
  if (m.user_id) html += `<div class="kv"><b>User ID</b><code>${esc(m.user_id)}</code></div>`;
  if (m.expires) html += `<div class="kv"><b>Login valid until</b>${new Date(m.expires).toLocaleString('en-GB', { hour12: false })}</div>`;
  html += '<div style="border-top:1px solid var(--border);margin:10px 0;padding-top:8px"><b>Quota</b></div>';
  html += `<div class="kv"><b>Tier / Plan</b>${esc(m.access_tier || '—')} / ${esc(m.tier_id || 'Free')}</div>`;
  if (m.daily_limit != null) html += `<div class="kv"><b>Today's credits</b>remaining <b class="tok">${m.daily_remaining ?? '—'}</b> / ${m.daily_limit} (used ${m.daily_spent ?? 0})</div>`;
  if (m.reset_at) html += `<div class="kv"><b>Next reset</b>${new Date(m.reset_at).toLocaleString('en-GB', { hour12: false })}</div>`;
  if (m.streak_current != null) html += `<div class="kv"><b>Streak</b>${m.streak_current} days (total active ${m.all_time_active_days ?? '—'} days)</div>`;
  if (m.tokens_7d != null) html += `<div class="kv"><b>Tokens in last 7 days</b>${m.tokens_7d.toLocaleString()}</div>`;
  if (m.country_code) html += `<div class="kv"><b>Region</b>${esc(m.country_code)}${m.country_block_reason ? ' ' + badge(m.country_block_reason, 'err') : ''}</div>`;
  if (m.error) html += `<div class="kv"><b>Error</b><span class="terr">${esc(m.error)}</span></div>`;
  if ((m.models || []).length) {
    html += `<details open style="margin-top:8px"><summary>Remaining today per model (${m.models.length})</summary><table style="margin-top:6px"><thead><tr><th>Model</th><th>Remaining</th><th>Limit</th><th>Used</th><th>Credit price</th></tr></thead><tbody>${
      m.models.map(x => `<tr><td>${esc(x.model)}</td><td><b>${x.remaining ?? '—'}</b></td><td>${x.limit ?? '—'}</td><td>${x.used ?? 0}</td><td>${x.price === 0 ? '<b class="tok">Free</b>' : (x.price ?? '—')}</td></tr>`).join('')}</tbody></table></details>`;
  }
  $('dr-body').innerHTML = html;
}

// ---------- Usage history ----------
function fillHistoryCredOptions(list) {
  const sel = $('hist-cred');
  if (!sel) return;
  const cur = sel.value;
  const opts = ['<option value="">All accounts</option>'].concat(
    (list || []).map(t => {
      const m = t.meta || {};
      const label = m.email || m.name || t.token_masked;
      return `<option value="${esc(t.id)}">${esc(label)}</option>`;
    })
  );
  sel.innerHTML = opts.join('');
  sel.value = cur;
}
async function loadHistory() {
  const wrap = $('hist-wrap');
  if (!wrap) return;
  wrap.innerHTML = '<div class="empty">Loading…</div>';
  try {
    if (!tokenCache.length) { try { const r = await api('/api/tokens'); tokenCache = r.tokens || []; fillHistoryCredOptions(tokenCache); } catch (e) {} }
    const cred = $('hist-cred') ? $('hist-cred').value : '';
    const q = '/api/account/history?limit=100' + (cred ? '&cred_id=' + encodeURIComponent(cred) : '');
    const d = await api(q);
    const rows = d.records || [];
    wrap.innerHTML = rows.length ? `<table><thead><tr>
        <th>Time</th><th>Account</th><th>Plan</th><th>Remaining today</th><th>Used</th><th>Tokens (7d)</th><th>Streak days</th><th>Result</th>
      </tr></thead><tbody>${
      rows.map(r => `<tr>
        <td style="font-size:12px">${new Date(r.ts).toLocaleString('en-GB', { hour12: false })}</td>
        <td>${esc(r.email || r.name || r.cred_id.slice(0, 8))}</td>
        <td>${esc(r.tier_id || 'Free')}</td>
        <td>${r.daily_remaining ?? '—'} / ${r.daily_limit ?? '—'}</td>
        <td>${r.daily_spent ?? '—'}</td>
        <td>${r.tokens_7d != null ? r.tokens_7d.toLocaleString() : '—'}</td>
        <td>${r.streak_current ?? '—'}</td>
        <td>${r.ok ? badge('Success', 'ok') : badge('Failed', 'err')}</td>
      </tr>`).join('')}</tbody></table>` : '<div class="empty">No records yet — click "Refresh Account Overview" or "Check" on a credential row to generate records</div>';
  } catch (e) { wrap.innerHTML = `<div class="empty">Load failed: ${esc(e.message)}</div>`; }
}

// ---------- Account Overview (renders upstream data) ----------
async function refreshAccountOverview() {
  $('overview-wrap').innerHTML = '<div class="empty">Fetching… (upstream may take a few seconds)</div>';
  try {
    const d = await api('/api/account/overview');
    $('overview-wrap').innerHTML = renderOverview(d);
  } catch (e) {
    const m = String(e.message || '');
    $('overview-wrap').innerHTML = `<div class="empty">Fetch failed: ${esc(m)}${m.includes('Cookie') ? ' — please import credentials above first' : ''}</div>`;
  }
}
function renderOverview(d) {
  const id = (d.identity && d.identity.user) || {};
  let html = '';
  // Identity
  html += '<div class="row" style="gap:12px;margin-bottom:14px">';
  if (id.image) html += `<img src="${esc(id.image)}" style="width:44px;height:44px;border-radius:50%" onerror="this.style.display='none'">`;
  html += `<div><div style="font-size:16px;font-weight:600">${esc(id.name || 'Unnamed account')}</div>
    <div style="font-size:12px;color:var(--muted)">${esc(id.email || '')}${id.id ? ' · User ID ' + esc(String(id.id).slice(0, 8)) + '…' : ''}</div></div>`;
  if (d.identity && d.identity.expires) html += `<span style="flex:1"></span><span style="font-size:12px;color:var(--muted)">Credential valid until ${new Date(d.identity.expires).toLocaleString('en-GB', { hour12: false })}</span>`;
  html += '</div>';

  // Usage stats
  const u = d.usage || {};
  if (u.streak || u.recent) {
    html += '<div style="border-top:1px solid var(--border);padding-top:12px;margin-bottom:4px"><b style="font-size:14px">📊 Usage Stats</b></div>';
    if (u.streak) html += `<p style="font-size:13px;margin-top:6px">🔥 Streak: <b class="tok">${u.streak.current ?? 0}</b> days (longest ${u.streak.longest ?? 0} days) · Total active: <b>${u.allTimeActiveDays ?? 0}</b> days</p>`;
    const r = u.recent || {};
    if (r.totalTokens != null) html += `<p style="font-size:13px;color:var(--muted);margin-top:4px">Last ${r.days ?? 7} days: <b>${r.messages ?? 0}</b> messages · Input <b>${(r.inputTokens || 0).toLocaleString()}</b> · Output <b>${(r.outputTokens || 0).toLocaleString()}</b> · Cache <b>${(r.cacheReadTokens || 0).toLocaleString()}</b> · Total <b>${(r.totalTokens || 0).toLocaleString()}</b> tokens</p>`;
    if ((u.sessionsByModel || []).length) {
      html += `<table style="margin-top:8px"><thead><tr><th>Model</th><th>Sessions</th><th>Units Used</th></tr></thead><tbody>${u.sessionsByModel.map(m => `<tr><td>${esc(m.model)}</td><td>${m.sessions}</td><td>${m.units}</td></tr>`).join('')}</tbody></table>`;
    }
  }

  // Daily quota
  const q = d.quota || {};
  const fb = q.freebucks || {};
  const daily = fb.daily || {};
  const tierId = (d.subscription && d.subscription.subscription && d.subscription.subscription.tierId) || null;
  html += '<div style="border-top:1px solid var(--border);padding-top:12px;margin-top:12px"><b style="font-size:14px">💰 Daily Quota</b></div>';
  html += `<p style="font-size:13px;margin-top:6px">Account tier <b>${esc(q.accessTier || '—')}</b> · Subscription plan <b>${esc(tierId || 'Free')}</b></p>`;
  if (daily.limit != null) {
    const pct = daily.limit > 0 ? Math.round(((daily.remaining || 0) / daily.limit) * 100) : 0;
    const reset = daily.resetAt ? new Date(daily.resetAt).toLocaleString('en-GB', { hour12: false }) : '—';
    html += `<p style="font-size:13px;margin-top:4px">Credits: remaining <b class="tok">${daily.remaining ?? '—'}</b> / ${daily.limit ?? '—'} (used ${daily.spent ?? 0}) · Reset <b>${reset}</b> <span style="color:var(--muted)">(midnight Pacific Time, refreshes automatically the next day)</span></p>`;
    html += `<div style="height:8px;background:#21262d;border-radius:4px;overflow:hidden;margin:8px 0 10px"><div style="height:100%;width:${pct}%;background:${pct > 50 ? 'var(--ok)' : pct > 20 ? 'var(--warn)' : 'var(--err)'}"></div></div>`;
  }
  const prices = fb.prices || {};
  const rl = q.rateLimitsByModel || {};
  const models = Object.keys(rl);
  if (models.length) {
    html += `<table><thead><tr><th>Model</th><th>Remaining Today</th><th>Limit</th><th>Used</th><th>Credit Price</th><th>Next Reset</th></tr></thead><tbody>${
      models.map(m => {
        const v = rl[m] || {};
        const remain = v.limit != null ? Math.max(0, (v.limit || 0) - (v.recentCount || 0)) : '—';
        const price = prices[m] != null ? (prices[m] === 0 ? '<b class="tok">Free</b>' : prices[m]) : '—';
        const reset = v.resetAt ? new Date(v.resetAt).toLocaleString('en-GB', { hour12: false, month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) : '—';
        const pool = v.poolLabel ? ` <span style="color:var(--muted);font-size:11px">${esc(v.poolLabel)}</span>` : '';
        return `<tr><td>${esc(m)}${pool}</td><td><b>${remain}</b></td><td>${v.limit ?? '—'}</td><td>${v.recentCount ?? 0}</td><td>${price}</td><td style="font-size:12px">${reset}</td></tr>`;
      }).join('')}</tbody></table>`;
  }
  const extra = Object.entries(prices).filter(([m]) => !rl[m]);
  if (extra.length) {
    html += `<details style="margin-top:8px"><summary>Other model credit prices (${extra.length})</summary><div style="margin-top:6px">${extra.map(([m, p]) => `<span class="chip">${esc(m)}: ${p === 0 ? 'Free' : p}</span>`).join('')}</div></details>`;
  }

  // Credential and refresh time
  const c = d.credential || {};
  html += `<div style="font-size:12px;color:var(--muted);margin-top:12px;border-top:1px solid var(--border);padding-top:8px">Current credential ${esc(c.token_masked || '')} · Source ${esc(c.source || '')}${c.added_at ? ' · Added ' + new Date(c.added_at).toLocaleString('en-GB', { hour12: false }) : ''} · Data updated at ${d.fetched_at ? new Date(d.fetched_at).toLocaleTimeString('en-GB', { hour12: false }) : '—'}</div>`;
  return html;
}
async function refreshCredential() {
  toast('Running credential keep-alive check…');
  try {
    const r = await api('/api/account/refresh', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' });
    toast(r.message || (r.ok ? 'Credential valid' : 'Credential may have expired'), 7000);
    if (r.ok) refreshAccountOverview();
  } catch (e) { toast('Check failed: ' + e.message, 6000); }
}

// ---------- Skills ----------
let editingSkillId = null;
let skillsCache = [];   // Indexed reference, avoids splicing user data into inline JS (prevents XSS / quote breakage)
async function refreshSkills() {
  try {
    const d = await api('/api/skills');
    const skills = d.skills || [];
    skillsCache = skills;
    $('roster-info').textContent = `roster preview ~${d.roster_tokens ?? '—'} tokens (injection budget ${d.max_roster_tokens ?? 2000})`;
    $('skills-wrap').innerHTML = skills.length ? `<table><thead><tr><th>Name</th><th>Description</th><th>Source</th><th>Status</th><th>Actions</th></tr></thead><tbody>${
      skills.map((s, i) => `<tr>
        <td>${esc(s.name)} ${s.builtin ? badge('Built-in', 'dim') : ''}</td>
        <td style="max-width:340px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(s.description)}">${esc(s.description)}</td>
        <td>${esc(s.source || 'local')}</td>
        <td>${s.enabled ? badge('Enabled', 'ok') : badge('Disabled', 'dim')}</td>
        <td class="row">
          <button class="ghost sm" onclick="toggleSkillAt(${i})">${s.enabled ? 'Disable' : 'Enable'}</button>
          ${s.builtin ? '<span style="color:var(--muted);font-size:12px">Built-in items cannot be edited</span>' : `<button class="ghost sm" onclick="editSkillAt(${i})">Edit</button><button class="ghost sm" onclick="delSkillAt(${i})">Delete</button>`}
        </td></tr>`).join('')}</tbody></table>` : '<div class="empty">Skill library is empty (built-in skills are written automatically on first launch)</div>';
  } catch (e) { $('skills-wrap').innerHTML = `<div class="empty">Load failed: ${esc(e.message)}</div>`; }
}
function toggleSkillAt(i) { const s = skillsCache[i]; if (s) toggleSkill(s.id, !s.enabled); }
function delSkillAt(i) { const s = skillsCache[i]; if (s) delSkill(s.id); }
function editSkillAt(i) {
  const s = skillsCache[i];
  if (!s) return;
  editingSkillId = s.id;
  $('skill-editor-title').textContent = 'Edit skill: ' + s.name;
  $('sk-name').value = s.name; $('sk-desc').value = s.description; $('sk-body').value = s.body || '';
  $('skill-editor').style.display = '';
  $('gate-result').textContent = '';
}
function newSkill() {
  editingSkillId = null;
  $('skill-editor-title').textContent = 'New skill';
  $('sk-name').value = ''; $('sk-desc').value = ''; $('sk-body').value = '';
  $('skill-editor').style.display = '';
  $('gate-result').textContent = '';
}
function editSkill(jsonStr) {
  const s = JSON.parse(jsonStr);
  editingSkillId = s.id;
  $('skill-editor-title').textContent = 'Edit skill: ' + s.name + (s.builtin ? ' (built-in, body only editable)' : '');
  $('sk-name').value = s.name; $('sk-desc').value = s.description; $('sk-body').value = s.body || '';
  $('skill-editor').style.display = '';
}
async function saveSkill() {
  const name = $('sk-name').value.trim(), desc = $('sk-desc').value.trim(), body = $('sk-body').value;
  if (!name || !desc) { toast('Name and description cannot be empty'); return; }
  let force = false;
  try {
    // Quality gate before saving: show issues first, force-save after user confirms
    const g = await api('/api/skills/gate', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ body: name + '\n' + desc + '\n' + body }) });
    const issues = g.issues || [];
    if (issues.length) {
      $('gate-result').innerHTML = '<span class="terr">Quality gate found issues:</span><br>' + issues.map(x => '• ' + esc(x)).join('<br>');
      if (!confirm('Quality gate found ' + issues.length + ' issue(s):\n\n' + issues.join('\n') + '\n\nForce save anyway?')) return;
      force = true;
    } else {
      $('gate-result').textContent = 'Quality gate passed ✅';
    }
    await api('/api/skills', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id: editingSkillId, name, description: desc, body, force }) });
    $('skill-save-result').textContent = '✅ Saved';
    $('skill-editor').style.display = 'none';
    refreshSkills();
  } catch (e) { $('skill-save-result').innerHTML = `<span class="terr">Save failed: ${esc(e.message)}</span>`; }
}
async function toggleSkill(id, enabled) {
  try { await api('/api/skills/toggle', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id, enabled }) }); refreshSkills(); }
  catch (e) { toast('Operation failed: ' + e.message); }
}
async function delSkill(id) {
  if (!confirm('Delete this skill?')) return;
  try { await api('/api/skills/delete', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id }) }); refreshSkills(); }
  catch (e) { toast('Delete failed: ' + e.message); }
}

// ---------- Memory ----------
let memCache = [];
let memEnabled = false;
function kindLabel(k) { return ({ preference: 'Preference', correction: 'Correction', habit: 'Habit', project: 'Project', feedback: 'Feedback' })[k] || k; }
async function refreshMemory() {
  try {
    const d = await api('/api/memory');
    const s = d.stats || {};
    memEnabled = !!d.enabled;
    const t = $('mem-toggle');
    if (t.checked !== memEnabled) t.checked = memEnabled;
    $('mem-toggle-state').textContent = memEnabled ? 'Enabled' : 'Disabled (default)';
    $('mem-toggle-state').style.color = memEnabled ? 'var(--ok)' : 'var(--muted)';
    $('mem-stats').innerHTML = `Total <b>${s.total ?? 0}</b> · Stable facts ${s.static_count ?? 0} · Corrections ${s.corrections ?? 0}`;
    const items = d.memories || [];
    memCache = items;
    $('mem-wrap').innerHTML = items.length ? `<table><thead><tr><th>Type</th><th>Title</th><th>Content</th><th>Status</th><th>Actions</th></tr></thead><tbody>${
      items.map((m, i) => `<tr>
        <td>${esc(kindLabel(m.kind))}</td>
        <td>${esc(m.title)}</td>
        <td style="max-width:420px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(m.content)}">${esc(m.content)}</td>
        <td>${m.is_static ? badge('Stable', 'ok') : badge('Recent', 'dim')}</td>
        <td class="row">
          <button class="ghost sm" onclick="toggleMemStaticAt(${i})">${m.is_static ? 'Mark Recent' : 'Mark Stable'}</button>
          <button class="ghost sm" onclick="delMemAt(${i})">Delete</button>
        </td></tr>`).join('')}</tbody></table>` : '<div class="empty">No memories yet — they accumulate automatically with normal use, or click "Add Manually"</div>';
  } catch (e) { $('mem-wrap').innerHTML = `<div class="empty">Load failed: ${esc(e.message)}</div>`; }
}
async function toggleMemoryEnabled() {
  const t = $('mem-toggle');
  const target = t.checked;
  t.disabled = true;
  try {
    const r = await api('/api/memory/toggle', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ enabled: target }) });
    memEnabled = !!(r && r.enabled !== undefined ? r.enabled : target);
    if (t.checked !== memEnabled) t.checked = memEnabled;
    $('mem-toggle-state').textContent = memEnabled ? 'Enabled' : 'Disabled (default)';
    $('mem-toggle-state').style.color = memEnabled ? 'var(--ok)' : 'var(--muted)';
    toast((r && r.message) || (memEnabled ? 'Memory layer enabled' : 'Memory layer disabled'), 5000);
  } catch (e) {
    t.checked = memEnabled; // roll back on failure
    toast('Toggle failed: ' + e.message);
  } finally { t.disabled = false; }
}
function newMemory() {
  $('mem-editor').style.display = '';
  $('mem-title').value = ''; $('mem-content').value = ''; $('mem-save-result').textContent = '';
}
async function saveMemory() {
  const title = $('mem-title').value.trim(), content = $('mem-content').value.trim(), kind = $('mem-kind').value;
  if (!title || !content) { toast('Title and content cannot be empty'); return; }
  try {
    await api('/api/memory', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ kind, title, content, is_static: false }) });
    $('mem-save-result').textContent = '✅ Saved';
    $('mem-editor').style.display = 'none';
    refreshMemory();
  } catch (e) { $('mem-save-result').innerHTML = `<span class="terr">Save failed: ${esc(e.message)}</span>`; }
}
function toggleMemStaticAt(i) { const m = memCache[i]; if (m) setMemStatic(m.id, !m.is_static); }
function delMemAt(i) { const m = memCache[i]; if (m) delMemory(m.id); }
async function setMemStatic(id, v) {
  try { await api('/api/memory/static', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id, is_static: v }) }); refreshMemory(); }
  catch (e) { toast('Operation failed: ' + e.message); }
}
async function delMemory(id) {
  if (!confirm('Delete this memory?')) return;
  try { await api('/api/memory/delete', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id }) }); refreshMemory(); }
  catch (e) { toast('Delete failed: ' + e.message); }
}

// ---------- Logs ----------
let logEvents = [];
let logSource = null;
async function initLogs() {
  if (logSource) return;
  try {
    const d = await api('/api/logs/recent?limit=100');
    logEvents = d.events || [];
    bindLogScroll();
    renderLogs();
    logSource = new EventSource('/api/logs/stream' + (apiKey() ? '?key=' + encodeURIComponent(apiKey()) : ''));
    logSource.onmessage = (e) => {
      try { const ev = JSON.parse(e.data); logEvents.push(ev); if (logEvents.length > 1000) logEvents = logEvents.slice(-1000); renderLogs(); } catch (err) {}
    };
    logSource.onerror = () => { /* automatic reconnection handled by the browser */ };
  } catch (e) { $('logbox').innerHTML = `<div class="empty">Log load failed: ${esc(e.message)}</div>`; }
}
let logPaused = false;
function visibleLogs() {
  const filter = $('log-filter')?.value || '';
  return (logEvents || []).filter(e => !filter || e.level === filter);
}
function setLogLevel(btn) {
  document.querySelectorAll('#log-level-seg button').forEach(b => b.classList.toggle('active', b === btn));
  const f = $('log-filter'); if (f) f.value = btn.dataset.level || '';
  renderLogs();
}
function toggleLogPause() {
  logPaused = !logPaused;
  const b = $('log-pause-btn'); if (b) { b.textContent = logPaused ? '▶ Resume scroll' : '⏸ Pause scroll'; b.classList.toggle('active', logPaused); }
  if (!logPaused) { const box = $('logbox'); if (box && logEvents.length) box.scrollTop = box.scrollHeight; }
}
function exportLogs() {
  const events = visibleLogs();
  if (!events.length) { toast('Current buffer is empty, nothing to export', 2500); return; }
  const blob = new Blob([JSON.stringify({ exported_at: new Date().toISOString(), count: events.length, logs: events }, null, 2)], { type: 'application/json' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob); a.download = 'freebuff2api-logs-' + new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-') + '.json'; a.click();
  URL.revokeObjectURL(a.href); toast('Exported ' + events.length + ' log entries');
}
function renderLogs() {
  const events = visibleLogs();
  const box = $('logbox');
  if (!box) return;
  // Error count badge + accessibility announcement (update hidden node only when count changes, to avoid flooding)
  const errCount = (logEvents || []).filter(e => e && e.level === 'error').length;
  const ec = $('log-err-count'); if (ec) { ec.textContent = errCount; ec.style.display = errCount ? '' : 'none'; }
  const sr = $('log-sr');
  if (sr && sr.textContent !== 'Loaded ' + logEvents.length + ' log entries, ' + errCount + ' error(s)') {
    sr.textContent = 'Loaded ' + logEvents.length + ' log entries, ' + errCount + ' error(s)';
  }
  // Large-list windowed rendering (v0.8): renders only the visible area + top/bottom buffer, no lag above 500 entries
  const ROW_H = 22; // approximate height of a single log row (px)
  const BUFFER = 40; // number of buffer rows above/below
  const viewport = box.clientHeight || 460;
  const visible = Math.ceil(viewport / ROW_H) + BUFFER * 2;
  const total = events.length;
  const topPad = Math.floor(box.scrollTop / ROW_H) || 0;
  const start = Math.max(0, topPad - BUFFER);
  const end = Math.min(total, start + visible);
  const slice = events.slice(start, end);
  const html = total === 0
    ? '<div class="empty">No logs yet</div>'
    : `<div style="height:${start * ROW_H}px"></div>` + slice.map(e =>
        `<div class="lv-${esc(e.level)}">[${fmtTime(e.ts)}] ${esc(e.level).toUpperCase()} ${esc(e.kind)}${e.req_id ? ' #' + esc(e.req_id) : ''} — ${esc(e.message)}</div>`
      ).join('') + `<div style="height:${(total - end) * ROW_H}px"></div>`;
  box.innerHTML = html;
  // Whether to stick to bottom (auto-scroll): forced off while paused; otherwise kept only if previously at the bottom
  const stick = !logPaused && box._stick !== false;
  if (stick && total > 0) box.scrollTop = box.scrollHeight;
}
function clearLogs() { logEvents = []; renderLogs(); }
// windowed scroll listener (throttled: re-render when scroll stops/changes)
function bindLogScroll() {
  const box = $('logbox'); if (!box || box._scrollBound) return;
  box._scrollBound = true;
  box.addEventListener('scroll', () => {
    box._stick = !logPaused && (box.scrollTop + box.clientHeight >= box.scrollHeight - 24);
    // Re-render on scroll is only needed once windowed rendering kicks in (total count near viewport + buffer);
    // threshold aligns with renderLogs' visible-area algorithm (≈ first non-visible row past the bottom buffer)
    if (logEvents.length > 100) { clearTimeout(box._rt); box._rt = setTimeout(renderLogs, 60); }
  }, { passive: true });
}

// ---------- Health Check ----------
async function refreshDoctor() {
  $('doctor-wrap').innerHTML = '<div class="empty">Checking…</div>';
  try {
    const d = await api('/api/doctor');
    const checks = d.checks || [];
    const icon = (s) => s === 'ok' ? '<span class="tok">✅ ok</span>' : s === 'fault' ? '<span class="terr">❌ fault</span>' : s === 'fact' ? '<span class="twarn">ℹ️ fact</span>' : '<span style="color:var(--muted)">◻ not checked</span>';
    $('doctor-wrap').innerHTML = checks.map(c => `<div class="doctor-item"><div class="st">${icon(c.state)}</div><div style="flex:1"><b>${esc(c.label)}</b><div style="color:var(--muted);margin-top:2px">${esc(c.detail)}</div>${c.fix ? `<div style="color:var(--accent);margin-top:2px">→ ${esc(c.fix)}</div>` : ''}</div></div>`).join('');
  } catch (e) { $('doctor-wrap').innerHTML = `<div class="empty">Health check failed: ${esc(e.message)}</div>`; }
}

// ---------- Credential Health Dashboard (v0.9 §2.1) ----------
function stateBadge(s) {
  if (s === 'closed' || s === 'half_open') return s === 'half_open' ? '<span class="badge warn">half</span>' : '<span class="badge ok">closed</span>';
  if (s === 'half') return '<span class="badge warn">half</span>';
  return '<span class="badge err">open</span>';
}
async function refreshHealth() {
  const w = $('health-wrap'); if (!w) return;
  w.innerHTML = '<div class="empty">Loading…</div>';
  try {
    const d = await api('/api/accounts/health');
    const rows = d.accounts || [];
    if (!rows.length) { w.innerHTML = '<div class="empty">No credential health data yet (import a Cookie / Bearer token first)</div>'; return; }
    w.innerHTML = '<div style="overflow-x:auto"><table style="width:100%"><thead><tr><th>Credential</th><th>Type</th><th>Circuit</th><th>Score</th><th>Failures</th><th>Cooldown</th><th>Last Error / Timeline</th></tr></thead><tbody>' +
      rows.map(r => {
        const hist = (r.history || []).slice(0, 10);
        const tl = hist.length ? `<details style="margin:4px 0 0"><summary>Timeline (${hist.length})</summary>${hist.map(h => `<div class="kv" style="font-size:12px"><b>${fmtTime(h.ts)} ${h.type === 'ok' ? '✅' : '❌'}</b>${esc(h.detail || '')}</div>`).join('')}</details>` : '';
        return `<tr><td>${esc((r.masked || r.id || '').slice(0, 26))}</td><td>${esc(r.kind || '—')}</td><td>${stateBadge(r.circuit_state)}</td><td>${Number(r.health_score || 0).toFixed(0)}</td><td>${r.trips || 0}</td><td>${esc(r.cooldown_until ? fmtTime(r.cooldown_until) : '—')}</td><td style="max-width:240px"><div style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(r.last_error || '—')}</div>${tl}</td></tr>`;
      }).join('') + '</tbody></table></div>';
  } catch (e) { w.innerHTML = `<div class="empty">Health dashboard load failed: ${esc(e.message)}</div>`; }
}

// ---------- Today's Recommendations (v0.9 §2.2) ----------
async function loadRecommend() {
  const w = $('recommend-wrap'); if (!w) return;
  try {
    const r = await fetch('/api/account/balance', { headers: apiKey() ? { authorization: 'Bearer ' + apiKey() } : {} });
    if (!r.ok) { $('recommend-panel').style.display = 'none'; return; }
    const b = await r.json();
    const mr = b.model_remaining || {};
    const rows = Object.entries(mr);
    if (!rows.length) { $('recommend-panel').style.display = 'none'; return; }
    // v0.10: /v1/models meta → availableAt / "unverified against policy" label (defensive: skip rendering this column if meta is missing)
    let metaById = {};
    try { const mm = await api('/v1/models'); if (Array.isArray(mm.meta)) mm.meta.forEach(x => { if (x && x.id) metaById[x.id] = x; }); } catch (e6) {}
    rows.sort((x, y) => (x[1].usable_today === -1 ? 0 : 1) - (y[1].usable_today === -1 ? 0 : 1));
    const top = rows.slice(0, 5);
    $('recommend-panel').style.display = '';
    const availCell = (m) => {
      const meta = metaById[m];
      if (!meta) return '<span class="badge dim" title="Dynamically added upstream / not yet in the policy table">Unverified</span>';
      if (meta.available === false) {
        if (meta.available_at) {
          let t = '';
          try { t = new Date(meta.available_at).toLocaleString('en-GB', { hour12: false }); } catch (e7) {}
          return '<span class="badge warn" title="' + esc(meta.available_at || '') + '">Paused/peak, expected to resume ' + esc(t) + '</span>';
        }
        return '<span class="badge warn">Paused/removed</span>';
      }
      return '<span class="badge ok">Available</span>';
    };
    w.innerHTML = '<div style="font-size:12px;color:var(--muted);margin-bottom:6px">Sorted by upstream rateLimitsByModel remaining count today (paused/peak models sorted last)</div>' +
      '<div style="overflow-x:auto"><table style="width:100%"><thead><tr><th>Model</th><th>Remaining Today</th><th>Credit Price</th><th>Availability</th></tr></thead><tbody>' +
      top.map(([m, v]) => `<tr><td>${esc(m)}</td><td><b>${v.usable_today === -1 ? 'Unlimited' : esc(String(v.usable_today ?? '—'))}</b></td><td>${v.price === 0 ? '<b class="tok">Free</b>' : esc(String(v.price ?? '—'))}</td><td>${availCell(m)}</td></tr>`).join('') +
      '</tbody></table></div>';
  } catch (e) { $('recommend-panel').style.display = 'none'; }
}

// ---------- Data Migration (v0.9 §2.3) ----------
async function exportConfig() {
  const st = $('migrate-status'); if (st) st.textContent = 'Exporting…';
  try {
    const d = await api('/api/export');
    const blob = new Blob([JSON.stringify({ data: d.data }, null, 2)], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob); a.download = 'freebuff2api-export-' + new Date().toISOString().slice(0, 10) + '.json'; a.click();
    URL.revokeObjectURL(a.href);
    if (st) st.textContent = 'Exported (includes ' + (d.data.tokens || []).length + ' credential(s))';
    toast('Config exported');
  } catch (e) { if (st) st.textContent = ''; toast('Export failed: ' + e.message, 3500); }
}
async function importConfig(files) {
  const f = files && files[0]; const st = $('migrate-status');
  if (!f) return;
  let parsed;
  try { parsed = JSON.parse(await f.text()); }
  catch (e) { if (st) st.textContent = ''; toast('JSON parse failed', 3000); return; }
  const data = parsed && parsed.data ? parsed.data : parsed;
  const tokens = (data && data.tokens) || [];
  const skills = (data && data.skills) || [];
  const mem = data && data.memory_enabled;
  const okc = confirm('Will import:\n· Credentials: ' + tokens.length + ' (overwrites local tokens.json, auto-backed up before import)\n· Skill enabled states: ' + skills.length + ' item(s)\n· Memory toggle: ' + (mem === undefined ? 'unchanged' : (mem ? 'on' : 'off')) + '\n· config whitelisted fields (api_keys/auth_tokens will not be overwritten)\n\nContinue?');
  if (!okc) { if (st) st.textContent = ''; return; }
  if (st) st.textContent = 'Importing…';
  try {
    const r = await api('/api/import', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ data }) });
    const s = r.imported || {};
    const msg = 'Import complete: tokens ' + (s.tokens ?? 0) + (s.backed_up_to ? ', backed up to ' + s.backed_up_to : '');
    if (st) st.textContent = msg; toast('Import successful');
    refreshTokens(); loadSettings();
  } catch (e) { if (st) st.textContent = ''; toast('Import failed: ' + e.message, 4000); }
}

// ---------- Startup ----------
initTabKeyboard();
refreshOverview();
loadRecommend();
loadGuide();
// Extension may only activate after page load; probe a few extra times after startup (up to 5 tries, stop once detected)
renderExtStatus();
(() => {
  let tries = 0;
  const t = setInterval(() => { pingExtension(); if (++tries >= 5 || extId) clearInterval(t); }, 2000);
})();
// hash routing: tray "System Health Check" → /#doctor (responds both on load and on hash change)
function applyHashTab() {
  const h = location.hash.replace(/^#/, '');
  if (h && document.querySelector('nav button[data-tab="' + h + '"]')) showTab(h);
}
if (location.hash === '#doctor') showTab('doctor');
window.addEventListener('hashchange', applyHashTab);
startOverviewTimer();
// The embedded login window needs WebView2 (Windows only): hide Option A when the gateway runs elsewhere (Linux/Docker/Render)
fetch('/healthz').then(r => r.json()).then(h => { if (h.os && h.os !== 'windows') { const e = $('wizard-embed'); if (e) e.style.display = 'none'; } }).catch(() => {});
</script>
</body>
</html>"##;
