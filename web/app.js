(function () {
  'use strict';
  const { render, createProgress, renderError } = window.TruthnessRender;
  const $ = (id) => document.getElementById(id);
  const form = $('form');
  const output = $('output');
  const providerSel = $('provider');
  const formError = $('form-error');
  let providers = [];
  let mode = 'url';
  let run = 0; // identifies the current check so stale polls are ignored

  async function api(path, options) {
    const res = await fetch(path, options);
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error || `The server answered ${res.status}`);
    return body;
  }

  function setMode(next) {
    mode = next;
    for (const m of ['url', 'text']) {
      $(`tab-${m}`).setAttribute('aria-selected', String(m === mode));
      $(`pane-${m}`).hidden = m !== mode;
    }
    formError.hidden = true;
  }
  $('tab-url').addEventListener('click', () => setMode('url'));
  $('tab-text').addEventListener('click', () => setMode('text'));

  function providerNote() {
    const p = providers.find((x) => x.id === providerSel.value);
    $('provider-note').textContent = !p ? '' : p.grounded ? 'Searches the web for evidence.' : 'No web search: answers from memory only.';
  }
  providerSel.addEventListener('change', () => {
    localStorage.setItem('truthness.provider', providerSel.value);
    providerNote();
  });

  async function loadProviders() {
    try {
      const data = await api('/api/providers');
      providers = data.providers;
      if (!providers.length) {
        providerSel.replaceChildren(new Option('No model configured', ''));
        $('provider-note').textContent = 'Install the claude CLI or add a provider to truthness.toml, then restart the server.';
        return;
      }
      providerSel.replaceChildren(
        ...providers.map((p) => new Option(p.model && p.model !== 'CLI default' ? `${p.label} · ${p.model}` : p.label, p.id))
      );
      const saved = localStorage.getItem('truthness.provider');
      providerSel.value = providers.some((p) => p.id === saved) ? saved : data.default || providers[0].id;
      providerNote();
    } catch (e) {
      $('provider-note').textContent = `Could not reach the server: ${e.message}`;
    }
  }

  function setBusy(busy) {
    for (const b of form.querySelectorAll('button.primary')) b.disabled = busy;
  }

  async function check(payload) {
    const mine = ++run;
    const started = Date.now();
    const secs = () => Math.round((Date.now() - started) / 1000);
    const retry = () => check(payload);
    setBusy(true);
    const progress = createProgress();
    output.replaceChildren(progress.el);
    output.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    try {
      const { id } = await api('/api/check', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      for (;;) {
        await new Promise((r) => setTimeout(r, 1200));
        if (mine !== run) return;
        const job = await api(`/api/check/${id}`);
        if (mine !== run) return;
        if (job.status === 'done') {
          output.replaceChildren(render(job.result, { onRecheck: () => check({ ...payload, force: true }) }));
          break;
        }
        if (job.status === 'error') throw new Error(job.error || 'Unknown error');
        progress.update(job.message, secs(), job.status);
      }
    } catch (e) {
      if (mine === run) output.replaceChildren(renderError(e.message, retry));
    } finally {
      if (mine === run) setBusy(false);
    }
  }

  form.addEventListener('submit', (ev) => {
    ev.preventDefault();
    formError.hidden = true;
    const fail = (msg, el) => {
      formError.textContent = msg;
      formError.hidden = false;
      el.focus();
    };
    const payload = { provider: providerSel.value || undefined };
    if (mode === 'url') {
      let url = $('url').value.trim();
      if (!url) return fail('Paste a link to check.', $('url'));
      if (!/^https?:\/\//i.test(url)) url = `https://${url}`;
      try {
        new URL(url);
      } catch {
        return fail('That does not look like a link.', $('url'));
      }
      payload.url = url;
      history.replaceState(null, '', `?url=${encodeURIComponent(url)}`);
    } else {
      const text = $('text').value.trim();
      if (text.length < 20) return fail('Paste a little more text so there is something to check.', $('text'));
      payload.text = text;
      history.replaceState(null, '', location.pathname);
    }
    check(payload);
  });

  loadProviders().then(() => {
    // Shareable links: /?url=https://…
    const url = new URLSearchParams(location.search).get('url');
    if (url) {
      $('url').value = url;
      form.requestSubmit();
    }
  });
})();
