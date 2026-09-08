import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile, writeFile, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// Optional real Chromium test. No extension or browser automation dependency;
// CDP controls only an isolated browser profile and a loopback fixture server.
const executable = process.argv[2];
if (!executable) throw new Error('Aufruf: node browser-smoke.mjs /absoluter/pfad/zu/chrome-headless-shell');
const artifacts = new URL('./artifacts/', import.meta.url);
await mkdir(artifacts, { recursive: true });
const profile = await mkdtemp(join(tmpdir(), 'uplink-dock-browser-'));
const names = ['chat', 'activity', 'points', 'stream-info'];
const html = new Map(await Promise.all(names.map(async name => [name, await readFile(new URL(`../${name}.html`, import.meta.url))])));
const policy = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src https: data:; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const server = createServer((request, response) => {
  const name = new URL(request.url, 'http://localhost').pathname.split('/').at(-1);
  if (!html.has(name)) { response.writeHead(404); response.end(); return; }
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Content-Security-Policy': policy });
  response.end(html.get(name));
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;
const chrome = spawn(executable, ['--no-sandbox', '--disable-gpu', '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
let socket;
try {
  const browserUrl = await new Promise((resolve, reject) => {
    let buffer = '';
    const timer = setTimeout(() => reject(new Error('Chromium-Start hat länger als 12 Sekunden gedauert')), 12_000);
    chrome.on('error', reject);
    chrome.on('exit', code => reject(new Error(`Chromium vor Bereitschaft beendet (${code})`)));
    chrome.stderr.on('data', data => {
      buffer += data;
      const match = buffer.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (match) { clearTimeout(timer); resolve(match[1]); }
    });
  });
  socket = new WebSocket(browserUrl);
  await once(socket, 'open');
  let nextId = 1;
  const pending = new Map();
  const exceptions = [];
  socket.onmessage = event => {
    const data = JSON.parse(event.data);
    if (data.method === 'Runtime.exceptionThrown') exceptions.push(data.params.exceptionDetails.text);
    if (data.id && pending.has(data.id)) {
      const { resolve, reject, timer } = pending.get(data.id);
      clearTimeout(timer); pending.delete(data.id);
      if (data.error) reject(new Error(data.error.message)); else resolve(data.result);
    }
  };
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP-Zeitlimit: ${method}`)); }, 12_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  const cdp = (method, params) => send(method, params, sessionId);
  await cdp('Page.enable'); await cdp('Runtime.enable');
  await cdp('Emulation.setDeviceMetricsOverride', { width: 520, height: 800, deviceScaleFactor: 1, mobile: false });
  await cdp('Page.addScriptToEvaluateOnNewDocument', { source: `
    window.__probe = { sockets: [], requests: [] };
    window.WebSocket = class { constructor(url) { this.url = url; __probe.sockets.push(this); } close() {} };
    window.fetch = (url, options = {}) => new Promise((resolve, reject) => __probe.requests.push({ url, options, resolve, reject }));
    __probe.emit = data => __probe.sockets.at(-1).onmessage({data: JSON.stringify(data)});
    __probe.status = (generation = '11111111111111111111111111111111') => __probe.emit({typ:'status',generation,plattformen:[
      {platform:'twitch',eingerichtet:true,verbunden:true,zustand:'connected'},
      {platform:'youtube',eingerichtet:true,verbunden:true,zustand:'connected'}
    ]});
    __probe.respond = (index, body, status = 200) => __probe.requests[index].resolve({status,ok:status >= 200 && status < 300,json:async()=>body});
    __probe.input = (id,value) => {const input=document.getElementById(id);input.value=value;input.dispatchEvent(new Event('input',{bubbles:true}));};
    __probe.chat = (id,text) => __probe.emit({id,ereignis:{typ:'chat',platform:'youtube',channel_id:'10',channel_login:'testkanal',message_id:String(id),sender_id:'20',sender_login:'test',sender_display:'Test',fragments:[{art:'text',text}],sent_at:new Date().toISOString()}});
    __probe.points = (id,status='unfulfilled') => __probe.emit({id,ereignis:{typ:'points',platform:'twitch',channel_id:'10',redemption_id:'redemption-'+id,reward_id:'reward-'+id,reward_title:'Wasser trinken',cost:100,status,dedupe_key:id+':'+status,occurred_at:new Date().toISOString(),actor:{display:'Testzuschauer'},user_input:'Ein Glas Wasser und kurz durchatmen.'}});
  ` });
  const evaluate = async expression => {
    const result = await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result.value;
  };
  const flush = () => evaluate('new Promise(resolve => setTimeout(resolve, 30))');
  const report = [];
  for (const name of names) {
    await cdp('Page.navigate', { url: `${origin}/uplink/dock/${name}?t=synthetic-dock-fixture` });
    for (let attempt = 0; ; attempt++) {
      if (await evaluate('!!window.__probe && __probe.sockets.length > 0')) break;
      assert.ok(attempt < 30, 'Dock script must initialize');
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    await evaluate('__probe.status()');
    if (name === 'chat') {
      await evaluate(`__probe.chat(1,'Hallo zusammen! Schön, dass ihr da seid.'); __probe.chat(2,'Ein Geschenk'); __probe.chat(2,'Fünf Geschenke – danke euch!'); __probe.input('eingabe','Gesendeter Text'); document.getElementById('senden').click(); __probe.input('eingabe','Mein nächster Entwurf bleibt stehen.'); __probe.status();`);
      assert.equal(await evaluate('document.getElementById("senden").disabled'), true);
      await evaluate('__probe.respond(0,{ergebnisse:[{platform:"twitch",ok:true},{platform:"youtube",ok:true}]})');
      await flush();
      assert.equal(await evaluate('document.getElementById("eingabe").value'), 'Mein nächster Entwurf bleibt stehen.');
      assert.equal(await evaluate('document.querySelectorAll(".zeile").length'), 2);
      assert.match(await evaluate('document.getElementById("liste").textContent'), /Fünf Geschenke/);
    } else if (name === 'activity') {
      await evaluate(`__probe.emit({id:1,ereignis:{typ:'activity',platform:'youtube',channel_id:'10',art:'sub_gift',count:1,dedupe_key:'gift-1',actor:{display:'Goldfuchs'},occurred_at:new Date().toISOString()}}); __probe.emit({id:2,ereignis:{typ:'activity',platform:'youtube',channel_id:'10',art:'sub_gift',count:5,dedupe_key:'gift-1',actor:{display:'Goldfuchs'},occurred_at:new Date().toISOString()}}); __probe.emit({id:3,ereignis:{typ:'activity',platform:'twitch',channel_id:'10',art:'follow',dedupe_key:'follow-1',actor:{display:'NeueZuschauerin'},occurred_at:new Date().toISOString()}});`);
      assert.equal(await evaluate('document.querySelectorAll(".eintrag").length'), 2);
      assert.match(await evaluate('document.getElementById("liste").textContent'), /5 Abos/);
    } else if (name === 'points') {
      await evaluate(`__probe.points(1); __probe.points(2); __probe.points(1,'fulfilled');`);
      await evaluate('new Promise(resolve => setTimeout(resolve, 600))');
      assert.equal(await evaluate('document.querySelectorAll(".karte").length'), 1);
      await evaluate(`document.querySelector('.ja').click(); __probe.points(2,'canceled'); __probe.respond(0,{ok:false,hinweis:'Schon erledigt'},409);`);
      await evaluate('new Promise(resolve => setTimeout(resolve, 600))');
      assert.equal(await evaluate('document.querySelectorAll(".karte").length'), 0);
      await evaluate('__probe.points(3); __probe.points(4);');
    } else {
      await evaluate(`__probe.respond(0,{ergebnisse:[{platform:'twitch',ok:true,info:{title:'Heute gemeinsam spielen',category_id:'1',category_name:'Deadlock',tags:['Deutsch','Community']}}]})`);
      await flush();
      await evaluate(`__probe.input('titel','Gesendeter Titel'); document.getElementById('speichern').click(); __probe.input('titel','Der nächste Titel bleibt mein Entwurf'); __probe.respond(1,{ergebnisse:[{platform:'twitch',ok:true,info:{title:'Gesendeter Titel',category_id:'1',category_name:'Deadlock',tags:['Deutsch','Community']}}]})`);
      await flush();
      assert.equal(await evaluate('document.getElementById("titel").value'), 'Der nächste Titel bleibt mein Entwurf');
      assert.equal(await evaluate('document.getElementById("nichtgespeichert").classList.contains("da")'), true);
      assert.equal(await evaluate('document.getElementById("ergebnis").textContent'), 'Vorheriger Stand auf Twitch gespeichert');
      await evaluate(`document.querySelector('.weg').click(); __probe.input('kategorie','Deadlock');`);
      await evaluate('new Promise(resolve => setTimeout(resolve, 350))');
      await evaluate(`__probe.input('kategorie',''); __probe.respond(2,{ergebnisse:[{platform:'twitch',ok:true,info:[{id:'1',name:'Deadlock'}]}]})`);
      await flush();
      assert.equal(await evaluate('document.getElementById("vorschlaege").style.display'), 'none');
    }
    await evaluate('__probe.sockets.at(-1).onclose({code:1012})');
    await evaluate('new Promise(resolve => setTimeout(resolve, 1200))');
    assert.equal(await evaluate('new URL(__probe.sockets.at(-1).url).searchParams.get("gen")'), '11111111111111111111111111111111');
    await evaluate('__probe.status("22222222222222222222222222222222"); __probe.sockets.at(-1).onclose({code:1012})');
    await evaluate('new Promise(resolve => setTimeout(resolve, 1200))');
    assert.equal(await evaluate('new URL(__probe.sockets.at(-1).url).searchParams.get("gen")'), '22222222222222222222222222222222');
    assert.equal(await evaluate('new URL(__probe.sockets.at(-1).url).searchParams.get("seit")'), '0');
    await evaluate('__probe.status("22222222222222222222222222222222")');
    assert.equal(await evaluate('document.documentElement.scrollWidth > innerWidth'), false, `${name}: no horizontal page overflow`);
    // Capture the final state after the retained entry/feedback animations.
    await evaluate('new Promise(resolve => setTimeout(resolve, 550))');
    const { data } = await cdp('Page.captureScreenshot', { format: 'png' });
    await writeFile(new URL(`${name}.png`, artifacts), Buffer.from(data, 'base64'));
    report.push({ dock: name, assertions: 'passed', screenshot: `${name}.png`, viewport: '520×800' });
  }
  assert.deepEqual(exceptions, []);
  const browser = await send('Browser.getVersion');
  await writeFile(new URL('report.json', artifacts), JSON.stringify({ browser: browser.product, source: 'actual dock HTML', network: 'loopback only; fetch/websocket mocks; no platform accounts', fonts: 'local brand fonts unavailable in isolated fixture; system fallback rendered', policy, report }, null, 2));
  console.log(JSON.stringify({ browser: browser.product, docks: report.length, assertions: 'passed', exceptions: exceptions.length, artifacts: artifacts.pathname }));
} finally {
  socket?.close();
  chrome.kill('SIGTERM');
  server.close();
  if (chrome.exitCode === null && chrome.signalCode === null) await once(chrome, 'exit').catch(() => {});
  await rm(profile, { recursive: true, force: true });
}
