# 3. Panel modernization plan (v0.8 UI/UX)

> Current panel: single HTML + vanilla JS (embedded in src/web.rs at 88.7KB, GitHub Dark style), no build step. **This document is design guidance and landing points only, it does not change any code.**

---

## 3.1 Current assessment (evidence)

- Visual: GitHub Dark (`--bg:#0d1117`), readable but **generic, no branding**; cards/tables/badges are the only component language
- Interaction: in-tab switching, drawer details, real-time SSE logs, toasts, one-click copy -- **everything expected is there**
- Shortfalls:
  1. No fine-grained responsive breakpoints (single column at 920px, so-so experience on phones/narrow windows)
  2. No keyboard-accessibility annotations (focus visibility, ARIA roles, `tabindex` traversal)
  3. No loading/skeleton states (cards flash empty while data is being fetched)
  4. No dark/light theme switching (`prefers-color-scheme` unused)
  5. No feedback on "refresh timing": SSE reconnects, request timeouts, and expired credentials only show up in the logs, no global status bar
  6. New data (memory, skills, accounts) is rebuilt via `innerHTML` -- no virtual scrolling, gets laggy with many entries (heavy memory/log volume)

## 3.2 Design direction (recommended: dark dashboard + branding)

| Dimension | Current | Target |
|------|------|------|
| Color | GitHub Dark default | Branded dark theme: primary color in the `#4f8cff` family + status colors (ok/warn/err) + semantic tokenization (CSS variables already exist, expand into layered tokens) |
| Font | System stack | Introduce a more modern monospace/Chinese font stack (optional: local font subsetting, no external font library) |
| Layout | max-width 1240 single column | Keep single column but add a sticky summary bar (uptime/account count/today's usage updated live); collapse nav into horizontal scroll on narrow screens |
| Components | Cards/tables | Unified component spec: card, table, badge, button, input, empty state, skeleton, toast, drawer, switch, tabs -- **forming a panel component library** (CSS class conventions) |
| Motion | None | Light micro-interactions: hover brightening already exists, add a focus-visible ring, drawer slide-in, toast slide-in, number roll-up (optional) |
| Accessibility | Weak | `aria-label` / `role="tablist"` / keyboard reachability / `:focus-visible` ring / contrast ratio >= 4.5:1 (badges already meet this) |
| Data display | Full `innerHTML` rebuild | Pagination or virtual scrolling for large lists (logs/memory/requests); add empty-state guidance illustrations (emoji + copy + next-step button) |

## 3.3 New/enhanced pages (aligned with the backend API, all using existing endpoints)

| Page | Data source | Content |
|------|--------|------|
| **Overview, enhanced** | `/api/usage/totals` + `/api/guide` + `/healthz` | Top summary bar (uptime/accounts/models/today's usage) + trend sparkline (draws a 7-day mini bar chart from `/api/usage/daily`, pure CSS/SVG, zero dependencies) |
| **Accounts, enhanced** | `/api/tokens` + `/api/account/overview` + `/api/account/history` | Account cards: avatar/nickname/email/plan badge/remaining today/health dot; batch refresh button (concurrent checks via Promise.all) |
| **Real-time logs, enhanced** | `/api/logs/stream` | Auto-scroll toggle, persisted error filter (localStorage), search highlighting, reconnect status bar |
| **Doctor, enhanced** | `/api/doctor` | One-click re-run + expandable per-item fix suggestions + copy diagnostic report (copies as Markdown) |
| **Onboarding guide, enhanced** | `/api/guide` | Add a "copy" button to each config snippet (already present); add C#/Go SDK examples; model list picker (dropdown populated from /v1/models) |
| **New: Settings page** | Existing config APIs | Listen address/port (hint to restart after change), memory toggle (already present), token_saver toggle, skill injection mode/budget, proxy, session cleanup interval -- unified into a UI, writes back to config.json |
| **New: About page** | `env!("CARGO_PKG_VERSION")` + `/healthz` | Version, build info, upstream address, disclaimer, check for updates (bridged to the desktop app) |

## 3.4 Frontend engineering recommendations (no build system added)

- Keep it "build-free": CSS variables + vanilla JS, following the existing style
- **Extract a small utility set**: `esc/badge/fmtTime/api/toast` are already functions -- add `debounce/throttle`, `virtual-list` (simple windowing), `sparkline` (pure SVG)
- **Code organization**: web.rs's embedded string is already 88KB; suggest splitting into `src/web_assets/{index.html,app.js,app.css}`, assembled on the Rust side via `include_str!` (**stays build-free**, it's just file separation, for easier editing and diffs)
- **Compatibility**: all changes remain backward compatible with the existing fetch/SSE interfaces; no new fetch/dependencies introduced

## 3.5 Acceptance checklist (frontend)

- [ ] No overflow or clipping at 320 / 768 / 1024 / 1440 breakpoints
- [ ] Keyboard Tab traversal reaches all buttons/inputs/toggles, focus visible
- [ ] Dark/light (optional) or at least `prefers-reduced-motion` respected
- [ ] Large logs (>1000 entries) scroll smoothly (windowed render)
- [ ] Failed requests show a clear toast + status bar (never silent)
- [ ] Empty states have guidance (go add an account / go import / go enable a skill)
- [ ] Existing E2E assertions (phase_g / phase_i panel-related) all keep passing
