// Service worker: talks to the Truthness server on behalf of pages, so the
// server needs no CORS headers and web pages cannot call it themselves.

const DEFAULTS = { server: 'http://localhost:8787', provider: '' };

async function settings() {
  const s = await chrome.storage.sync.get(DEFAULTS);
  return { server: String(s.server || DEFAULTS.server).replace(/\/+$/, ''), provider: s.provider || '' };
}

async function call(server, path, init) {
  let res;
  try {
    res = await fetch(server + path, init);
  } catch {
    throw new Error(`Could not reach the Truthness server at ${server}. Is it running? You can change the address in the extension's options.`);
  }
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `The server answered ${res.status}`);
  return body;
}

async function handle(msg) {
  const { server, provider } = await settings();
  switch (msg.type) {
    case 'start': {
      const payload = { ...msg.payload };
      if (provider) payload.provider = provider;
      const { id } = await call(server, '/api/check', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      return { id, server };
    }
    case 'poll':
      return call(server, `/api/check/${encodeURIComponent(msg.id)}`);
    case 'providers':
      return call((msg.server || server).replace(/\/+$/, ''), '/api/providers');
    default:
      throw new Error('Unknown message');
  }
}

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  handle(msg).then(sendResponse, (e) => sendResponse({ error: e.message }));
  return true; // answer asynchronously
});

// Toolbar button and right-click menu: inject the panel into the current tab
// (only when asked, via activeTab) and tell it what to check.
async function openPanel(tab, message) {
  if (!tab?.id) return;
  try {
    await chrome.scripting.executeScript({ target: { tabId: tab.id }, files: ['render.js', 'content.js'] });
    await chrome.tabs.sendMessage(tab.id, message);
  } catch (e) {
    // Browser pages (chrome://, the extension store, PDFs) cannot be scripted.
    console.warn('Truthness cannot run on this page:', e.message);
    await chrome.action.setBadgeText({ tabId: tab.id, text: '✕' });
    await chrome.action.setTitle({ tabId: tab.id, title: 'Truthness cannot read this kind of page' });
  }
}

chrome.action.onClicked.addListener((tab) => openPanel(tab, { type: 'tn-open', mode: 'page' }));

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: 'tn-selection', title: 'Fact-check “%s” with Truthness', contexts: ['selection'] });
  chrome.contextMenus.create({ id: 'tn-page', title: 'Fact-check this page with Truthness', contexts: ['page', 'video'] });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === 'tn-selection' && info.selectionText) {
    openPanel(tab, { type: 'tn-open', mode: 'text', text: info.selectionText });
  } else if (info.menuItemId === 'tn-page') {
    openPanel(tab, { type: 'tn-open', mode: 'page' });
  }
});
