# 4. High Availability & Security Hardening (v0.8 P1/P2)

> Runtime stability + security boundaries, without changing the protocol layer or refactoring.

---

## 4.1 Runtime robustness

| Item | Current state (evidence) | Improvement |
|----|-------------|------|
| Upstream session auto-cleanup | Already present (`thread_cleanup_loop`, default 1h/24h) | Keep as-is; add a capacity cap + TTL to the `web_threads.json` binding table (see 2.3-#6) |
| Memory store WAL | Already enabled | No change; add a backup reminder to the README |
| Telemetry write backpressure | `TelemetryWriter` queue + drop counter | Panel "health check" already shows dropped count; add a threshold alert (panel turns red at >1000) |
| Request body size limit | `/v1/uploads` has an explicit 20MB limit | Other endpoints have no explicit body limit (axum defaults to 2MB) — `/v1/chat/completions` with large context could hit 413; confirm client behavior and return a readable error |
| Upload MIME | Already passed through + inferred from extension | Add size/type stats (kind already tracked); optionally allow an `x-max-upload-mb` header override |
| Port conflicts | start.bat detects it + desktop popup | Have the gateway itself give a **clear error message** when binding `listen_addr` fails at startup (currently `?` bubbles up an anyhow error, which is uninformative) |
| Graceful shutdown | None | Add `SIGINT/SIGTERM` handling: stop keep-alive loops, wait for in-flight requests (optional, low priority) |

## 4.2 Security hardening (review: current defenses are already solid, filling in details)

**Existing defenses (verified, keep unchanged)**:
- Admin endpoint auth `admin_authorized` (local direct connection / API key dual mode)
- CSRF defense `origin_allowed` (browser Origin whitelist, including extension Origins)
- Non-loopback listening forces api_keys (hard validation in config validate)
- Request key redaction (`x-api-key`/authorization → `sk***…`)
- Upload error body truncation (prevents leaking internal details)
- Panel API key stored only in localStorage

**Recommended enhancements (P2)**:

| Item | Description |
|----|----|
| Token rotation | The panel-generated API key supports "rotation": generate a new key while **keeping the old key valid for a 60s grace period** before removal, to avoid a 401 at the moment of switching |
| Panel CSP | Add `Content-Security-Policy: default-src 'self'` to the embedded page (currently no CSP header); the local panel has no external resources, so it can be tightened directly |
| Security response headers | Add `X-Content-Type-Options: nosniff` and `Referrer-Policy: strict-origin-when-cross-origin` uniformly to `/ui`, `/healthz`, `/v1/*` responses (lightweight, doesn't touch existing logic) |
| Upload MIME whitelist | Currently any content-type is passed through as-is; consider rejecting or escaping "text-injection-prone" MIME types (html/svg) on storage (storageId only stores the storage key, so risk is low, but html/svg uploaded as a document could XSS the display side) |
| Credential redaction | `tokens.json` permissions (Windows inherited ACLs may be too permissive) — recommend documenting that it should live in a protected directory; the desktop build is already under userData, so a hint could be added |
| Log privacy | Telemetry's `error_excerpt` may contain fragments of user messages; add a "log redaction toggle" `redact_logs` (default on? default off needs weighing) — **recommend defaulting to on, redacting only token strings within headers/bodies** |

## 4.3 Configuration robustness

| Item | Improvement |
|----|------|
| `config.json` atomic write | Already implemented (temp file + rename, `atomic_write_json`) — keep as-is; new config items should also use atomic writes |
| Extended config validation | Add: `listen_addr` port validity, `api_keys` format (optional hint for the `sk-` prefix), `ad_providers` whitelist of valid values |
| Config hot reload | Do not introduce file watching (too complex); keep the current "panel writes back + restart takes effect" semantics; same for new items |

## 4.4 Desktop shell (Electron) hardening

| Item | Current state | Improvement |
|----|----|------|
| Gateway crash self-healing | Present (auto-restart on exit + failure popup) | Keep as-is; make the restart interval (2s) configurable |
| Login window security | `nodeIntegration:false, contextIsolation:true` | Already up to standard; add `sandbox:true` (Electron default security) + keep `webSecurity` at default |
| Update flow | electron-updater points to GitHub Release | Keep as-is; add a tray bubble notification on "check for update failed" (the error event already exists) |
| Multi-instance | None | Add `app.requestSingleInstanceLock()`: a second launch focuses the existing window, avoiding two gateways fighting over the same port |

## 4.5 Acceptance criteria

- [ ] Uploading an oversized file (>2MB, non-uploads endpoint) produces a readable 413 message
- [ ] A clear error message appears when port binding fails (including the occupying-process hint)
- [ ] Panel responses include nosniff / Referrer-Policy
- [ ] A second desktop launch focuses the existing window instead of relaunching the gateway
- [ ] Logs/telemetry contain no plaintext full cookies (redaction is effective)
