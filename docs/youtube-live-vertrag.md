# YouTube-Live-Adapter: Schnittstellenvertrag

Stand: 8. September 2026. Gilt für das Crate `crates/uplink-youtube-live`. Der Adapter bereitet einen YouTube-Broadcast vor, bindet ihn an einen Stream, startet ihn, meldet seinen Zustand und beendet ihn auf ausdrücklichen Auftrag. Er besitzt keine Medien, keinen Coordinator und keine Tokens. VOD, `videos.insert`, Archiv und Drive-Fallback gehören nicht dazu.

## Quellen des YouTube-Vertrags

Geprüft am 8. September 2026 gegen die offizielle Referenz (`developers.google.com/youtube/v3/live/docs`):

| Aufruf | Pflichtfelder / Bedingungen | Quota |
| --- | --- | --- |
| `liveStreams.insert` (`part=snippet,cdn,contentDetails,status`) | `snippet.title` (1 bis 128 Zeichen), `cdn.frameRate`, `cdn.ingestionType`, `cdn.resolution`; `contentDetails.isReusable` optional | 50 |
| `liveStreams.list` (`id=` oder `mine=true`, `part=id,snippet,cdn,status`) | liefert `cdn.ingestionInfo.streamName`, `rtmpsIngestionAddress`, `status.streamStatus` (`active`, `created`, `error`, `inactive`, `ready`) | 1 |
| `liveBroadcasts.insert` (`part=snippet,status,contentDetails`) | `snippet.title` (bis 100 Zeichen), `snippet.scheduledStartTime`, `status.privacyStatus`; optional `contentDetails.enableAutoStart`, `enableAutoStop`, `monitorStream.enableMonitorStream` | 50 |
| `liveBroadcasts.bind` (`id`, `streamId`, `part=id,contentDetails`) | ohne `streamId` löst der Aufruf die Bindung; Antwort enthält `contentDetails.boundStreamId` | 50 |
| `liveBroadcasts.transition` (`id`, `broadcastStatus`, `part=id,status`) | `testing` und `live` verlangen `status.streamStatus=active` am gebundenen Stream; `testing` verlangt Monitorstream; `complete` beendet | 50 |
| `liveBroadcasts.list` (`id=` oder `mine=true&broadcastStatus=…`, `part=id,snippet,status,contentDetails`) | `status.lifeCycleStatus`: `created`, `ready`, `testing`, `testStarting`, `live`, `liveStarting`, `complete`, `revoked` | 1 |

Mit `enableAutoStart=true` schaltet YouTube den Broadcast selbst live, sobald Daten am gebundenen Stream ankommen; mit `enableAutoStop=true` beendet YouTube ihn kurz nach dem Ende der Daten. Beides sind Zusagen von YouTube, keine Zusagen dieses Adapters: der Adapter prüft den Ist-Zustand immer über `list`.

Ein Nutzerlauf kostet rund 200 Einheiten (Stream neu) oder 150 Einheiten (Stream wiederverwendet) plus je Statusabfrage 2 Einheiten. Das Standardkontingent von 10.000 Einheiten pro Tag reicht für etwa 40 Streams pro Tag über alle Nutzer, wenn der Aufrufer den Status alle 30 Sekunden abfragt und pro Stream zwei Stunden läuft. Der Adapter cached nichts über Prozessgrenzen hinweg und garantiert keine Exactly-once-Ausführung.

Benötigter OAuth-Scope: `https://www.googleapis.com/auth/youtube.force-ssl` (der bestehende Bot-Grant trägt genau diesen Scope) oder `https://www.googleapis.com/auth/youtube`. Fehlt beides, blockiert der Adapter mit `RechteFehlen`.

## Identität und Tokens

```rust
pub struct Identitaet {
    pub streamer_id: i64,            // relay.users.streamer_id, Twitch-ID des Nutzers
    pub channel_id: String,          // bestätigte YouTube-Kanal-ID (UC…)
    pub connection_generation: i64,  // relay.destination_fences.generation für youtube
}
```

Tokens kommen ausschließlich über `uplink_chat::token::TokenQuelle` (Trait `PlatformBroker`, Pfad `/twitch/api/v2/internal/platform-token`). Vor jedem Aufruf gilt:

1. `grant.platform_user_id` muss `identitaet.channel_id` gleichen, sonst `Blockiert(IdentitaetAbweichung)` ohne API-Aufruf.
2. `grant.scopes` muss den Live-Scope enthalten, sonst `Blockiert(RechteFehlen)`.
3. Nach einem 401 wird der Grant einmal invalidiert (`TokenQuelle::invalidieren`) und der Aufruf einmal wiederholt; ein zweites 401 ergibt `Blockiert(NeuAnmeldungNoetig)`.

Der Adapter speichert nie ein Token, nie den `streamName`. Beide bleiben im RAM und werden mit `zeroize` gelöscht.

## Einstellungen und Freigabe

```rust
pub enum Sichtbarkeit { Private, Unlisted, Public }   // Anfangswert Private

pub struct LiveEinstellungen {
    pub titel: String,               // 1 bis 100 Zeichen, keine Steuerzeichen
    pub sichtbarkeit: Sichtbarkeit,
    pub auto_start: bool,            // Anfangswert false
    pub auto_stop: bool,             // Anfangswert false
    pub live_freigegeben_at: Option<DateTime<Utc>>,
    pub connection_generation: i64,  // Generation, unter der die Freigabe erteilt wurde
}
```

Ohne `live_freigegeben_at` legt `prepare` nichts an und antwortet `Blockiert(KeineFreigabe)`. Eine Freigabe gilt nur für die Verbindungsgeneration, unter der sie erteilt wurde. Eine neue Verbindung (höhere Generation) macht die Freigabe ungültig; der Nutzer bestätigt sie im Dashboard erneut. Eine gespeicherte Verbindung ohne Freigabe ist nie eine Veröffentlichungserlaubnis.

`RunStore::einstellungen_speichern(streamer_id, LiveEinstellungen)` und `RunStore::einstellungen_laden(streamer_id)` sind die einzigen Schreib- und Lesewege. Der Aufrufer (Dashboard über Bot und Uplink-API) setzt `live_freigegeben_at` nur aus einer ausdrücklichen Nutzeraktion.

## Öffentliche Adapter-API

```rust
pub struct YouTubeLive { /* api, store, tokens, Sperre je Nutzer */ }

impl YouTubeLive {
    pub fn new(api: Arc<dyn LiveApi>, store: Arc<dyn RunStore>, tokens: Arc<TokenQuelle>) -> Self;

    pub async fn prepare(&self, id: &Identitaet, anforderung: RunAnforderung) -> Result<Vorbereitung, LiveFehler>;
    pub async fn start(&self, id: &Identitaet) -> Result<Zustand, LiveFehler>;
    pub async fn status(&self, id: &Identitaet) -> Result<Zustand, LiveFehler>;
    pub async fn medien_unterbrochen(&self, id: &Identitaet) -> Result<Zustand, LiveFehler>;
    pub async fn finish(&self, id: &Identitaet, grund: Endegrund) -> Result<Zustand, LiveFehler>;
}

pub struct RunAnforderung { pub uplink_session: String }   // opake Session-Kennung des Aufrufers, 1 bis 128 Zeichen

pub struct Vorbereitung { pub zustand: Zustand, pub ingest: Option<IngestZugang> }

pub struct IngestZugang {
    pub rtmps_url: String,                    // cdn.ingestionInfo.rtmpsIngestionAddress
    pub stream_name: Zeroizing<String>,       // cdn.ingestionInfo.streamName, Debug zeigt [geschützt]
}

pub struct Referenzen { pub run_id: i64, pub broadcast_id: String, pub stream_id: String }

pub enum Zustand {
    Inaktiv,
    Vorbereitet { refs: Referenzen },                           // gebunden, wartet auf Medien
    Sendet { refs: Referenzen, stream_status: String },         // Stream active, Broadcast noch nicht live
    Live { refs: Referenzen, seit: DateTime<Utc> },             // lifeCycleStatus=live und boundStreamId passt
    Beendet { refs: Referenzen, grund: Endegrund, youtube_bestaetigt: bool },
    Blockiert { grund: Blockgrund, refs: Option<Referenzen> },
    Fehler { refs: Option<Referenzen>, fehler: ApiFehler, wiederaufnehmbar: bool },
    Unklar { refs: Option<Referenzen>, schritt: Schritt },      // POST ohne Antwort, nächster Aufruf gleicht ab
}

pub enum Blockgrund {
    KeineFreigabe, RechteFehlen, NeuAnmeldungNoetig, LiveNichtFreigeschaltet,
    IdentitaetAbweichung, VeralteteGeneration, UnklareZuordnung, Quota,
}

pub enum Schritt { StreamInsert, BroadcastInsert, Bind, TransitionLive, TransitionComplete }

pub enum Endegrund { NutzerStop, SessionEndeBestaetigt, AdminStop }

pub enum LiveFehler {
    Ungueltig(&'static str),   // Eingaben (Titel, Session-Kennung, IDs)
    Store(&'static str),       // Datenbank
    Broker(TokenFehler),       // Tokenbroker nicht erreichbar oder abgewiesen
    Belegt,                    // paralleler Aufruf für denselben Nutzer läuft
}
```

### `prepare`

1. Nutzer-Sperre nehmen (`Belegt` bei Parallelaufruf, keine Warteschlange).
2. Einstellungen laden. Ohne Freigabe `Blockiert(KeineFreigabe)`. `id.connection_generation` kleiner als gespeichert: `Blockiert(VeralteteGeneration)`. Größer: Freigabe verfallen, `Blockiert(KeineFreigabe)`.
3. Aktiven Run laden. Run mit kleinerer Generation wird als `beendet` mit Grund `generation_ueberholt` geschlossen, ohne YouTube-Aufruf. Run derselben Generation in `vorbereitet`, `sendet` oder `live` wird zurückgegeben (idempotent), `unklar` wird abgeglichen (siehe unten), `beendet` führt zu einem neuen Run. Ein abgeschlossener Broadcast wird nie wieder als startfähig angenommen.
4. Grant holen, Identität und Scope prüfen.
5. Stream: gespeicherte `stream_id` per `liveStreams.list?id=` prüfen. Passt (`cdn.ingestionType=rtmp`, `streamStatus` nicht `error`, gehört laut `snippet.channelId` zum Kanal), wird er wiederverwendet. Sonst `liveStreams.insert` mit `snippet.title="Uplink"`, `cdn.frameRate=variable`, `cdn.ingestionType=rtmp`, `cdn.resolution=variable`, `contentDetails.isReusable=true`.
6. Broadcast: `liveBroadcasts.insert` mit Titel, `scheduledStartTime=jetzt`, `privacyStatus`, `enableAutoStart`, `enableAutoStop`, `monitorStream.enableMonitorStream=false`. Titel und Beschreibung enthalten keine Run-, Session- oder sonstigen technischen Kennungen.
7. `liveBroadcasts.bind`; Antwort muss `boundStreamId == stream_id` tragen.
8. Kontrolle per `liveBroadcasts.list?id=`: `lifeCycleStatus` in `created` oder `ready`, `boundStreamId` passt, `enableAutoStart`/`enableAutoStop` entsprechen der Einstellung. Weicht etwas ab: `Fehler{wiederaufnehmbar:false}`.
9. Run als `vorbereitet` speichern. `IngestZugang` aus dem Stream-Ressourcenstand zurückgeben.

Jeder POST (Schritte 5 bis 7) wird vorher als offener Schritt (`schritt`, `schritt_seit`) im Run gespeichert und nachher gelöscht. Bricht der Prozess dazwischen ab, ist der Run `unklar`.

### Abgleich bei `unklar`

| Schritt | Abgleich | Ergebnis |
| --- | --- | --- |
| `StreamInsert` | `liveStreams.list?mine=true`, Kandidaten: Titel `Uplink`, `publishedAt` nicht älter als `schritt_seit` minus 60 s | genau einer: übernehmen; keiner: Insert wiederholen; mehrere: `Blockiert(UnklareZuordnung)` |
| `BroadcastInsert` | `liveBroadcasts.list?mine=true&broadcastStatus=upcoming` plus `active`, Kandidaten: gleicher Titel, `publishedAt` nicht älter als `schritt_seit` minus 60 s | genau einer: übernehmen; keiner: Insert wiederholen; mehrere: `Blockiert(UnklareZuordnung)` |
| `Bind` | `liveBroadcasts.list?id=`: `boundStreamId` | passt: fertig; sonst Bind wiederholen |
| `TransitionLive` | `lifeCycleStatus` | `live`/`liveStarting`: fertig; `complete`: `Beendet`; sonst wiederholen |
| `TransitionComplete` | `lifeCycleStatus` | `complete`: `Beendet`; sonst wiederholen |

Ein blockierter Run wird nie stillschweigend durch einen fremden Broadcast ersetzt. Der Nutzer sieht die Blockade im Dashboard und beendet oder löscht die Kandidaten selbst in YouTube Studio.

### `start`

Nur nötig bei `auto_start=false`. Der Aufrufer ruft `start`, sobald der Medienpfad Daten an YouTube sendet. Der Adapter vermerkt den Wunsch als `start_angefordert_at` im Run und prüft `streamStatus=active`; ist der Stream noch nicht aktiv, antwortet er `Sendet{stream_status}` ohne Transition. Bei `active` und passender Bindung folgt `liveBroadcasts.transition?broadcastStatus=live` als offener Schritt. Bei `auto_start=true` ist `start` ein reiner Statusabruf. Weil `start_angefordert_at` gesetzt bleibt, führt der nächste `status` die Transition selbst nach, sobald der Stream aktiv wird; nach `Live` wird das Feld nicht mehr gebraucht.

### `status`

Liest Stream und Broadcast per `list` (je 1 Einheit). Zuordnung:

| Beobachtung | Zustand |
| --- | --- |
| kein aktiver Run | `Inaktiv` |
| `lifeCycleStatus=live` und `boundStreamId == stream_id` | `Live` |
| `lifeCycleStatus=live`, aber andere Bindung | `Fehler{wiederaufnehmbar:false}` (fremdes Ereignis, nicht übernehmen) |
| `start_angefordert_at` gesetzt, `streamStatus=active`, Broadcast `ready`/`created`, Bindung passt | Transition nach `live` nachführen, danach `Live` oder `Sendet` |
| `streamStatus=active`, Broadcast `ready`, `liveStarting` oder `testing` | `Sendet` |
| `streamStatus` nicht `active`, Broadcast `created`/`ready` | `Vorbereitet` |
| `lifeCycleStatus=complete` | `Beendet{youtube_bestaetigt:true}`, Run wird geschlossen |
| `lifeCycleStatus=revoked` oder Broadcast fehlt (404) | `Fehler{wiederaufnehmbar:false}`, Run wird geschlossen |
| Quota oder Ratelimit | `Fehler{wiederaufnehmbar:true}`, gespeicherter Zustand bleibt |
| Transportfehler | `Fehler{wiederaufnehmbar:true}`, gespeicherter Zustand bleibt |

### `medien_unterbrochen`

Vermerkt nur, dass der Aufrufer keine Medien mehr liefert (`unterbrochen_at`). Kein YouTube-Aufruf, keine Transition. Bei `auto_stop=true` beendet YouTube selbst; `status` meldet das dann als `Beendet`.

### `finish`

Verlangt einen `Endegrund`. Ein TCP-Abbruch, ein Ingest-Timeout oder ein Neustart sind keine Endegründe; dafür gibt es `medien_unterbrochen`. Ablauf: Run laden, Generation prüfen, `lifeCycleStatus` lesen. `complete`: Run schließen, `youtube_bestaetigt=true`. `live`, `liveStarting`, `testing`: `transition?broadcastStatus=complete` als offener Schritt, danach Run schließen. `created`/`ready` (YouTube lässt hier kein `complete` zu): Run lokal schließen mit `youtube_bestaetigt=false`; der Broadcast bleibt als geplantes Ereignis stehen und wird beim nächsten `prepare` nicht wiederverwendet.

## Persistenz

Migration `db/migrations/20260908_youtube_live.sql` (additiv, Registrierung in `migrations.rs` übernimmt Codex):

- `relay.youtube_live_settings`: eine Zeile je `streamer_id` (FK `relay.users`), `channel_id`, `connection_generation`, `titel`, `sichtbarkeit`, `auto_start`, `auto_stop`, `live_freigegeben_at`, `stream_id` (wiederverwendbarer Stream, kanalgebunden), `updated_at`.
- `relay.youtube_live_runs`: `run_id` (Identity), `streamer_id`, `channel_id`, `connection_generation`, `uplink_session`, `zustand`, `schritt`, `schritt_seit`, `stream_id`, `broadcast_id`, `titel`, `sichtbarkeit`, `auto_start`, `auto_stop`, `fehler`, `ende_grund`, `youtube_bestaetigt`, `unterbrochen_at`, `live_seit`, `start_angefordert_at`, `created_at`, `updated_at`, `ended_at`. Eindeutiger Teilindex: höchstens ein Run je `streamer_id` mit `ended_at IS NULL`.

Alle Zustandswechsel sind einzelne `UPDATE … WHERE zustand=… AND connection_generation=… RETURNING`-Statements (Compare-and-set). Ein Statement, das keine Zeile trifft, meldet `LiveFehler::Store("Run wurde zwischenzeitlich geändert.")`.

```rust
pub trait SqlZugang: Send + Sync {
    fn query<'a>(&'a self, sql: &'a str, params: &'a [&'a (dyn ToSql + Sync)])
        -> BoxFuture<'a, Result<Vec<tokio_postgres::Row>, &'static str>>;
}
pub struct PostgresRunStore { sql: Arc<dyn SqlZugang> }
```

`uplink_service::store::Store::query` erfüllt `SqlZugang` mit einer Zeile Adapter-Code (Codex). Jede Query läuft dort in eigener Transaktion mit Frist; der Adapter verlässt sich auf keine Mehr-Statement-Transaktion.

## Aufrufervertrag (Coordinator, Codex)

1. `prepare` in eigener Tokio-Task mit Frist von höchstens 30 Sekunden aufrufen, nie innerhalb einer Sperre des Coordinators. Andere Plattformen starten unabhängig davon; das YouTube-Ziel bleibt bis zum Ergebnis `starting`.
2. Ergebnis `Vorbereitung.ingest` in das YouTube-Ziel des Medienpfads übernehmen (`rtmps_url` plus `stream_name`); der Adapter schreibt nicht in `relay.destinations`.
3. Nach Medienstart bei `auto_start=false` einmal `start` rufen, danach alle 30 Sekunden `status`. Ein `start` bei noch inaktivem Stream setzt nur `start_angefordert_at` und meldet `Sendet`; die eigentliche Transition holt der folgende `status` nach, sobald der Stream aktiv ist. `Live` ist die einzige Beobachtung, die als `publication_confirmed=true` gelten darf.
4. Bei Ingest-Verlust `medien_unterbrochen`; `finish` nur mit Endegrund aus Nutzerknopf, bestätigtem Sessionende oder Admin.
5. Nach einem Prozessneustart genügt `status`; der Adapter gleicht offene Schritte selbst ab.
6. `Blockiert` und `Fehler{wiederaufnehmbar:false}` sind sichtbare Endzustände des Ziels; kein automatischer Neuversuch ohne Nutzeraktion.
7. Einstellungen aus dem Dashboard über den Bot in `RunStore::einstellungen_speichern` schreiben; `live_freigegeben_at` nur aus einer ausdrücklichen Nutzerhandlung mit der aktuellen Generation setzen.

## Dashboard-Komponente

`bot/dashboard_v2/src/components/uplink/UplinkYouTubeLive.tsx` (Deadlock-Twitch-Bot) erhält Werte und Callbacks als Props, ohne eigenen Fetch oder OAuth:

```ts
export type YouTubeLiveSichtbarkeit = 'private' | 'unlisted' | 'public';
export type YouTubeLiveZustand = 'inaktiv' | 'vorbereitet' | 'sendet' | 'live' | 'beendet' | 'blockiert' | 'fehler' | 'unklar';
export interface YouTubeLiveEinstellungen { titel: string; sichtbarkeit: YouTubeLiveSichtbarkeit; autoStart: boolean; autoStop: boolean; freigegebenAm: string | null; }
export interface YouTubeLiveStatus { zustand: YouTubeLiveZustand; hinweis: string | null; broadcastId: string | null; seit: string | null; wiederaufnehmbar: boolean; }
export interface YouTubeLiveEntwurf { titel: string; sichtbarkeit: YouTubeLiveSichtbarkeit; autoStart: boolean; autoStop: boolean; liveFreigeben: boolean; }
export interface UplinkYouTubeLiveProps {
  verbunden: boolean; kanalName: string | null;
  einstellungen: YouTubeLiveEinstellungen | null; status: YouTubeLiveStatus | null;
  beschaeftigt: boolean; fehlerText?: string | null;
  onSpeichern: (entwurf: YouTubeLiveEntwurf) => void; onBeenden: () => void;
}
```

Die Anbindung an `api/uplink.ts`, `UplinkZiel.tsx` und die Bot-Handler übernimmt Codex. Benötigte Bot-Routen (Vorschlag, nicht gebaut): `GET/PUT /twitch/api/v2/uplink/youtube-live/einstellungen`, `GET /twitch/api/v2/uplink/youtube-live/status`, `POST /twitch/api/v2/uplink/youtube-live/beenden`; der Bot reicht sie an Uplink `GET/PUT /v1/me/youtube-live/settings`, `GET /v1/me/youtube-live/status`, `POST /v1/me/youtube-live/finish` weiter, jeweils mit `streamer_id` und `connection_generation`.

## Offene Aufruferanforderungen

- Uplink-Service: `SqlZugang`-Adapter für `Store`, Migration in `migrations.rs` eintragen, Routen für Einstellungen, Status und Ende, Aufruf von `prepare`/`start`/`status`/`medien_unterbrochen`/`finish` aus dem Sessionlebenszyklus, `publication_confirmed` aus `Zustand::Live`.
- Bot: Einstellungs- und Statusrouten mit Session-Identität, Weiterreichen der `connection_generation` aus `twitch_analytics`, Kanal-ID aus dem bestehenden YouTube-Grant.
- Dashboard: Komponente in `UplinkZiel.tsx` für `platform === 'youtube'` einhängen und mit `api/uplink.ts`-Aufrufen versorgen.

## Abweichungen der Implementierung

Stand der ersten Umsetzung des Crates `uplink-youtube-live`:

- `RunStore::einstellungen_speichern` nimmt zusätzlich die bestätigte `channel_id` entgegen (`einstellungen_speichern(streamer_id, channel_id, LiveEinstellungen)`), damit der kanalgebundene Stream in `youtube_live_settings.channel_id` verankert wird. `einstellungen_laden` liefert dazu passend `GespeicherteEinstellungen` mit `channel_id` und der gemerkten `stream_id`, und `run_anlegen` erhält die Anlagewerte über `RunNeu`.
- Spalte `youtube_live_runs.start_angefordert_at` trägt den bei `start` (nur `auto_start=false`) angeforderten Live-Wunsch. Sie wird per Compare-and-set gesetzt; der nächste `status` führt die Transition nach, sobald der Stream aktiv ist, die Bindung passt und der Lebenszyklus in `ready`/`created` steht. Das erfüllt REQ-04, verlagert die Transition aber vom `start`-Aufruf in den folgenden `status`.
- `liveStreams.list?mine=true` und `liveBroadcasts.list?mine=true` folgen `nextPageToken` über höchstens fünf Seiten (List-Antworten bis 1 MiB); reicht das für den Abgleich eines offenen Schritts nicht, meldet der Adapter `Blockiert(UnklareZuordnung)` statt blind fortzufahren. Kandidaten zählen nur mit parsbarem `publishedAt` im Fenster `[schritt_seit - 60 s, schritt_seit + 15 min]`; Stream-Kandidaten zusätzlich mit passender `snippet.channelId`, Broadcast-Kandidaten mit passendem Titel und leerer oder zum Stream passender `boundStreamId`.
- `GoogleLiveApi::mit_basis`/`mit_basis_und_frist` akzeptieren nur `https://`-Basen auf `www.googleapis.com` oder einem `.googleapis.com`-Host und liefern sonst `Err`. Für Tests gegen lokale Gegenstellen gibt es `GoogleLiveApi::mit_basis_ungeprueft` (mit `#[doc(hidden)]` markiert), das die Prüfung überspringt.
