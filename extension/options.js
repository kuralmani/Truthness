const DEFAULTS = { server: 'http://localhost:8787', provider: '' };
const $ = (id) => document.getElementById(id);

function status(text, bad) {
  $('status').textContent = text;
  $('status').className = bad ? 'bad' : '';
}

function serverValue() {
  return ($('server').value.trim() || DEFAULTS.server).replace(/\/+$/, '');
}

// Servers other than localhost need the user's permission before the
// extension may contact them. Must be called from a click handler.
async function ensureAccess(server) {
  const url = new URL(server);
  if (!/^https?:$/.test(url.protocol)) throw new Error('The address must start with http:// or https://');
  if (['localhost', '127.0.0.1'].includes(url.hostname)) return;
  const granted = await chrome.permissions.request({ origins: [`${url.protocol}//${url.hostname}/*`] });
  if (!granted) throw new Error('Permission to contact that server was not granted.');
}

async function loadProviders(server, selected) {
  const res = await chrome.runtime.sendMessage({ type: 'providers', server });
  if (!res || res.error) throw new Error(res ? res.error : 'No answer from the extension.');
  const sel = $('provider');
  sel.replaceChildren(new Option('Server default', ''));
  for (const p of res.providers) {
    sel.append(new Option(`${p.label}${p.model && p.model !== 'CLI default' ? ` · ${p.model}` : ''}${p.grounded ? '' : ' (no web search)'}`, p.id));
  }
  sel.value = res.providers.some((p) => p.id === selected) ? selected : '';
  return res.providers.length;
}

async function test() {
  try {
    const server = serverValue();
    await ensureAccess(server);
    const n = await loadProviders(server, $('provider').value);
    status(n ? `Connected. ${n} model${n === 1 ? '' : 's'} available.` : 'Connected, but the server has no model configured.', !n);
  } catch (e) {
    status(e.message, true);
  }
}

$('test').addEventListener('click', test);
$('save').addEventListener('click', async () => {
  try {
    const server = serverValue();
    await ensureAccess(server);
    await chrome.storage.sync.set({ server, provider: $('provider').value });
    status('Saved.');
  } catch (e) {
    status(e.message, true);
  }
});

(async () => {
  const s = await chrome.storage.sync.get(DEFAULTS);
  $('server').value = s.server;
  try {
    await loadProviders(s.server.replace(/\/+$/, ''), s.provider);
  } catch (e) {
    status(e.message, true);
  }
})();
