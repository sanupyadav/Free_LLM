// Desktop preload: exposes minimal IPC capabilities to the renderer (control panel)
// Security constraint: only expose whitelisted methods, never the ipcRenderer object itself
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('freebuffDesktop', {
  /** Opens the freebuff.com login window; on success, automatically captures and stores the cookie */
  openLogin: () => ipcRenderer.invoke('open-login'),
  /** Manually triggers a cookie capture (when already logged in) */
  captureCookie: () => ipcRenderer.invoke('capture-cookie'),
  isDesktop: true,
});
