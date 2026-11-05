import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import './style.css';

type Color = 'blue' | 'red' | 'yellow';
type Row = {
  key: string; name: string; harness: string; state: string; reason: string | null;
  detail: string | null; since: number; color: string; section: string;
};
type Stats = { total_ms: number; by_color: Record<string, number>; sessions: { key: string; name: string; ms: number }[] };
type View = { rows: Row[]; light: string; count: number; quiet_until: number; stats: Stats | null };
type Config = {
  stalled_after_secs: number; dnd: string; away_after_secs: number; phone_after_secs: number;
  phone_command: string[]; hotkey: string;
  escalation: { start_interval_secs: number; min_interval_secs: number; start_volume: number; ramp_secs: number; yellow_max_rings: number; yellow_delay_secs: number };
  sounds: Record<Color, string>;
};

const root = document.querySelector<HTMLElement>('#app')!;
let view: View | null = null;
let inSettings = false;
let headlessOpen = false;
const esc = (text: string) => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&#39;');
const duration = (ms: number) => {
  const minutes = Math.floor(Math.max(0, ms) / 60_000);
  const hours = Math.floor(minutes / 60);
  return hours ? `${hours}h${String(minutes % 60).padStart(2, '0')}m` : `${minutes}m`;
};
const dot = (color: string) => `<span class="dot ${esc(color)}" aria-label="${esc(color)}"></span>`;
const updateDot = (element: Element, color: string) => {
  element.className = `dot ${color}`;
  element.setAttribute('aria-label', color);
};

function render(): void {
  if (inSettings || !view) return;
  if (!root.querySelector('.list')) {
    root.innerHTML = `<header><div class="headline">${dot('grey')}<div><h1>Pay Attention</h1><span id="count"></span></div></div>
      <div class="header-actions"><button id="quiet" class="subtle"></button><button id="settings" class="icon-button" aria-label="Settings">⚙</button></div></header>
      <div class="list"><section data-section="needs"><h2>Needs you <span></span></h2><div></div></section>
      <section data-section="seen"><h2>Seen <span></span></h2><div></div></section>
      <section data-section="working"><h2>Working <span></span></h2><div></div></section>
      <details id="headless" data-section="headless" ${headlessOpen ? 'open' : ''}><summary>Headless <span></span></summary><div></div></details>
      <p class="empty">No sessions yet.</p></div><footer></footer>`;
    root.querySelector('#settings')!.addEventListener('click', () => void settings());
    root.querySelector('#quiet')!.addEventListener('click', () => void invoke('quiet', { enabled: view!.quiet_until <= Date.now() }));
    root.querySelector<HTMLDetailsElement>('#headless')!.addEventListener('toggle', event => { headlessOpen = (event.currentTarget as HTMLDetailsElement).open; });
  }
  const list = root.querySelector<HTMLElement>('.list')!;
  const scroll = list.scrollTop;
  const focused = document.activeElement instanceof HTMLButtonElement ? document.activeElement : null;
  const existing = new Map(Array.from(list.querySelectorAll<HTMLButtonElement>('[data-key]'), button => [button.dataset.key!, button]));
  const remaining = view.quiet_until - Date.now();
  updateDot(root.querySelector('.headline .dot')!, view.light);
  root.querySelector('#count')!.textContent = `${view.count} need${view.count === 1 ? 's' : ''} you`;
  root.querySelector('#quiet')!.textContent = remaining > 0 ? `Quiet ${duration(remaining + 59_999)}` : 'Quiet 1h';
  for (const section of ['needs', 'seen', 'working', 'headless']) {
    const group = list.querySelector<HTMLElement>(`[data-section="${section}"]`)!;
    const rows = view.rows.filter(row => row.section === section);
    group.hidden = rows.length === 0;
    group.querySelector('h2 span, summary span')!.textContent = String(rows.length);
    const container = group.querySelector('div')!;
    rows.forEach((row, index) => {
      let button = existing.get(row.key);
      if (!button) {
        button = document.createElement('button');
        button.dataset.key = row.key;
        button.innerHTML = `${dot('grey')}<span class="row-content"><span class="row-title"><strong></strong><span class="badge"></span></span><span class="reason"></span><span class="detail"></span></span><span class="wait"></span>`;
        button.addEventListener('click', () => void invoke('click', { key: button!.dataset.key }));
      }
      button.className = `row${section === 'seen' ? ' dim' : ''}`;
      updateDot(button.querySelector('.dot')!, row.color);
      for (const [selector, text] of [['strong', row.name || row.key], ['.badge', row.harness], ['.reason', row.reason ?? row.state], ['.detail', row.detail ?? ''], ['.wait', duration(Date.now() - row.since)]]) {
        const element = button.querySelector<HTMLElement>(selector)!;
        if (element.textContent !== text) element.textContent = text;
      }
      button.querySelector<HTMLElement>('.detail')!.hidden = !row.detail;
      if (container.children[index] !== button) container.insertBefore(button, container.children[index] ?? null);
      existing.delete(row.key);
    });
  }
  for (const button of existing.values()) button.remove();
  list.querySelector<HTMLElement>('.empty')!.hidden = view.rows.length > 0;
  const stats = view.stats;
  root.querySelector('footer')!.innerHTML = stats ? `<strong>Waited on you today: ${duration(stats.total_ms)}</strong><div class="breakdown">blue ${duration(stats.by_color.blue ?? 0)} · yellow ${duration(stats.by_color.yellow ?? 0)} · red ${duration(stats.by_color.red ?? 0)}</div>
    ${stats.sessions.map(session => `<div class="top-session"><span>${esc(session.name || session.key)}</span><span>${duration(session.ms)}</span></div>`).join('')}` : 'Stats unavailable';
  if (focused?.isConnected && document.activeElement !== focused) focused.focus({ preventScroll: true });
  list.scrollTop = scroll;
}

const field = (label: string, name: string, value: string | number, type = 'text', extra = '') =>
  `<label>${label}<input name="${name}" type="${type}" value="${esc(String(value))}" ${extra}></label>`;

async function settings(): Promise<void> {
  inSettings = true;
  root.innerHTML = '<header><h1>Settings</h1></header><p class="empty">Loading…</p>';
  try {
    const [{ config, version: initialVersion }, systems, autostart] = await Promise.all([
      invoke<{ config: Config; version: unknown }>('get_config'), invoke<string[]>('system_sounds'), invoke<boolean>('is_autostart_enabled'),
    ]);
    let version = initialVersion;
    const e = config.escalation;
    root.innerHTML = `<header><button id="back" class="subtle">← Back</button><h1>Settings</h1></header>
      <form id="settings-form"><div class="settings-body">
      <h2>General</h2>
      ${field('Quiet hours (local, HH:MM-HH:MM; empty to disable)', 'dnd', config.dnd)}
      ${field('Stalled after (seconds)', 'stalled_after_secs', config.stalled_after_secs, 'number', 'min="0" step="1" required')}
      ${field('Global shortcut', 'hotkey', config.hotkey, 'text', 'required')}
      <label class="checkbox"><input name="autostart" type="checkbox" ${autostart ? 'checked' : ''}> Launch at login</label>
      <h2>Escalation</h2>
      ${field('Starting interval (seconds)', 'start_interval_secs', e.start_interval_secs, 'number', 'min="0.01" step="any" required')}
      ${field('Minimum interval (seconds)', 'min_interval_secs', e.min_interval_secs, 'number', 'min="0.01" step="any" required')}
      ${field('Starting volume (0–1)', 'start_volume', e.start_volume, 'number', 'min="0" max="1" step="any" required')}
      ${field('Ramp duration (seconds)', 'ramp_secs', e.ramp_secs, 'number', 'min="0.01" step="any" required')}
      ${field('Yellow maximum rings', 'yellow_max_rings', e.yellow_max_rings, 'number', 'min="0" step="1" required')}
      ${field('Yellow delay (seconds)', 'yellow_delay_secs', e.yellow_delay_secs, 'number', 'min="0" step="any" required')}
      <h2>Phone</h2>
      ${field('Away after (seconds)', 'away_after_secs', config.away_after_secs, 'number', 'min="0" step="1" required')}
      ${field('Phone after (seconds)', 'phone_after_secs', config.phone_after_secs, 'number', 'min="0" step="1" required')}
      <label>Command as a JSON argv array<textarea name="phone_command" rows="3" spellcheck="false">${esc(JSON.stringify(config.phone_command))}</textarea></label>
      <p class="hint">Use {name}, {state} and {detail} in arguments. No shell is used.</p>
      <h2>Sounds</h2>
      ${(['blue', 'red', 'yellow'] as Color[]).map(color => {
        const value = config.sounds[color];
        const mode = value === 'synth' ? 'synth' : value.startsWith('system:') ? 'system' : 'file';
        return `<fieldset data-color="${color}"><legend>${dot(color)} ${color}</legend><div class="sound-controls">
          <select name="${color}_mode"><option value="synth" ${mode === 'synth' ? 'selected' : ''}>Synth</option><option value="system" ${mode === 'system' ? 'selected' : ''}>System sound</option><option value="file" ${mode === 'file' ? 'selected' : ''}>File</option></select>
          <button type="button" class="preview">Preview</button></div>
          <select name="${color}_system">${systems.map(name => `<option value="${esc(name)}" ${value === `system:${name}` ? 'selected' : ''}>${esc(name)}</option>`).join('')}</select>
          <div class="file-controls"><input name="${color}_file" value="${esc(mode === 'file' ? value : '')}" placeholder="Sound file path"><button type="button" class="browse">Choose…</button></div></fieldset>`;
      }).join('')}
      <p id="error" role="alert"></p></div><div class="settings-footer"><button type="submit" class="primary">Save</button></div></form>`;
    const form = root.querySelector<HTMLFormElement>('#settings-form')!;
    const input = (name: string) => form.elements.namedItem(name) as HTMLInputElement;
    const source = (color: Color) => input(`${color}_mode`).value === 'synth' ? 'synth' : input(`${color}_mode`).value === 'system' ? `system:${input(`${color}_system`).value}` : input(`${color}_file`).value;
    const updateSound = (color: Color) => {
      const group = form.querySelector<HTMLElement>(`[data-color="${color}"]`)!;
      group.querySelector<HTMLElement>(`[name="${color}_system"]`)!.hidden = input(`${color}_mode`).value !== 'system';
      group.querySelector<HTMLElement>('.file-controls')!.hidden = input(`${color}_mode`).value !== 'file';
    };
    for (const color of ['blue', 'red', 'yellow'] as Color[]) {
      const group = form.querySelector<HTMLElement>(`[data-color="${color}"]`)!;
      updateSound(color);
      input(`${color}_mode`).addEventListener('change', () => updateSound(color));
      group.querySelector('.preview')!.addEventListener('click', () => void invoke('preview', { color, source: source(color) }));
      group.querySelector('.browse')!.addEventListener('click', async () => {
        await invoke('set_dialog_open', { open: true });
        try {
          const path = await open({ multiple: false, directory: false, title: 'Choose a sound' });
          if (path) input(`${color}_file`).value = path;
        } finally { await invoke('set_dialog_open', { open: false }); }
      });
    }
    root.querySelector('#back')!.addEventListener('click', () => { inSettings = false; render(); });
    form.addEventListener('submit', async event => {
      event.preventDefault();
      const error = root.querySelector<HTMLElement>('#error')!;
      try {
        const command: unknown = JSON.parse(input('phone_command').value);
        if (!Array.isArray(command) || !command.every(item => typeof item === 'string')) throw new Error('Phone command must be a JSON array of strings.');
        const number = (name: string) => Number(input(name).value);
        const updated: Config = {
          dnd: input('dnd').value, hotkey: input('hotkey').value, stalled_after_secs: number('stalled_after_secs'),
          away_after_secs: number('away_after_secs'), phone_after_secs: number('phone_after_secs'), phone_command: command,
          escalation: { start_interval_secs: number('start_interval_secs'), min_interval_secs: number('min_interval_secs'), start_volume: number('start_volume'), ramp_secs: number('ramp_secs'), yellow_max_rings: number('yellow_max_rings'), yellow_delay_secs: number('yellow_delay_secs') },
          sounds: { blue: source('blue'), red: source('red'), yellow: source('yellow') },
        };
        version = await invoke('save_config', { config: updated, version });
        if (input('autostart').checked !== await invoke<boolean>('is_autostart_enabled')) await invoke('set_autostart', { enabled: input('autostart').checked });
        inSettings = false;
        render();
      } catch (cause) { error.textContent = String(cause); }
    });
  } catch (error) {
    root.innerHTML = `<header><button id="back" class="subtle">← Back</button><h1>Settings</h1></header><p class="empty">${esc(String(error))}</p>`;
    root.querySelector('#back')!.addEventListener('click', () => { inSettings = false; render(); });
  }
}

await listen<View>('state', event => { view = event.payload; render(); });
await listen('open-settings', () => void settings());
await invoke('refresh');
