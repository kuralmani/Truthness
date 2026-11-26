// In-page UI: a "Check" button on YouTube videos and a slide-in panel that
// shows the verdict. Lives in a shadow root so it neither inherits the page's
// styles nor leaks its own. Injected on demand on other sites.
(function () {
  'use strict';
  if (window.__truthnessLoaded) return;
  window.__truthnessLoaded = true;

  const { render, createProgress, renderError, h, icon } = window.TruthnessRender;
  const MAX_PAGE_CHARS = 60000;
  const isYouTube = /(^|\.)youtube\.com$/.test(location.hostname);
  const isVideoPage = () => isYouTube && /^\/(watch|shorts\/|live\/)/.test(location.pathname);

  let ui = null; // built lazily
  let run = 0; // identifies the current check; stale polls are dropped
  let checkedUrl = null;

  function send(message) {
    return new Promise((resolve, reject) => {
      try {
        chrome.runtime.sendMessage(message, (res) => {
          if (chrome.runtime.lastError) return reject(new Error(chrome.runtime.lastError.message));
          if (!res) return reject(new Error('No answer from the extension.'));
          if (res.error) return reject(new Error(res.error));
          resolve(res);
        });
      } catch {
        reject(new Error('The extension was updated or reloaded. Refresh this page and try again.'));
      }
    });
  }

  async function loadSheet(file) {
    const css = await (await fetch(chrome.runtime.getURL(file))).text();
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(css);
    return sheet;
  }

  async function build() {
    if (ui) return ui;
    const hostEl = document.createElement('div');
    hostEl.id = 'truthness-root';
    const shadow = hostEl.attachShadow({ mode: 'open' });
    shadow.adoptedStyleSheets = await Promise.all([loadSheet('verdict.css'), loadSheet('panel.css')]);

    const fab = h('button', { class: 'tn-fab', type: 'button', hidden: true, onclick: () => open({ mode: 'page' }) }, h('span', { class: 'tn-fab-icon' }, icon('search')), 'Check the facts');
    const body = h('div', { class: 'tn-panel-body' });
    const fullLink = h('a', { target: '_blank', rel: 'noopener noreferrer', hidden: true }, 'Full page');
    const panel = h(
      'aside',
      { class: 'tn tn-panel', hidden: true, 'aria-label': 'Truthness fact check' },
      h(
        'div',
        { class: 'tn-panel-head' },
        h('span', { class: 'tn-panel-title' }, h('span', { class: 'tn-fab-icon' }, icon('search')), 'Truthness'),
        h('div', { class: 'tn-panel-actions' }, fullLink, h('button', { class: 'tn-close', type: 'button', 'aria-label': 'Close', onclick: close }, '×'))
      ),
      body
    );
    // Keep YouTube's keyboard shortcuts from firing while the panel has focus.
    panel.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') close();
      e.stopPropagation();
    });
    shadow.append(fab, panel);
    document.documentElement.append(hostEl);
    ui = { fab, panel, body, fullLink };
    return ui;
  }

  function close() {
    if (!ui) return;
    ui.panel.hidden = true;
    ui.fab.hidden = !isVideoPage();
  }

  function pageText() {
    const candidates = [...document.querySelectorAll('article, main, [role="main"]')];
    let best = '';
    for (const el of candidates) {
      const t = el.innerText || '';
      if (t.length > best.length) best = t;
    }
    if (best.length < 600) best = document.body ? document.body.innerText : '';
    return best.replace(/\n{3,}/g, '\n\n').trim().slice(0, MAX_PAGE_CHARS);
  }

  function payloadFor(request) {
    if (request.mode === 'text') return { text: request.text, title: document.title };
    // The server reads YouTube captions itself; other pages are sent as the
    // reader sees them, which also covers pages behind a login.
    if (isVideoPage()) return { url: location.href };
    return { url: location.href, title: document.title, text: pageText() };
  }

  async function open(request) {
    const u = await build();
    u.fab.hidden = true;
    u.panel.hidden = false;
    u.panel.querySelector('.tn-close').focus();
    check(payloadFor(request));
  }

  async function check(payload) {
    const mine = ++run;
    const started = Date.now();
    const secs = () => Math.round((Date.now() - started) / 1000);
    const show = (node) => mine === run && ui.body.replaceChildren(node);
    checkedUrl = location.href;
    ui.fullLink.hidden = true;
    const progress = createProgress();
    show(progress.el);
    try {
      const { id, server } = await send({ type: 'start', payload });
      if (payload.url && !payload.text) {
        ui.fullLink.href = `${server}/?url=${encodeURIComponent(payload.url)}`;
        ui.fullLink.hidden = false;
      }
      for (;;) {
        await new Promise((r) => setTimeout(r, 1500));
        if (mine !== run) return;
        const job = await send({ type: 'poll', id });
        if (job.status === 'done') {
          show(render(job.result, { showSource: false, onRecheck: () => check({ ...payload, force: true }) }));
          return;
        }
        if (job.status === 'error') throw new Error(job.error || 'Unknown error');
        if (mine === run) progress.update(job.message, secs(), job.status);
      }
    } catch (e) {
      show(renderError(e.message, () => check(payload)));
    }
  }

  chrome.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === 'tn-open') open(msg);
  });

  if (isYouTube) {
    const sync = async () => {
      const u = await build();
      // YouTube swaps videos without reloading: a verdict for the previous
      // video must not stay on screen next to the new one.
      if (!u.panel.hidden && checkedUrl && checkedUrl !== location.href) {
        run++;
        u.fullLink.hidden = true;
        u.body.replaceChildren(
          isVideoPage()
            ? h('div', { class: 'tn-idle' }, h('p', {}, 'You moved to a different video.'), h('button', { class: 'tn-btn', type: 'button', onclick: () => open({ mode: 'page' }) }, 'Check this video'))
            : h('p', { class: 'tn-muted' }, 'Open a video to check it.')
        );
        checkedUrl = location.href;
      }
      if (u.panel.hidden) u.fab.hidden = !isVideoPage();
    };
    document.addEventListener('yt-navigate-finish', sync);
    window.addEventListener('popstate', sync);
    // Shorts change the URL while scrolling without firing a navigation event.
    let last = location.href;
    setInterval(() => {
      if (location.href !== last) {
        last = location.href;
        sync();
      }
    }, 1000);
    sync();
  }
})();
