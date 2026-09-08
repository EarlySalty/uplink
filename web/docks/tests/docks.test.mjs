import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { setImmediate } from 'node:timers/promises';
import test from 'node:test';
import { JSDOM, VirtualConsole } from 'jsdom';

const connected = [{ platform: 'twitch', eingerichtet: true, verbunden: true }];
const info = { title: 'Alter Titel', category_id: '1', category_name: 'Altes Spiel', tags: ['Deutsch'] };
const result = (value) => ({ ergebnisse: [{ platform: 'twitch', ok: true, info: value }] });

// Execute the actual unmodified inline dock scripts. Only external transports and
// time are controlled; assertions observe user inputs, DOM and real request bodies.
function dock(t, name) {
  const requests = [];
  const sockets = [];
  const errors = [];
  let now = 0;
  let nextTimer = 1;
  const timers = new Map();
  const console = new VirtualConsole();
  console.on('jsdomError', error => errors.push(error.message));
  const dom = new JSDOM(readFileSync(new URL(`../${name}.html`, import.meta.url), 'utf8'), {
    url: `https://uplink.test/uplink/dock/${name}?t=synthetic-dock-fixture`,
    runScripts: 'dangerously',
    virtualConsole: console,
    beforeParse(window) {
      window.setTimeout = (fn, delay = 0) => {
        const id = nextTimer++;
        timers.set(id, { fn, at: now + delay });
        return id;
      };
      window.clearTimeout = id => timers.delete(id);
      window.setInterval = () => nextTimer++;
      window.clearInterval = () => {};
      window.fetch = (url, options = {}) => new Promise((resolve, reject) => requests.push({ url, options, resolve, reject }));
      window.WebSocket = class {
        constructor(url) { this.url = url; sockets.push(this); }
        close() {}
      };
    },
  });
  t.after(() => {
    dom.window.close();
    assert.deepEqual(errors, [], 'No JavaScript errors in the real dock script');
  });
  const document = dom.window.document;
  const element = id => document.getElementById(id);
  const message = data => sockets.at(-1).onmessage({ data: JSON.stringify(data) });
  const status = () => message({ typ: 'status', generation: '11111111111111111111111111111111', plattformen: connected });
  function input(id, value) {
    element(id).value = value;
    element(id).dispatchEvent(new dom.window.Event('input', { bubbles: true }));
  }
  async function tick(ms) {
    const until = now + ms;
    for (;;) {
      const ready = [...timers].filter(([, timer]) => timer.at <= until).sort((a, b) => a[1].at - b[1].at)[0];
      if (!ready) break;
      timers.delete(ready[0]);
      now = ready[1].at;
      ready[1].fn();
      await setImmediate();
    }
    now = until;
    await setImmediate();
  }
  async function respond(request, body, statusCode = 200) {
    assert.ok(request, 'Expected a real dock fetch request');
    request.resolve({ status: statusCode, ok: statusCode >= 200 && statusCode < 300, json: async () => body });
    await setImmediate();
  }
  return { dom, document, element, message, status, input, tick, respond, requests, sockets };
}

function chatEvent(id, text, platform = 'youtube') {
  return { id, ereignis: { typ: 'chat', platform, channel_id: '10', channel_login: 'testkanal', message_id: 'message-1', sender_id: '20', sender_login: 'test', sender_display: 'Test', fragments: [{ art: 'text', text }], sent_at: new Date().toISOString() } };
}
function pointEvent(id, status = 'unfulfilled', platform = 'twitch') {
  return { id, ereignis: { typ: 'points', platform, channel_id: '10', redemption_id: 'redemption-1', reward_id: 'reward-1', reward_title: 'Wasser trinken', cost: 100, status, dedupe_key: `${platform}:redemption-1:${status}`, occurred_at: new Date().toISOString() } };
}

async function loadedInfo(t) {
  const app = dock(t, 'stream-info');
  app.status();
  await app.respond(app.requests[0], result(info));
  return app;
}
async function category(app, id, name) {
  app.element('katkarte-platz').querySelector('.weg')?.click();
  app.input('kategorie', name);
  await app.tick(300);
  await app.respond(app.requests.at(-1), result([{ id, name }]));
  app.element('vorschlaege').querySelector('button').click();
}
function tag(app, value) {
  app.input('tag', value);
  app.element('tag-dazu').click();
}

test('Chat: a successful send clears only the submitted draft', async t => {
  const app = dock(t, 'chat'); app.status();
  app.input('eingabe', 'Gesendeter Text'); app.element('senden').click();
  await app.respond(app.requests[0], { ergebnisse: [{ platform: 'twitch', ok: true }] });
  assert.equal(app.element('eingabe').value, '');
  assert.equal(app.element('senden').disabled, false);
});
test('Chat: a reply keeps a newer draft, even if edited back to the same text', async t => {
  const app = dock(t, 'chat'); app.status();
  app.input('eingabe', 'Gleicher Text'); app.element('senden').click();
  app.input('eingabe', 'Neuer Entwurf'); app.input('eingabe', 'Gleicher Text');
  await app.respond(app.requests[0], { ergebnisse: [{ platform: 'twitch', ok: true }] });
  assert.equal(app.element('eingabe').value, 'Gleicher Text');
});
test('Chat: a status frame cannot allow a second send while fetch is pending', async t => {
  const app = dock(t, 'chat'); app.status();
  app.input('eingabe', 'Hallo'); app.element('senden').click();
  app.status();
  assert.equal(app.element('senden').disabled, true);
  app.element('senden').click();
  assert.equal(app.requests.length, 1);
  app.message({ typ: 'status', generation: '11111111111111111111111111111111', plattformen: [] });
  await app.respond(app.requests[0], { ergebnisse: [{ platform: 'twitch', ok: true }] });
  assert.equal(app.element('senden').disabled, true);
});
test('Chat: an HTTP 200 with no accepted platform keeps the draft', async t => {
  const app = dock(t, 'chat'); app.status();
  app.input('eingabe', 'Noch nicht gesendet'); app.element('senden').click();
  await app.respond(app.requests[0], { ergebnisse: [{ platform: 'twitch', ok: false, hinweis: 'Neu verbinden' }] });
  assert.equal(app.element('eingabe').value, 'Noch nicht gesendet');
  assert.match(app.element('rueckmeldung').textContent, /Neu verbinden/);
});
test('Chat: an empty success envelope does not silently discard text', async t => {
  const app = dock(t, 'chat'); app.status();
  app.input('eingabe', 'Hallo'); app.element('senden').click();
  await app.respond(app.requests[0], { ergebnisse: [] });
  assert.equal(app.element('eingabe').value, 'Hallo');
  assert.notEqual(app.element('meldung').textContent, '');
});
test('Chat: an updated message replaces its row and keeps order/platform separation', t => {
  const app = dock(t, 'chat'); app.status();
  app.message(chatEvent(1, 'Ein Geschenk'));
  app.message(chatEvent(2, 'Anderer Chat', 'twitch'));
  app.message(chatEvent(3, 'Fünf Geschenke'));
  app.message(chatEvent(4, 'Fünf Geschenke'));
  const rows = app.document.querySelectorAll('.zeile');
  assert.equal(rows.length, 2);
  assert.match(rows[0].textContent, /Fünf Geschenke/);
  assert.match(rows[1].textContent, /Anderer Chat/);
});
test('Stream-Info: pending initial load preserves a typed title', async t => {
  const app = dock(t, 'stream-info');
  app.input('titel', 'Schon eingetippt');
  await app.respond(app.requests[0], result(info));
  assert.equal(app.element('titel').value, 'Schon eingetippt');
});
test('Stream-Info: saved response preserves all newer local field changes', async t => {
  const app = await loadedInfo(t);
  app.input('titel', 'Gesendeter Titel'); tag(app, 'Live');
  await category(app, '2', 'Gesendetes Spiel');
  app.element('speichern').click();
  const saving = app.requests.at(-1);
  app.input('titel', 'Nächster Titel'); tag(app, 'Später');
  await category(app, '3', 'Nächstes Spiel');
  await app.respond(saving, result({ title: 'Gesendeter Titel', category_id: '2', category_name: 'Gesendetes Spiel', tags: ['Deutsch', 'Live'] }));
  assert.equal(app.element('titel').value, 'Nächster Titel');
  assert.match(app.element('katkarte-platz').textContent, /Nächstes Spiel/);
  assert.match(app.element('chips').textContent, /Später/);
  assert.equal(app.element('nichtgespeichert').classList.contains('da'), true);
  assert.match(app.element('ergebnis').textContent, /Vorheriger Stand auf Twitch gespeichert/);
  app.element('speichern').click();
  assert.deepEqual(JSON.parse(app.requests.at(-1).options.body), { title: 'Nächster Titel', category_id: '3', tags: ['Deutsch', 'Live', 'Später'] });
});
test('Stream-Info: unchanged submitted fields become clean; a newer field remains dirty', async t => {
  const app = await loadedInfo(t);
  app.input('titel', 'Titel mit Rand '); tag(app, 'Live');
  app.element('speichern').click();
  const saving = app.requests.at(-1);
  tag(app, 'Später');
  await app.respond(saving, result({ ...info, title: 'Titel mit Rand', tags: ['Deutsch', 'Live'] }));
  assert.equal(app.element('titel').value, 'Titel mit Rand');
  assert.match(app.element('chips').textContent, /Später/);
  app.element('speichern').click();
  assert.deepEqual(JSON.parse(app.requests.at(-1).options.body), { tags: ['Deutsch', 'Live', 'Später'] });
});
test('Stream-Info: successful save without readback keeps a newer title dirty', async t => {
  const app = await loadedInfo(t);
  app.input('titel', 'Gesendet'); app.element('speichern').click();
  app.input('titel', 'Später');
  await app.respond(app.requests.at(-1), { ergebnisse: [{ platform: 'twitch', ok: true, hinweis: 'Gespeichert; Stand nicht lesbar' }] });
  assert.equal(app.element('titel').value, 'Später');
  assert.equal(app.element('nichtgespeichert').classList.contains('da'), true);
  assert.match(app.element('ergebnis').textContent, /Vorheriger Stand auf Twitch gespeichert/);
  assert.match(app.element('ergebnis').textContent, /Stand nicht lesbar/);
});
test('Stream-Info: editing after confirmation relabels the earlier saved revision', async t => {
  const app = await loadedInfo(t);
  app.input('titel', 'Gespeicherter Titel'); app.element('speichern').click();
  await app.respond(app.requests.at(-1), result({ ...info, title: 'Gespeicherter Titel' }));
  assert.equal(app.element('nichtgespeichert').classList.contains('da'), false);
  assert.equal(app.element('ergebnis').textContent, 'Auf Twitch gespeichert');
  app.input('titel', 'Neuer Entwurf');
  assert.equal(app.element('nichtgespeichert').classList.contains('da'), true);
  assert.equal(app.element('ergebnis').textContent, 'Vorheriger Stand auf Twitch gespeichert');
});
test('Search: clearing the input invalidates an already running response', async t => {
  const app = await loadedInfo(t);
  app.element('katkarte-platz').querySelector('.weg').click();
  app.input('kategorie', 'Deadlock'); await app.tick(300);
  const searching = app.requests.at(-1);
  app.input('kategorie', '');
  await app.respond(searching, result([{ id: '2', name: 'Deadlock' }]));
  assert.equal(app.element('vorschlaege').style.display, 'none');
  assert.equal(app.element('vorschlaege').children.length, 0);
});
test('Search: a replaced query invalidates old data before the debounce starts', async t => {
  const app = await loadedInfo(t);
  app.element('katkarte-platz').querySelector('.weg').click();
  app.input('kategorie', 'Deadlock'); await app.tick(300);
  const searching = app.requests.at(-1);
  app.input('kategorie', 'Andere');
  await app.respond(searching, result([{ id: '2', name: 'Deadlock' }]));
  assert.notEqual(app.element('vorschlaege').style.display, 'block');
});
for (const status of ['fulfilled', 'canceled']) {
  test(`Points: external ${status} removes the pending card across event IDs`, async t => {
    const app = dock(t, 'points'); app.status();
    app.message(pointEvent(1));
    assert.equal(app.document.querySelectorAll('.karte').length, 1);
    app.message(pointEvent(2, status));
    await app.tick(600);
    assert.equal(app.document.querySelectorAll('.karte').length, 0);
    assert.match(app.element('liste').textContent, /Nichts mehr offen/);
    app.message(pointEvent(3));
    assert.equal(app.document.querySelectorAll('.karte').length, 0, 'Late unfulfilled replay cannot reopen a completed redemption');
  });
}
test('Points: identical redemption IDs on different platforms stay separate', async t => {
  const app = dock(t, 'points'); app.status();
  app.message(pointEvent(1)); app.message(pointEvent(2, 'unfulfilled', 'kick'));
  app.message(pointEvent(3, 'fulfilled'));
  await app.tick(600);
  const cards = app.document.querySelectorAll('.karte');
  assert.equal(cards.length, 1);
  assert.equal(cards[0].getAttribute('data-platform'), 'kick');
});
test('Activity: gift updates replace the existing visible event', t => {
  const app = dock(t, 'activity'); app.status();
  const event = { typ: 'activity', platform: 'youtube', channel_id: '10', art: 'sub_gift', count: 1, dedupe_key: 'gift-1', occurred_at: new Date().toISOString() };
  app.message({ id: 1, ereignis: event });
  app.message({ id: 2, ereignis: { ...event, count: 5 } });
  app.message({ id: 3, ereignis: { ...event, count: 5 } });
  assert.equal(app.document.querySelectorAll('.eintrag').length, 1);
  assert.match(app.element('liste').textContent, /5 Abos/);
});
test('Docks: markup has no remote scripts or remote fonts', () => {
  for (const name of ['chat', 'activity', 'points', 'stream-info']) {
    const html = readFileSync(new URL(`../${name}.html`, import.meta.url), 'utf8');
    assert.doesNotMatch(html, /<script[^>]+src\s*=/i);
    assert.doesNotMatch(html, /@import|@font-face[\s\S]*?url\(["']?https?:/i);
    assert.match(html, /<html lang="de">/);
  }
});

for (const name of ['chat', 'activity', 'points', 'stream-info']) {
  test(`${name}: disconnected unconfigured platforms stay hidden despite a hint`, t => {
    const app = dock(t, name);
    const disconnected = { platform: 'kick', eingerichtet: false, verbunden: false, zustand: 'disconnected', hinweis: 'Nicht verbunden' };
    app.message({ typ: 'status', generation: '11111111111111111111111111111111', plattformen: [
      ...connected,
      disconnected,
      { platform: 'youtube', eingerichtet: true, verbunden: false, zustand: 'disconnected', hinweis: 'Verbindung wird wiederhergestellt' },
    ] });
    assert.match(app.element('plattformen').textContent, /Twitch/);
    assert.match(app.element('plattformen').textContent, /YouTube.*wiederhergestellt/);
    assert.doesNotMatch(app.element('plattformen').textContent, /Kick/);
    app.message({ typ: 'status', generation: '11111111111111111111111111111111', plattformen: [disconnected] });
    assert.equal(app.element('plattformen').textContent, 'Noch keine Plattform verbunden');
  });
  test(`${name}: unavailable broker and unsupported integration stay visible`, t => {
    const app = dock(t, name);
    app.message({ typ: 'status', generation: '11111111111111111111111111111111', plattformen: [
      { platform: 'kick', eingerichtet: false, verbunden: false, zustand: 'unavailable', hinweis: 'Kontoverbindung ist nicht erreichbar' },
      { platform: 'tiktok', eingerichtet: false, verbunden: false, zustand: 'unsupported', hinweis: 'TikTok-Chat ist noch nicht verfügbar' },
    ] });
    assert.match(app.element('plattformen').textContent, /Kick.*nicht erreichbar/);
    assert.match(app.element('plattformen').textContent, /TikTok.*nicht verfügbar/);
    assert.doesNotMatch(app.element('plattformen').textContent, /Noch keine Plattform verbunden/);
  });
}
test('Points: late failed local request cannot revive an externally completed card', async t => {
  const app = dock(t, 'points'); app.status();
  app.message(pointEvent(1));
  app.document.querySelector('.ja').click();
  app.message(pointEvent(2, 'fulfilled'));
  await app.respond(app.requests[0], { ok: false, hinweis: 'Schon erledigt' }, 409);
  assert.equal(app.document.querySelector('.ja').disabled, true);
  await app.tick(600);
  assert.equal(app.document.querySelectorAll('.karte').length, 0);
});
test('Stream-Info: partial platform failure leaves the submitted wishes retryable', async t => {
  const app = await loadedInfo(t);
  app.input('titel', 'Für beide Plattformen'); app.element('speichern').click();
  await app.respond(app.requests.at(-1), { ergebnisse: [
    { platform: 'twitch', ok: true, info: { ...info, title: 'Für beide Plattformen' } },
    { platform: 'kick', ok: false, hinweis: 'Nicht erreichbar' },
  ] });
  assert.equal(app.element('nichtgespeichert').classList.contains('da'), true);
  app.element('speichern').click();
  assert.deepEqual(JSON.parse(app.requests.at(-1).options.body), { title: 'Für beide Plattformen' });
});

for (const name of ['chat', 'activity', 'stream-info', 'points']) {
  test(name + ': bus generation resets cursor while same generation preserves it', async t => {
    const app = dock(t, name);
    const first = '11111111111111111111111111111111';
    const second = '22222222222222222222222222222222';
    app.message({ typ: 'status', generation: first, plattformen: connected });
    const sample = name === 'stream-info' ? { id: 3, ereignis: {typ:'info',platform:'twitch',channel_id:'10',title:'Alt'} } : name === 'points' ? pointEvent(3) : name === 'activity' ? {id:3,ereignis:{typ:'activity',platform:'twitch',channel_id:'10',art:'follow',user_display:'Test',occurred_at:'2026-09-08T10:00:00Z',dedupe_key:'follow-1'}} : chatEvent(3,'Alt');
    app.message(sample);
    app.message({ typ: 'status', generation: first, plattformen: connected });
    app.sockets.at(-1).onclose({code:1012});
    await app.tick(1000);
    let url = new URL(app.sockets.at(-1).url);
    assert.equal(url.searchParams.get('gen'), first);
    assert.equal(url.searchParams.get('seit'), '3');
    app.message({typ:'status',generation:second,plattformen:connected,nachlauf_unvollstaendig:true});
    app.sockets.at(-1).onclose({code:1012});
    await app.tick(1000);
    url = new URL(app.sockets.at(-1).url);
    assert.equal(url.searchParams.get('gen'), second);
    assert.equal(url.searchParams.get('seit'), '0');
  });
}
