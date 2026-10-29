'use strict';
(() => {
  const REQUEST_TIMEOUT_MS = __REQUEST_TIMEOUT_MS__;
  const byId = (id) => document.getElementById(id);
  const baseUrl = new URL('/v1', window.location.origin).href;
  let token = '';
  let models = [];
  let pending = false;
  let connectionRevision = 0;
  byId('base-url').textContent = baseUrl;

  async function request(path, headers = {}) {
    const response = await fetch(path, {
      headers, cache: 'no-store', credentials: 'omit', redirect: 'error',
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    if (!response.ok) {
      const error = new Error(response.status === 401 || response.status === 403
        ? 'Check your master key and sign in again.'
        : `Could not retrieve information (HTTP ${response.status}). Try refreshing in a moment.`);
      error.status = response.status;
      throw error;
    }
    return response.json();
  }

  function message(text, failed = false) {
    byId('message').textContent = text;
    byId('message').classList.toggle('error', failed);
  }

  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text);
      message('Copied to clipboard.');
    } catch {
      message('Could not copy. Select and copy the displayed value manually.', true);
    }
  }

  async function health() {
    try {
      const result = await request('/health/liveliness');
      if (result.status !== 'alive') throw new Error('Invalid health response');
      byId('health').textContent = 'Online';
      byId('sidebar-status').textContent = 'Service running';
      byId('health-dot').classList.add('healthy');
    } catch {
      byId('health').textContent = 'Unavailable';
      byId('sidebar-status').textContent = 'Check your connection';
      byId('health-dot').classList.remove('healthy');
    }
  }

  function row(label, detail, action) {
    const element = document.createElement('div');
    element.className = 'data-row';
    const content = document.createElement('div');
    const title = document.createElement('strong');
    title.textContent = label;
    const description = document.createElement('p');
    description.textContent = detail;
    content.append(title, description);
    element.append(content, action);
    return element;
  }

  function renderModels() {
    const query = byId('model-search').value.trim().toLocaleLowerCase();
    const matches = models.filter((model) => model.toLocaleLowerCase().includes(query));
    byId('search-count').textContent = `${matches.length} / ${models.length} models`;
    byId('model-list').replaceChildren(...matches.map((model) => {
      const button = document.createElement('button');
      button.className = 'copy-button';
      button.textContent = 'Copy name';
      button.setAttribute('aria-label', `Copy model name: ${model}`);
      button.addEventListener('click', () => { void copy(model); });
      return row(model, 'Configured', button);
    }));
    byId('models-empty').hidden = matches.length > 0;
    byId('models-empty').textContent = models.length === 0 ? 'No models configured.' : 'No matching models.';
  }

  function reset() {
    connectionRevision += 1;
    token = '';
    models = [];
    byId('master-key').value = '';
    byId('model-search').value = '';
    byId('connection').hidden = false;
    byId('disconnect').hidden = true;
    byId('refresh').hidden = true;
    byId('search-row').hidden = true;
    byId('model-count').textContent = '—';
    byId('version').textContent = 'LLM Unify';
    byId('model-list').replaceChildren();
    byId('endpoint-list').replaceChildren();
    byId('model-list').hidden = true;
    byId('endpoint-list').hidden = true;
    byId('models-empty').hidden = false;
    byId('models-empty').textContent = 'Sign in to view configured models.';
    byId('endpoints-empty').hidden = false;
    byId('updated').textContent = 'Disconnected';
  }

  async function connect(candidate) {
    if (pending) return;
    pending = true;
    const revision = connectionRevision;
    byId('connect').disabled = true;
    byId('refresh').disabled = true;
    message('Loading information…');
    try {
      const [status] = await Promise.all([
        request('/ui/api/status', { Authorization: `Bearer ${candidate}` }), health(),
      ]);
      if (revision !== connectionRevision) return;
      if (status.product !== 'LLM Unify' || !Array.isArray(status.models) || status.models.some((model) => typeof model !== 'string') || !Array.isArray(status.endpoints)) {
        throw new Error('Could not read the information. Please refresh again.');
      }
      token = candidate;
      models = status.models;
      byId('master-key').value = '';
      byId('connection').hidden = true;
      byId('disconnect').hidden = false;
      byId('refresh').hidden = false;
      byId('search-row').hidden = false;
      byId('version').textContent = `LLM Unify v${status.version}`;
      byId('model-count').textContent = models.length.toLocaleString('en-US');
      renderModels();
      byId('endpoint-list').replaceChildren(...status.endpoints.map((endpoint) => {
        const method = document.createElement('span');
        method.className = 'tag';
        method.textContent = endpoint.method;
        return row(endpoint.path, endpoint.description, method);
      }));
      byId('endpoints-empty').hidden = true;
      byId('model-list').hidden = false;
      byId('endpoint-list').hidden = false;
      byId('updated').textContent = `Last updated: ${new Date().toLocaleString('en-US')}`;
      message('Configuration is up to date.');
    } catch (error) {
      if (revision !== connectionRevision) return;
      if (!token || error.status === 401 || error.status === 403) reset();
      const text = error.name === 'TimeoutError' || error.name === 'AbortError'
        ? 'Connection timed out. Try refreshing in a moment.'
        : error instanceof TypeError ? 'Could not connect. Check whether the service is running.' : error.message;
      message(text, true);
    } finally {
      pending = false;
      byId('connect').disabled = false;
      byId('refresh').disabled = false;
    }
  }

  byId('auth-form').addEventListener('submit', (event) => {
    event.preventDefault();
    const candidate = byId('master-key').value.trim();
    if (candidate) void connect(candidate);
  });
  byId('model-search').addEventListener('input', renderModels);
  byId('copy-base').addEventListener('click', () => { void copy(baseUrl); });
  byId('refresh').addEventListener('click', () => { if (token) void connect(token); });
  byId('disconnect').addEventListener('click', () => { reset(); message('Signed out.'); });
  window.addEventListener('pagehide', reset);
  void health().then(() => message('Sign in to open your workspace.'));
})();
