# Chat und OBS-Docks

Eigenständiges Rustmodul ohne Abhängigkeit auf den alten Medienkern, SQL, den Uplink-Dienst oder eine zweite OAuth-Verwaltung. Die geprüften Plattformadapter und Wiremodelle stammen gezielt aus dem bisherigen Chatmodul; Hub, Dockauthentifizierung, Router, Lebensdauer und Replaygrenzen sind neu. Die vier bisherigen HTML-Docks werden mit korrigierten Zustandsübergängen eingebunden.

## Dienstvertrag

```rust,ignore
let hub = ChatHub::new(config, dock_identity, platform_broker)?;
let app = service_router.merge(uplink_chat::router(hub.clone()));
hub.set_active(streamer_id, true)?; // tatsächlicher Medienstart
hub.set_active(streamer_id, false)?; // tatsächliches Medienende
hub.shutdown().await;              // beim Herunterfahren des Dienstes
```

`DockIdentity::resolve([u8; 32])` löst SHA-256 eines Docktokens über die bestehende persistente Speicherung in `DockUser { streamer_id: u64, enabled: bool }` auf. Positive IDs entsprechen der authentifizierten Twitch-Identität; keine Auflösung nach Anzeigenamen. Rotation, Verschlüsselung und Erzeugung der bestehenden `dock_`-Adressen bleiben Verantwortung des Dienstes. Auch bestehende WebSockets prüfen den Hash erneut; widerrufene Adressen schließen mit 1008, vorübergehend nicht prüfbare Zugänge mit 1013.

`PlatformBroker::grant(streamer_id, platform, refresh)` liefert nur einen flüchtigen Grant: Access-Token, Ablauf, Plattform-ID, Login und tatsächlich gewährte Scopes. `refresh=true` bedeutet einen neuen Abruf nach einer Plattform-401, keine eigene OAuth-Implementierung. Reguläre Überprüfung erzwingt keinen Tokenrefresh. Gleichzeitige Abrufe derselben Identität werden zusammengefasst; Grants haben geschütztes Debug und werden beim Drop überschrieben. Kein Refresh-Token, keine Token-Datei und keine ENV-Konfiguration im Chatmodul.

`metrics` und `chatter_history` verwenden die bestehenden internen Kennzahlen-/Verlaufpfade über denselben injizierten Broker. Fehlende Implementierung liefert ausdrücklich `Unavailable`; sie erfindet weder historische Zuschauerzahlen noch Erstchatter. Der Dienstadapter benötigt eigene begrenzte Anfragezeiten und Antwortgrößen. Der Chat-Hub begrenzt seine Aufrufe zusätzlich.

`ChatConfig` enthält eine exakte Originliste, höchstens aktive Nutzer und WebSockets pro Nutzer sowie eine Leerlauffrist. OBS-Fenster desselben Nutzers teilen einen Bus und dieselben Plattformverbindungen. `set_active` verhindert die Verwechslung eines geöffneten Docks mit einer echten Livesession. Der öffentliche Dienst muss außerhalb seiner ausdrücklich weitergeleiteten Routen unverändert intern geschützt bleiben; diese Routerrouten verwenden Dockauthentifizierung bzw. Kick-Signatur.

## Tatsächlich implementiert

- Twitch: EventSub-WebSocket, Helix-Chatversand mit geprüftem `is_sent`, getrennte optionale Aktivitätsrechte, Tokenwiderruf, begrenzter Reconnect mit erlaubtem Twitch-Zielhost, Kanalpunkt-Add und -Update; Titel/Kategorie/Tags und Einlösungen über Helix.
- Kick: echte Chat-/Aktivitätsabonnements und Chatversand; RSA-Signatur über ID, Zeitstempel und unveränderten Body; begrenzte Replayliste. Eine volle oder geschlossene Eingangsqueue antwortet 503 und verhindert eine falsche Zustellbestätigung. Der Retry bleibt zustellbar.
- YouTube: aktiven Broadcast finden, Livechat mit dokumentiertem Pollingabstand und Quotenpause lesen, Textnachrichten senden; Read-/Writefehler bleiben sichtbar. Die frühere unzulässige Kombination `mine` plus `broadcastStatus` und der aus dem Bestand übernommene falsche Chatpfad wurden korrigiert. GET und POST verwenden `/youtube/v3/liveChat/messages`. YouTube bestätigt den Versand mit einer Nachrichten-ID, Kick mit `is_sent=true`; eine unvollständige HTTP-200-Antwort wird nicht als Erfolg ausgegeben.
- Antworten aus dem Dock starten gleichzeitig an allen verbundenen unterstützten Plattformen. Jede Plattform liefert ein eigenes Ergebnis. Keine verbundenen Empfänger oder ein ungeklärter Abschluss ergeben keinen behaupteten Erfolg.
- Replay bewahrt neue Inhalte mit gleicher fachlicher Ereignis-ID; identische Wiederholung wird entdoppelt. Rein lokale Hervorhebung ändert die fachliche Identität nicht. Generation bleibt als JavaScript-Zahl exakt darstellbar.

## Grenzen und offene Integration

Keine allgemeine Moderationsoberfläche mit Löschen, Timeout oder Ban übernommen: diese Funktionen waren im bisherigen Chatmodul nicht vollständig gebaut. Der gemeinsame Ausbau bleibt offen. Stream-Info-Verwaltung ist tatsächlich für Twitch vorhanden; Kick/YouTube-Titelverwaltung ist damit nicht freigegeben. TikTok meldet fehlende nachgewiesene Chatintegration ausdrücklich und erzeugt keine simulierten Nachrichten.

Die aktuell bekannten Twitch-Grantmetadaten des aktivierten Uplink-Ziels enthalten weder moderne EventSub-Chatrechte noch Legacy-IRC-Chatrechte. Die vorhandenen Rechte für Titel und Einlösungen werden unabhängig vom Chat geprüft. Neue Freigabe erfolgt über den bereits existierenden `scope_profile=uplink`-OAuthweg, der im geprüften Twitch-Bot-Quellstand die benötigten Scopes enthält. Keine manuell kopierten Tokens.

Replay: höchstens 1024 Ereignisse bzw. 4 MiB und 15 Minuten pro Nutzer; maximal 32 KiB pro normalisiertem Ereignis, Broadcastqueue 256, Dedupe 2048 Hashpaare. HTTP-Plattformantworten maximal 1 MiB, Twitch-Frames/-Nachrichten maximal 256 KiB. Parser, Queue, Auth und Handshake haben getrennte Grenzen. Eine große vollständige Community-Langzeitstatistik und deren Kapazitätsnachweis sind kein Ergebnis dieses Moduls; historische Zahlen stammen weiterhin vom bestehenden Broker.

Die API ist axum-0.8-kompatibel. HTML und WebSockets brauchen den erlaubten Origin; Schreibaufrufe zusätzlich `X-Dock-Token`. Caddy muss wie bisher den Docktoken aus Zugriffslogs entfernen. Keine Roh-Request-URI mit `?t=` protokollieren. Die Live-Datenbank und der tatsächliche zentrale Broker wurden in dieser isolierten Crateprobe nicht verwendet.

## Prüfung am 8. September 2026

```sh
/home/nathanael/.cargo/bin/cargo test -j2 -p uplink-chat
/home/nathanael/.cargo/bin/cargo clippy -j2 -p uplink-chat --all-targets -- -D warnings
npm test --prefix web/docks/tests
```

126 Rusttests grün, darunter echte lokale WebSocketverbindung mit Replay/Update und Tokenwiderruf, gleichzeitige Plattformzustellung, Origins, persistente Dockidentität, Queue-/Replaygrenzen, OAuth-Fehlerklassen, Wiederverbindung und RSA-Prüfung. Die zusammengefassten Grantabrufe wurden zuvor rot nachgewiesen (8 statt 1 Abruf), danach grün. Der offizielle YouTube-Requestpfad und fehlende Sendebestätigungen bei Kick/YouTube wurden ebenfalls zuerst rot nachgewiesen. YouTube-Sendefehler unterscheiden zwischen Kontingent, beendetem Chat und fehlender Freigabe; der Verbindungsstatus setzt eine erfolgreich gelesene Antwort voraus und wird bei Fehlern zurückgesetzt. Bestehende Adaptertests verwenden isolierte HTTP-/WebSocketserver und synthetische Grants.

24 DOM-Regressionen sowie vier echte lokale Chromium-Bedienproben inklusive Screenshots sind in `web/docks/tests/README.md` beschrieben. Das ist kein eingeloggter Produktiv-Dock-/OBS-/Vier-Plattform-Nachweis. Serviceadapter, Migration, gemeinsame unabhängige Rust-/Sicherheitsprüfung und zentraler Gate folgen auf den Integrationscommit. Kein Stream, Chatversand oder OAuth-Abschluss auf einer echten Plattform ausgeführt.

Primärquellen erneut gelesen: [Twitch Chat](https://dev.twitch.tv/docs/chat/), [EventSub-Typen](https://dev.twitch.tv/docs/eventsub/eventsub-subscription-types/), [Kick Webhook-Sicherheit](https://github.com/KickEngineering/KickDevDocs/blob/main/events/webhook-security.md), [Kick OpenAPI mit Chatantwort](https://api.kick.com/swagger/doc.yaml), [YouTube Broadcastliste](https://developers.google.com/youtube/v3/live/docs/liveBroadcasts/list), [YouTube Livechatliste](https://developers.google.com/youtube/v3/live/docs/liveChatMessages/list), [YouTube Chatversand](https://developers.google.com/youtube/v3/live/docs/liveChatMessages/insert). Sie belegen die Protokolle und API-Verträge, keine Accountfreigabe.
