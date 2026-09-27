# Freebuff2API One-Click Login Extension (Chrome / Edge)

The browser edition can't read `__Secure-next-auth.session-token` with a page script — it's an **HttpOnly** cookie, and browser security policy forbids `document.cookie` access to it. This extension reads it via the official `chrome.cookies` API (the browser's only legitimate way to do so), and automatically sends the login credential to the local gateway.

## Installation (30 seconds)

1. Confirm this directory exists (`Freebuff2API/browser-extension/`)
2. Open your browser's extensions page:
   - Chrome: enter `chrome://extensions` in the address bar
   - Edge: enter `edge://extensions` in the address bar
3. Turn on **Developer mode** in the top right
4. Click **Load unpacked** → select this directory (`browser-extension`)
5. The Freebuff2API icon appears in the extensions bar
6. **Refresh the already-open gateway panel page** (so the extension's `bridge.js` injects and completes the handshake)
7. **Recommended for multiple accounts:** on the extension's **Details** page turn on **Allow in Incognito**. "One-click login" then opens a private (Incognito) window, imports the account you log in with, and closes the window. Your existing accounts stay logged in, so there's no need to log out. Never click "Log out" on an account you've imported: that kills its session.

## Method A: panel "one-click login" (recommended — fully automatic)

1. Open the gateway control panel (e.g. `http://127.0.0.1:47821/ui`) → the "Accounts" page
2. Click **🔑 One-click login**
3. If the browser already has a freebuff.com login session → the credential is imported immediately; refresh the panel to see the account
4. If not logged in yet → the extension automatically opens the freebuff.com login page and prompts "please complete GitHub login"; **no further action is needed after logging in successfully** — the extension automatically detects the session and imports it (waits up to 3 minutes)
5. A notification appears: "✅ Automatically imported N credential(s)"

Repeated imports won't duplicate entries (identical values are automatically deduplicated).

## Gateway API key (optional)

When the gateway has `api_keys` configured, the import endpoint validates `Authorization: Bearer <key>`:

- **Clicking "one-click login" from the panel**: the key you filled in at the top of the panel is sent along with the request automatically, no need to fill it in again in the extension.
- **Clicking the extension icon directly**: right-click the extension icon → "Options" → fill in the "Gateway API Key" shown in the panel.
- **Leave it blank if the gateway has no `api_keys` configured**: the extension won't send an `Authorization` header, and local direct connections work as usual.
- A notification saying "🔑 Gateway API key validation is enabled" means the key is missing or incorrect — follow the two steps above and retry.

## Method B: clicking the extension icon (equivalent path)

Clicking the extension icon follows the same flow as above: if a login session can be read, it's imported directly; if not, it automatically opens freebuff.com and waits for login, then imports automatically once logged in. Useful when you don't want to open the panel and just want to store the credential into the gateway first.

## How it works (why it's fully automatic)

- The extension's `bridge.js` is injected as a content script into the local gateway panel page, and uses `window.postMessage` to tell the panel the extension's ID; the panel then connects directly to the extension via `chrome.runtime.sendMessage(extensionId, ...)` (`externally_connectable` only allows `127.0.0.1` / `localhost`).
- When the extension receives a request: it first checks `__Secure-next-auth.session-token`; if absent, it opens freebuff.com and **polls every 2 seconds**, and as soon as it gets the token it immediately POSTs it to the gateway's `/api/tokens/import` (if the gateway has `api_keys` configured, it automatically attaches `Authorization: Bearer <key>`).
- The gateway port is probed in the order `passed in by panel > options page setting > 47821 → 47822 → 8787`; **the port only changes if the connection is refused** — if the gateway responds with any HTTP response (including an error), it stops at that port and reports honestly.

## FAQ

**Clicking "one-click login" in the panel does nothing / says the extension wasn't detected**: confirm the extension is enabled and at version 1.1.0, then **refresh the panel page** (`Ctrl+F5`) to let `bridge.js` re-inject.

**"No login credential found" appears with no login page opening automatically**: often the browser blocked the new tab; manually open [freebuff.com](https://freebuff.com) and log in — the extension will still detect it during polling and import automatically (or click the icon again).

**"Failed to connect to local gateway" appears**: confirm Freebuff2API is running (default port 47821). If you've changed the port: right-click the extension icon → "Options" → enter the port; or click "one-click login" directly from the gateway panel (the panel automatically attaches its own port).

**"Gateway API key validation is enabled" (401) appears**: the gateway has `api_keys` configured. Fill in the key at the top of the panel, and right-click the extension icon → "Options" → "Gateway API Key" and enter the same key; it's attached automatically when clicking "one-click login" from the panel, no manual entry needed.

**"Login wait timed out" appears**: no login was detected within 3 minutes. After finishing login, click the icon or the panel button again to import immediately.

**Does it read my password?** No. The extension only reads cookies under the freebuff.com domain (which include a login session identifier), never touches account passwords, and never sends data anywhere other than your own `127.0.0.1`.

## Permissions

| Permission | Purpose |
|------|------|
| `cookies` | Read the freebuff.com login cookie (only way to read an HttpOnly cookie) |
| `host_permissions: freebuff.com` | Restricted to reading only this domain |
| `host_permissions: 127.0.0.1/localhost` | Can only send to your own local gateway |
| `notifications` | Import result / login reminder notifications |
| `storage` | Remembers your configured gateway port and (optional) gateway API key |
| `content_scripts` (bridge.js, 127.0.0.1/localhost only) | Tells the local gateway panel the extension's ID, enabling a direct panel→extension connection |
| `externally_connectable` (127.0.0.1/localhost only) | Only allows the local gateway panel to message the extension |

## Source files (freely auditable, plain native JS, no dependencies)

| File | Purpose |
|------|------|
| `background.js` | Main flow: reading the cookie, multi-port import, auto-navigation, polling while waiting for login, panel message handling |
| `bridge.js` | content script: panel ↔ extension handshake (only injected on 127.0.0.1 / localhost) |
| `options.html` / `options.js` | port and gateway API key settings page |
