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
        ? '管理キーを確認して、もう一度ログインしてください。'
        : `情報を取得できませんでした（HTTP ${response.status}）。しばらくしてから更新してください。`);
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
      message('クリップボードにコピーしました。');
    } catch {
      message('コピーできませんでした。表示された値を選択してコピーしてください。', true);
    }
  }

  async function health() {
    try {
      const result = await request('/health/liveliness');
      if (result.status !== 'alive') throw new Error('Invalid health response');
      byId('health').textContent = 'オンライン';
      byId('sidebar-status').textContent = 'サービス稼働中';
      byId('health-dot').classList.add('healthy');
    } catch {
      byId('health').textContent = '接続できません';
      byId('sidebar-status').textContent = '接続を確認してください';
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
    byId('search-count').textContent = `${matches.length} / ${models.length}件`;
    byId('model-list').replaceChildren(...matches.map((model) => {
      const button = document.createElement('button');
      button.className = 'copy-button';
      button.textContent = '名前をコピー';
      button.setAttribute('aria-label', `${model}のモデル名をコピー`);
      button.addEventListener('click', () => { void copy(model); });
      return row(model, '登録済み', button);
    }));
    byId('models-empty').hidden = matches.length > 0;
    byId('models-empty').textContent = models.length === 0 ? '登録されたモデルがありません。' : '一致するモデルがありません。';
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
    byId('models-empty').textContent = 'ログインすると登録モデルを表示します。';
    byId('endpoints-empty').hidden = false;
    byId('updated').textContent = '未接続';
  }

  async function connect(candidate) {
    if (pending) return;
    pending = true;
    const revision = connectionRevision;
    byId('connect').disabled = true;
    byId('refresh').disabled = true;
    message('情報を取得しています…');
    try {
      const [status] = await Promise.all([
        request('/ui/api/status', { Authorization: `Bearer ${candidate}` }), health(),
      ]);
      if (revision !== connectionRevision) return;
      if (status.product !== 'LLM Unify' || !Array.isArray(status.models) || status.models.some((model) => typeof model !== 'string') || !Array.isArray(status.endpoints)) {
        throw new Error('情報を読み取れませんでした。もう一度更新してください。');
      }
      token = candidate;
      models = status.models;
      byId('master-key').value = '';
      byId('connection').hidden = true;
      byId('disconnect').hidden = false;
      byId('refresh').hidden = false;
      byId('search-row').hidden = false;
      byId('version').textContent = `LLM Unify v${status.version}`;
      byId('model-count').textContent = models.length.toLocaleString('ja-JP');
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
      byId('updated').textContent = `最終更新 ${new Date().toLocaleString('ja-JP')}`;
      message('最新の設定を取得しました。');
    } catch (error) {
      if (revision !== connectionRevision) return;
      if (!token || error.status === 401 || error.status === 403) reset();
      const text = error.name === 'TimeoutError' || error.name === 'AbortError'
        ? '接続がタイムアウトしました。しばらくしてから更新してください。'
        : error instanceof TypeError ? '接続できませんでした。サービスの稼働状況を確認してください。' : error.message;
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
  byId('disconnect').addEventListener('click', () => { reset(); message('ログアウトしました。'); });
  window.addEventListener('pagehide', reset);
  void health().then(() => message('ログインしてワークスペースを開きましょう。'));
})();
