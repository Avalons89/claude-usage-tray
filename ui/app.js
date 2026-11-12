const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const app = document.getElementById('app');
const strip = document.getElementById('strip');
const card = document.getElementById('card');

const DRAG_THRESHOLD_PX = 4;
const MODELS_SHOWN = 3;
const ALWAYS_IN_STRIP = ['session', 'weekly_all'];
const LIMIT_HELP = {
  session: 'Share of your 5-hour session limit used, across claude.ai and every device',
  weekly_all: 'Share of your weekly limit used, all models',
};

const tokenFormat = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 });
const dollarFormat = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' });
const dayAndTime = new Intl.DateTimeFormat(undefined, { weekday: 'short', hour: '2-digit', minute: '2-digit' });

const tok = (n) => tokenFormat.format(n);
const usd = (n) => dollarFormat.format(n);
const totalTokens = (t) => t.input + t.output + t.cache_read + t.cache_write;

function node(tag, text, className, title) {
  const el = document.createElement(tag);
  if (text != null) el.textContent = text;
  if (className) el.className = className;
  if (title) el.title = title;
  return el;
}

function row(label, value, title, className = 'row') {
  const el = node('div', null, className, title);
  el.append(node('span', label, 'dim'), node('span', value));
  return el;
}

function limitHelp(limit) {
  return LIMIT_HELP[limit.kind] ?? `Share of your weekly ${limit.label.replace(/ week$/, '')} limit used`;
}

function resetText(limit) {
  if (!limit.resets_at) return '';
  const at = new Date(limit.resets_at);
  if (limit.kind !== 'session') return `resets ${dayAndTime.format(at)}`;
  const minutes = Math.max(0, Math.round((at - Date.now()) / 60000));
  return `resets in ${Math.floor(minutes / 60)}h${String(minutes % 60).padStart(2, '0')}m`;
}

function renderStrip(snap) {
  const items = [];
  const limits = snap.account?.limits ?? [];
  for (const limit of limits.filter((l) => ALWAYS_IN_STRIP.includes(l.kind) || l.level === 'high')) {
    const item = node('span', null, null, limitHelp(limit));
    item.append(node('b', `${limit.label} ${Math.round(limit.percent)}%`, limit.level));
    if (limit.kind === 'session') item.append(' ', node('span', resetText(limit), 'dim'));
    items.push(item);
  }
  if (snap.account_error) {
    items.push(node('span', snap.account ? 'Account numbers stale' : 'Account unavailable', 'warn', snap.account_error));
  }

  const usage = snap.usage;
  const total = totalTokens(usage.today);
  items.push(total === 0
    ? node('span', 'Today: no Claude Code use yet', 'dim', 'Counted from the Claude Code logs on this PC')
    : node('span', `Today ${tok(total)} tok · ${usd(usage.cost_today)}`, null,
      'Tokens Claude Code used on this PC since midnight, and what they would cost at API prices'));
  if (usage.burn_per_minute > 0) {
    items.push(node('span', `Burn ${tok(usage.burn_per_minute)} tok/min`, null,
      'Tokens per minute over the last 15 minutes, cache reads included'));
  }
  strip.replaceChildren(...items);
}

function limitRow(limit) {
  const el = node('div', null, 'limit', limitHelp(limit));
  const name = node('span', `${limit.label} `);
  name.append(node('b', `${Math.round(limit.percent)}%`, limit.level));
  const bar = node('div', null, `bar ${limit.level}`);
  bar.setAttribute('role', 'progressbar');
  bar.setAttribute('aria-label', limit.label);
  bar.setAttribute('aria-valuemin', '0');
  bar.setAttribute('aria-valuemax', '100');
  bar.setAttribute('aria-valuenow', String(Math.round(limit.percent)));
  const fill = node('i');
  fill.style.width = `${Math.min(100, limit.percent)}%`;
  bar.append(fill);
  el.append(name, node('span', resetText(limit), 'dim'), bar);
  return el;
}

function renderCard(snap) {
  const usage = snap.usage;
  const today = usage.today;
  const total = totalTokens(today);
  const parts = (snap.account?.limits ?? []).map(limitRow);

  const tokens = node('div', null, 'group');
  tokens.append(row('Today', total === 0 ? 'no use yet' : `${tok(total)} tokens`, 'Tokens Claude Code used on this PC since midnight'));
  if (total > 0) {
    tokens.append(
      row('input · output', `${tok(today.input)} · ${tok(today.output)}`, null, 'row detail'),
      row('cache read · write', `${tok(today.cache_read)} · ${tok(today.cache_write)}`,
        'Cache reads are cheap re-reads of earlier context; writes store new context for reuse', 'row detail'),
    );
  }

  const cost = node('div', null, 'group');
  cost.append(
    node('div', 'At API prices', 'dim', 'What this PC\'s Claude Code use would cost on the pay-as-you-go API'),
    row('today · week · month', `${usd(usage.cost_today)} · ${usd(usage.cost_week)} · ${usd(usage.cost_month)}`),
  );

  const activity = node('div', null, 'group');
  activity.append(row('Burn', usage.burn_per_minute > 0 ? `${tok(usage.burn_per_minute)} tok/min` : 'idle',
    'Tokens per minute over the last 15 minutes, cache reads included'));
  if (usage.models_today.length > 0) {
    const models = usage.models_today.slice(0, MODELS_SHOWN).map((m) => `${m.name} ${Math.round(m.share * 100)}%`).join(' · ');
    activity.append(row('Models today', models, 'Share of today\'s API-price cost'));
  }

  const footer = node('div', null, 'group');
  if (snap.account_error) footer.append(node('div', snap.account_error, 'warn detail'));
  const checked = snap.account ? ` · limits ${snap.account.checked_at}` : '';
  footer.append(row(snap.account?.plan ?? '', `logs ${snap.updated_at}${checked}`,
    'Logs are read every minute, plan limits every 5 minutes', 'row detail'));

  card.replaceChildren(...parts, tokens, cost, activity, footer);
}

function render(snap) {
  renderStrip(snap);
  renderCard(snap);
}

function setOpen(open) {
  card.hidden = !open;
  strip.setAttribute('aria-expanded', String(open));
}

// A press that moves hands the window to the OS for dragging; a press that doesn't is a click.
let press = null;
let dragged = false;
strip.addEventListener('mousedown', (e) => {
  dragged = false;
  press = e.button === 0 ? { x: e.screenX, y: e.screenY } : null;
});
strip.addEventListener('mousemove', (e) => {
  if (!press || Math.hypot(e.screenX - press.x, e.screenY - press.y) < DRAG_THRESHOLD_PX) return;
  press = null;
  dragged = true;
  invoke('start_drag');
});
strip.addEventListener('mouseup', () => { press = null; });
strip.addEventListener('click', (e) => {
  // detail is 0 for Enter and Space, which never follow a drag.
  if (e.detail > 0 && dragged) return;
  setOpen(card.hidden);
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') setOpen(false);
});

new ResizeObserver(() => {
  const box = app.getBoundingClientRect();
  invoke('fit', { width: Math.ceil(box.width), height: Math.ceil(box.height) });
}).observe(app);

listen('snapshot', (event) => render(event.payload));
invoke('snapshot').then((snap) => snap && render(snap));
