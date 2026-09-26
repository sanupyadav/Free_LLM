// Freebuff2API desktop launcher (Electron shell)
// - Spawns the gateway binary (resources/freebuff2api.exe or target/release/freebuff2api.exe)
// - Waits for HTTP readiness, then loads the local control panel
// - System tray + failure dialog + log persistence + open config/data/log directories
// - One-click OAuth login: opens the freebuff.com login page in a built-in BrowserWindow, then
//   automatically captures the cookie on success and POSTs it to the gateway's /api/tokens/import

const { app, BrowserWindow, Tray, Menu, nativeImage, ipcMain, session, dialog, shell } = require('electron');
const { spawn, execFile } = require('node:child_process');
const http = require('node:http');
const https = require('node:https');
const path = require('node:path');
const fs = require('node:fs');

const GATEWAY_PORT = 47821;
const GATEWAY_URL = `http://127.0.0.1:${GATEWAY_PORT}`;

// Single-instance guard (v0.8): a second launch doesn't relaunch the gateway,
// it just activates and focuses the existing window
const gotTheLock = app.requestSingleInstanceLock();
if (!gotTheLock) {
  app.quit();
} else {
  app.on('second-instance', () => {
    // Existing instance received a second-launch signal -> show and foreground the main window
    if (mainWindow) {
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.show();
      mainWindow.focus();
      // Even if the gateway isn't ready yet, the tray/window can still open the console
      writeLog('[app] Received second-launch signal, activating existing window');
    }
  });
}

let gateway = null;
let mainWindow = null;
let tray = null;
let ready = false;
let logStream = null;
let lastStartupError = null;

// ---------- Log persistence ----------
function logDir() {
  const dir = path.join(app.getPath('userData'), 'logs');
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}
function logPath() {
  return path.join(logDir(), 'gateway.log');
}
function writeLog(line) {
  try {
    if (!logStream) {
      logStream = fs.createWriteStream(logPath(), { flags: 'a' });
    }
    logStream.write(`[${new Date().toISOString()}] ${line}\n`);
  } catch (e) {
    console.error('[log] write failed', e.message);
  }
}

// Locate the gateway binary (install dir or dev dir)
function findGateway() {
  const candidates = [
    path.join(process.resourcesPath || '', 'freebuff2api.exe'), // packaged: resources/freebuff2api.exe
    path.join(path.dirname(process.execPath), '..', 'resources', 'freebuff2api.exe'),
    path.join(app.getAppPath(), '..', 'target', 'release', 'freebuff2api.exe'), // dev
    path.join(__dirname, '..', 'target', 'release', 'freebuff2api.exe'),
    path.join(__dirname, '..', '..', 'target', 'release', 'freebuff2api.exe'),
  ];
  for (const p of candidates) {
    if (p && fs.existsSync(p)) return p;
  }
  return null;
}

// Migration: earlier versions used freebuff2api-desktop as the userData dir name;
// move config/data over on first launch
function migrateLegacyData() {
  try {
    const oldDir = path.join(app.getPath('appData'), 'freebuff2api-desktop');
    const newDir = app.getPath('userData');
    if (oldDir === newDir || !fs.existsSync(oldDir)) return;
    for (const f of ['config.json', 'freebuff2api.sqlite', 'telemetry.sqlite', 'skills.sqlite', 'tokens.json']) {
      const src = path.join(oldDir, f);
      const dst = path.join(newDir, f);
      if (fs.existsSync(src) && !fs.existsSync(dst)) {
        fs.copyFileSync(src, dst);
        writeLog(`[migrate] ${f} migrated from old directory`);
      }
    }
    fs.mkdirSync(path.join(newDir, 'data'), { recursive: true });
    const oldData = path.join(oldDir, 'data');
    if (fs.existsSync(oldData)) {
      for (const f of fs.readdirSync(oldData)) {
        const src = path.join(oldData, f);
        const dst = path.join(newDir, 'data', f);
        if (fs.statSync(src).isFile() && !fs.existsSync(dst)) {
          fs.copyFileSync(src, dst);
          writeLog(`[migrate] data/${f} migrated`);
        }
      }
    }
  } catch (e) {
    writeLog(`[migrate] migration failed (ignored): ${e.message}`);
  }
}

function checkHealth() {
  return new Promise((resolve) => {
    const req = http.get(`${GATEWAY_URL}/healthz`, (res) => {
      res.resume();
      resolve(res.statusCode === 200);
    });
    req.setTimeout(1000, () => { req.destroy(); resolve(false); });
    req.on('error', () => resolve(false));
  });
}

async function waitForGateway(timeoutMs = 15000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (await checkHealth()) return true;
    await new Promise(r => setTimeout(r, 500));
  }
  return false;
}

// Read the user-configured listen port (may differ from the default)
function configuredPort() {
  try {
    const cfgPath = path.join(app.getPath('userData'), 'config.json');
    if (fs.existsSync(cfgPath)) {
      const cfg = JSON.parse(fs.readFileSync(cfgPath, 'utf8'));
      const addr = String(cfg.listen_addr || '');
      const m = addr.match(/:(\d+)$/);
      if (m) return parseInt(m[1], 10);
    }
  } catch (_) { /* ignored: fall back to the default port if config is corrupt */ }
  return GATEWAY_PORT;
}

function startGateway() {
  const exe = findGateway();
  if (!exe) {
    lastStartupError = 'Gateway binary freebuff2api.exe not found (incomplete install? please re-download and reinstall)';
    writeLog(`[ERROR] ${lastStartupError}`);
    return;
  }
  const configPath = path.join(app.getPath('userData'), 'config.json');
  // If the user hasn't configured config.json, use defaults (starts fine with an empty token too)
  if (!fs.existsSync(configPath)) {
    fs.writeFileSync(configPath, JSON.stringify({
      listen_addr: '127.0.0.1:47821',
      upstream_base_url: 'https://www.codebuff.com',
      auth_tokens: [],
      sqlite_path: path.join(app.getPath('userData'), 'freebuff2api.sqlite').replace(/\\/g, '/'),
      http_proxy: '',
      skip_upstream_check: true,
    }, null, 2));
  }
  writeLog(`[start] Launching gateway: ${exe} --config ${configPath}`);
  gateway = spawn(exe, ['--config', configPath], {
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
    cwd: app.getPath('userData'), // relative paths (data/tokens.json etc.) land in the userData dir
  });
  gateway.stdout.on('data', d => { const s = String(d).trim(); console.log('[gateway]', s); writeLog(`[out] ${s}`); });
  gateway.stderr.on('data', d => { const s = String(d).trim(); console.error('[gateway-err]', s); writeLog(`[err] ${s}`); });
  gateway.on('exit', (code) => {
    console.log(`[gateway] exited code=${code}`);
    writeLog(`[exit] code=${code}`);
    if (ready && !app.isQuitting) {
      // Auto-restart the gateway on crash (warn after 3 failures)
      setTimeout(() => {
        startGateway();
        setTimeout(async () => {
          if (!(await checkHealth())) {
            dialog.showErrorBox(
              'Freebuff2API gateway error',
              `The gateway process keeps exiting (last code=${code}).\n\nCommon causes:\n1. Port ${configuredPort()} is in use by another program\n2. The config file is corrupt (%APPDATA%\\freebuff2api\\config.json)\n3. Antivirus software is blocking it\n\nDetailed log: ${logPath()}`
            );
          }
        }, 4000);
      }, 2000);
    }
  });
}

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1280,
    height: 820,
    title: 'Freebuff2API Console',
    icon: path.join(__dirname, 'icons', 'icon.png'),
    autoHideMenuBar: true,
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      preload: path.join(__dirname, 'preload.js'),
    },
  });
  mainWindow.loadURL(`http://127.0.0.1:${configuredPort()}`);
  mainWindow.on('closed', () => { mainWindow = null; });
}

function openConsole() {
  if (!mainWindow) createWindow();
  else mainWindow.show();
}

// Open the console and jump to a given tab (hash routing, to avoid a full-page
// loadURL causing a refresh / lost session)
function openConsoleAt(hashTab) {
  openConsole();
  if (mainWindow) {
    const apply = () => { try { mainWindow.webContents.executeJavaScript(`location.hash='#${hashTab}'`); } catch (_) { /* ignored */ } };
    const wc = mainWindow.webContents;
    // Hook did-finish-load if the page hasn't finished loading yet, otherwise apply immediately
    if (wc && wc.isLoading()) wc.once('did-finish-load', apply);
    else apply();
  }
}

function createTray() {
  const icon = path.join(__dirname, 'icons', 'icon.png');
  let trayIcon = nativeImage.createFromPath(icon);
  if (trayIcon.isEmpty()) {
    trayIcon = nativeImage.createEmpty();
  }
  tray = new Tray(trayIcon.resize({ width: 16, height: 16 }));
  tray.setToolTip('Freebuff2API Gateway');
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: 'Open Console', click: openConsole },
    { label: '➕ One-click login for new account', click: openLoginWindow },
    { type: 'separator' },
    { label: '🩺 System check', click: () => { openConsoleAt('doctor'); } },
    { label: '📄 Open logs', click: () => { shell.openPath(logPath()); } },
    { label: '⚙️ Open config', click: () => { shell.showItemInFolder(path.join(app.getPath('userData'), 'config.json')); } },
    { label: '📁 Open data directory', click: () => { shell.openPath(app.getPath('userData')); } },
    { type: 'separator' },
    { label: '🔄 Check for updates', click: () => { checkForUpdates(); } },
    { label: 'Health check', click: async () => { const ok = await checkHealth(); tray.displayBalloon({ title: 'Freebuff2API', content: ok ? 'Gateway is running normally ✅' : 'Gateway is not responding ❌ (click "🩺 System check" for details)' }); } },
    { type: 'separator' },
    { label: 'Quit', click: () => { app.isQuitting = true; if (gateway) gateway.kill(); app.quit(); } },
  ]));
  tray.on('click', openConsole);
}

// ---------- OAuth one-click login ----------
let loginWindow = null;

function buildCookieHeader(cookies) {
  return cookies
    .filter(c => c.value)
    .map(c => `${c.name}=${c.value}`)
    .join('; ');
}

function importCookiesToGateway(cookieStr) {
  return new Promise((resolve) => {
    const data = JSON.stringify({ cookie: cookieStr });
    const req = http.request({
      host: '127.0.0.1', port: configuredPort(), path: '/api/tokens/import',
      method: 'POST', headers: { 'content-type': 'application/json', 'content-length': Buffer.byteLength(data) },
    }, (res) => {
      let body = '';
      res.on('data', c => body += c);
      res.on('end', () => resolve({ ok: res.statusCode < 400, body }));
    });
    req.on('error', (e) => resolve({ ok: false, body: String(e) }));
    req.write(data);
    req.end();
  });
}

// Capture the freebuff.com session cookie (including the next-auth triplet)
async function captureCookies() {
  const ses = session.fromPartition('persist:freebuff-login');
  const cookies = await ses.cookies.get({ url: 'https://freebuff.com' });
  const cookieStr = buildCookieHeader(cookies);
  if (cookieStr.includes('__Secure-next-auth.session-token')) {
    const result = await importCookiesToGateway(cookieStr);
    return { cookieStr, result };
  }
  return { cookieStr: '', result: { ok: false, body: 'No login session detected' } };
}

function openLoginWindow() {
  if (loginWindow) { loginWindow.show(); return; }
  const ses = session.fromPartition('persist:freebuff-login');
  loginWindow = new BrowserWindow({
    width: 1000, height: 720,
    title: 'Freebuff Login',
    webPreferences: { nodeIntegration: false, contextIsolation: true, session: ses, partition: 'persist:freebuff-login' },
  });
  // Listen for navigation completion: capture the cookie on entering /chat, /account
  // or /web (the login-success signal)
  loginWindow.webContents.on('did-navigate', async (e, url) => {
    if (/freebuff\.com\/(chat|account|web)/.test(url)) {
      // Wait briefly for the cookie to be persisted
      setTimeout(async () => {
        const { result } = await captureCookies();
        if (result.ok) {
          writeLog(`[login] Cookie stored successfully`);
          if (loginWindow) {
            loginWindow.webContents.executeJavaScript(`alert('✅ Login successful, cookie stored automatically!\\nRefresh the gateway panel to see the account')`);
          }
          setTimeout(() => { if (loginWindow) { loginWindow.close(); loginWindow = null; } }, 1500);
        } else {
          // Show a clear message on failure instead of failing silently
          dialog.showMessageBox(loginWindow || mainWindow || undefined, {
            type: 'warning',
            title: 'No login session detected',
            message: 'The page is open, but no login cookie has been captured yet.',
            detail: 'Please make sure you\'ve completed freebuff.com login in the open window\n(it redirects to /chat after login).\n\nNo further action is needed after login, the gateway will capture it automatically.',
            buttons: ['Keep waiting', 'Close window'],
          }).then(({ response }) => {
            if (response === 1 && loginWindow) { loginWindow.close(); loginWindow = null; }
          });
        }
      }, 1200);
    }
  });
  loginWindow.on('closed', () => { loginWindow = null; });
  loginWindow.loadURL('https://freebuff.com/');
}

app.on('ready', async () => {
  writeLog('[app] starting');
  migrateLegacyData();
  startGateway();
  const ok = await waitForGateway();
  if (!ok) {
    ready = false;
    const port = configuredPort();
    writeLog(`[ERROR] Gateway not ready (port ${port})`);
    dialog.showErrorBox(
      'Freebuff2API failed to start',
      `The gateway didn't respond within 15 seconds (expected http://127.0.0.1:${port}).\n\n` +
      `Common causes and fixes:\n` +
      `1. Port ${port} is in use by another program -> edit %APPDATA%\\freebuff2api\\config.json and change listen_addr\n` +
      `2. First launch is slow -> wait and retry from the tray "Open Console"\n` +
      `3. Antivirus/firewall is blocking it -> allow freebuff2api.exe through\n\n` +
      `Log: ${logPath()}`
    );
  } else {
    ready = true;
    writeLog('[app] Gateway ready');
  }
  createWindow();
  createTray();

  // Main window IPC: panel click "one-click login" -> open the login window
  ipcMain.handle('open-login', () => openLoginWindow());
  ipcMain.handle('capture-cookie', async () => await captureCookies());

  // Check for updates (electron-updater, packaged builds only)
  setupUpdater();
});

// ---------- Auto-update ----------
let updater = null;
function setupUpdater() {
  if (!app.isPackaged) return; // skip in dev mode
  try {
    const { autoUpdater } = require('electron-updater');
    updater = autoUpdater;
    autoUpdater.autoDownload = true;          // auto-download when a new version is available
    autoUpdater.autoInstallOnAppQuit = true;  // auto-install on quit
    autoUpdater.setFeedURL({
      provider: 'generic',
      url: 'https://github.com/lza6/Freebuff-2API/releases/latest/download/',
    });
    autoUpdater.on('update-available', () => {
      tray.setToolTip('Freebuff2API — new version available, downloading…');
      tray.displayBalloon({ title: 'Freebuff2API update available', content: 'Downloading in the background; it will auto-install on quit' });
    });
    autoUpdater.on('update-downloaded', () => {
      tray.setToolTip('Freebuff2API — update ready, will install on quit');
      dialog.showMessageBox({
        type: 'info',
        title: 'Update downloaded',
        message: 'The new version has finished downloading and will auto-install when you quit.',
        buttons: ['Restart and install now', 'Later'],
      }).then(({ response }) => {
        if (response === 0) { app.isQuitting = true; if (gateway) gateway.kill(); updater.quitAndInstall(); }
      });
    });
    autoUpdater.on('update-not-available', () => {
      tray.displayBalloon({ title: 'Freebuff2API', content: 'Already on the latest version ✅' });
    });
    autoUpdater.on('error', (e) => { writeLog(`[updater] ${e.message}`); console.error('[updater]', e.message); });
    autoUpdater.checkForUpdates().catch(() => {});
  } catch (e) {
    writeLog(`[updater] initialization failed ${e.message}`);
    console.error('[updater] initialization failed', e.message);
  }
}

function checkForUpdates() {
  if (!updater) { setupUpdater(); }
  if (updater) {
    updater.checkForUpdates().catch((e) => {
      dialog.showErrorBox('Update check failed', `Couldn't reach the update source (possibly a network issue).\n\n${e.message}`);
    });
  } else {
    dialog.showMessageBox({ type: 'info', title: 'Check for updates', message: 'Updates aren\'t checked in dev mode.' });
  }
}

app.on('window-all-closed', (e) => {
  // Stay resident in the tray: hide only, don't quit
  if (process.platform !== 'darwin') {
    // keep the tray process alive
  }
});

app.on('before-quit', () => {
  app.isQuitting = true;
  if (gateway) gateway.kill();
  if (logStream) { try { logStream.end(); } catch (_) { /* ignore close errors */ } }
});
