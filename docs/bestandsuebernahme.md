# Gesicherte Oberfläche und vorhandene Integrationen

Stand: 8. September 2026. Grundlage ist der rs-relay-Commit
`78c6b4ab7ec65ba0f6f074ae1467f592d7ffd79c`. Die Herkunft und SHA-256-Prüfsummen
stehen in [bestandsmanifest.json](bestandsmanifest.json).

## Was in diesem Repository liegt

Die vier Dateien in `web/docks/` sind unveränderte, aus dem genannten Git-Commit
entnommene Quellassets für Chat, Aktivitäten, Kanalpunkte und Stream-Infos.
Sie enthalten HTML, CSS und Browser-JavaScript. Der Nutzer hat die Wiederverwendung
der vorhandenen Oberfläche erlaubt. Das ist eine Sicherung der Bedienoberfläche,
kein neuer JavaScript-Dienst und kein übernommener Medienkern.

Die Assets sind noch nicht an den neuen Rust-Dienst angeschlossen. Ihre enthaltenen
Browseraufrufe sprechen den bisherigen API-Vertrag an. Sie dürfen erst nach
Vertrags-, Berechtigungs- und Oberflächenprüfung als aktive Docks ausgeliefert werden.
Ein Datei- oder Quellcodevergleich belegt keine funktionierende Plattformverbindung.
Es wurden keine Dock-Zugänge, Kontotokens oder Nutzerkonfigurationen übernommen.

| Gesicherte Datei | Bisherige öffentliche Route | Aufgabe |
| --- | --- | --- |
| `web/docks/chat.html` | `/uplink/dock/chat` | Chat lesen, Nachricht senden, Hervorhebungen und Kennzahlen |
| `web/docks/activity.html` | `/uplink/dock/activity` | Plattformaktivitäten |
| `web/docks/points.html` | `/uplink/dock/points` | Kanalpunkte-Einlösungen bearbeiten |
| `web/docks/stream-info.html` | `/uplink/dock/stream-info` | Titel, Kategorie und weitere unterstützte Stream-Infos |

Die Dateien müssen später unter den vorgesehenen Dock-Routen ausgeliefert werden.
Ihre `basis()`-Funktion entfernt `/dock/<name>` aus dem Pfad. Ein beliebiger
Dateiserver unter `/web/docks/<name>.html` erfüllt diesen Vertrag nicht.

## Bisheriger Dock-Vertrag

Der Zugriff erfolgt bisher mit einem eigenen widerrufbaren Dock-Zugang. Der Browser
liest `t` aus der URL, entfernt den sichtbaren Parameter per `history.replaceState`
und sendet den Zugang für Schreibaufrufe im Header `X-Dock-Token`. Der WebSocket
verwendet weiterhin `t` als Query-Parameter. Das ist kein Plattform-OAuth-Token.
Auch künftig müssen Proxy- und Anwendungslogs solche Zugangswerte auslassen.

Alle Adressen werden relativ zum Dock-Ursprung gebildet. Auf der bisherigen
Hauptdomain entfernt Caddy `/uplink` und leitet nur die erlaubten Dock-/Chatpfade
an `127.0.0.1:8891` weiter. Die interne Verwaltungs-API ist dadurch nicht öffentlich.
Diese Proxyregel wird durch die Übernahme nicht verändert.

| Methode und interner Pfad | Inhalt bzw. Verhalten |
| --- | --- |
| `GET /v1/chat/ws` | Ereignisstrom; `arten` für `chat`, `activity`, `points`, `info`; `seit` als Wiederanlaufcursor und `gen` als Prozessgeneration |
| `POST /v1/chat/send` | Bisher `{text}` an alle aktiven Plattformadapter; je Plattform ein Ergebnis mit Erfolg oder Hinweis |
| `GET /v1/stream-info` | Unterstützte Plattforminformationen lesen |
| `PUT /v1/stream-info` | Unterstützte Informationen ändern |
| `GET /v1/stream-info/kategorien?q=...` | Kategorien suchen |
| `POST /v1/points/einloesungen/{id}` | Kanalpunkte-Einlösung erfüllen oder ablehnen |
| `POST /v1/me/dock-token/rotate?streamer_id=...` | Interner Dashboardweg zum Erneuern des Dock-Zugangs |

Zu erhalten und erneut zu prüfen sind Mandantentrennung, Origin-Prüfung,
begrenzte Sockets, Schreibrate, Entwertung offener Sockets, Wiederanlaufcursor,
Ereignisfilter, sichere Textdarstellung und sichtbare Fehler bei Schreibaktionen.
Die bisherigen Grenzen sind Implementierungsdetails des Bestands, keine
Freigabe neuer Produktgrenzen.

Die Persistenz alter Dock-Adressen hängt an `relay.users.dock_token_hash` und
`relay.users.dock_token_enc`; die dazugehörigen Migrationen heißen
`20260827000001_dock_token_je_nutzer.sql` und `20260827000002_dock_token_enc.sql`.
Eine spätere Migration muss bestehende Adressen und serverseitige Verschlüsselung
gezielt behandeln. Die alte Migrationskette wird hier nicht übernommen.

## Dashboard und Overlay bleiben im Twitch-Bot

Im rs-relay-Repository gibt es keine weitere versionierte React-Oberfläche und
kein eigenständiges Overlay-Asset. Die vorhandene Dashboard-Shell und der
Overlay-Builder leben im Repository `EarlySalty/Deadlock-Twitch-Bot`:

| Baustein | Vorhandener Ort |
| --- | --- |
| Uplink-Seite | `bot/dashboard_v2/src/pages/Uplink.tsx` |
| Zielkarten | `bot/dashboard_v2/src/pages/UplinkZiel.tsx` |
| Dashboard-API | `bot/dashboard_v2/src/api/uplink.ts` |
| OBS-Hilfe, Hinweise und Aufdecken | `bot/dashboard_v2/src/uplinkHelp.ts`, `uplinkHelp.css`, `uplinkDisclosure.ts` |
| Bisherige Profil-/Uploadempfehlungen | `bot/dashboard_v2/src/uplinkEmpfehlung.ts` |
| Plattformlogos | `bot/dashboard_v2/src/assets/platforms/` |
| Gemeinsame Navigation | `bot/dashboard_v2/src/components/layout/DashboardSidebar.tsx`, `src/App.tsx` |
| OBS-Anleitungsassets | `bot/dashboard_v2/public/uplink/` |
| Overlay-Builder | `bot/dashboard_v2/src/pages/OverlayBuilder.tsx`, `src/components/verwaltung/OverlayBuilderSection.tsx` |
| Öffentliches Stats-Overlay einschließlich eingebettetem HTML | `rust/crates/tb-dashboard-api/src/handlers/overlay.rs` |

Das Stats-Overlay verwendet `/twitch/overlay?streamer=...` und
`/twitch/api/v2/public/overlay`. Seine Daten kommen aus dem Twitch-Bot und dessen
Statistikquellen. Es ist kein Hochkanteditor, kein Multichat-Bildeinbau und keine
vorhandene Umsetzung der neuen Screenshot-Anforderung.

Das Dashboard ruft relative `/twitch/api/v2/uplink/...`-Routen auf. Deren Rust-Proxy
liegt in `rust/crates/tb-dashboard-api/src/handlers/uplink.rs` und spricht bisher
die Relay-API mit `X-Relay-Auth` an. Für den Neubau wird dieser vorhandene Proxy
gegen die neue Steuerungs-API angepasst. Es wird kein zweites React-Projekt und
keine parallele Login-Wahrheit angelegt.

Die bisherige Hilfe und die Typen enthalten SRT-Eingang, alte feste Profilannahmen,
Reconnect-Fristen und alte Bitratenempfehlungen. Sie müssen vor neuer Freigabe
gegen Produktvertrag v0.3 ersetzt bzw. geprüft werden. Eine unveränderte Übernahme
der gesamten Uplink-Seite wäre fachlich falsch.

## Konten und Token bleiben autoritativ im Twitch-Bot

Relevante bestehende Rust-Module unter
`rust/crates/tb-dashboard-api/src/handlers/` sind `plattform_connect.rs`,
`plattform_oauth.rs`, `platform_store.rs`, `platform_token.rs` und `uplink.rs`.
Twitch verwendet weiterhin `twitch_raid_auth`, andere Plattformen
`platform_connections`. Die zusätzliche Uplink-Autorisierung baut auf dem
bestehenden bewussten Verbinden-Weg auf.

Der Chat bezieht Zugänge serverseitig über
`/twitch/api/v2/internal/platform-token` mit `X-Internal-Token`. Weitere bestehende
Verträge liefern `/twitch/api/v2/internal/chatter-verlauf` und
`/twitch/api/v2/internal/stream-kennzahlen`. Der Neubau muss diese Verträge erhalten
oder mit dem Twitch-Bot gemeinsam migrieren. Keine Tokenkopie wird in diesem
Repository angelegt. YouTube-Liveberechtigung beweist keine VOD-Uploadberechtigung.

## Rust-Chat als geprüfter Übernahmekandidat

Die folgenden Quellen aus rs-relay sind zunächst Referenzen. Es wurde keine
unkompilierte Kopie davon in den Neubau gelegt:

| Bestand | Weiterverwendung und notwendige Trennung |
| --- | --- |
| `src/chat/nachricht.rs`, `ereignis.rs`, `twitch_activity.rs` | Normalisierte Nachrichten, Aktivitäten und Status; `Platform` aus neutralem Vertragsmodul beziehen |
| `src/chat/bus.rs` | Begrenzter Verlauf und Ereignisverteilung; Updates mit gleicher Fremd-ID gesondert modellieren |
| `src/chat/adapter.rs`, `twitch.rs`, `kick.rs`, `youtube.rs`, `kick_webhook.rs` | Adapter und Signaturprüfung; Konto-/Funktionsrechte erneut nachweisen, TikTok ist bisher Stub |
| `src/chat/token.rs`, `helix.rs`, `punkte.rs` | Bestehende Kontoquelle und API-Aufrufe; sichere Fehler und erneute Autorisierung prüfen |
| `src/chat/hervorhebung.rs`, `kennzahlen.rs` | Vorhandene Bot-Verträge und Darstellungen; ID-basierte Identität prüfen |
| `src/chat/supervisor.rs` | Adapterlebenszyklus neu an sessionneutrale Signale und den tatsächlichen Verbindungsbestand anschließen |
| `src/streaminfo/mod.rs`, `twitch.rs` | Stream-Info-Vertrag ohne Medienabhängigkeit übernehmen; Plattformfunktionen nicht über den belegten Umfang hinaus anbieten |
| `src/api/dock_wache.rs` | Socket-, Entwertungs- und Schreibschutz als isolierten Baustein prüfen |
| `src/api/chat.rs`, `kick.rs` | Verhalten und Regressionen als Referenz; Handler gegen neuen Dienstzustand schreiben |

Konkrete Kopplungen und offene Befunde:

- Fast alle Chat-/Stream-Info-Dateien importieren `Platform` aus
  `src/transcode/profile.rs`. Der neue gemeinsame Typ darf den alten
  Transcodingkern nicht als Abhängigkeit zurückholen.
- `chat/supervisor.rs` importiert `SessionSignal` aus `ingest/srt.rs`.
  Das aktuelle Signal enthält lediglich Start mit Streamer und Plattformen sowie
  Ende mit Streamer. Ein neutraler Sessionvertrag ersetzt diese SRT-Kopplung.
- `api/chat.rs` verwendet das alte `AppState`, `store`, `Config` und die bisherige
  Dock-Persistenz. `AppState` enthält SRT-Registry, Punktebudget und Kill-Liste.
  Deshalb ist das gesamte alte API-Modul kein zulässiger Kopierbaustein.
- `chat/mod.rs` verdrahtet alte `Config`/`Secrets` und testet alte Dienstdateien.
  Der neue Einstieg benötigt normale Konfiguration und serverseitige
  Secret-Beschaffung nach den geltenden Workspace-Regeln.
- Der bisherige Schreibvertrag sendet jede Nachricht automatisch an alle aktiven
  Adapter. v0.3 beschreibt gezielte Zustellung als vorgeschlagenen neuen Vertrag;
  die konkrete Bedienentscheidung muss vor Verdrahtung geklärt werden. Die alte
  Regel wird nicht unbemerkt übernommen.
- Der Supervisor dokumentiert eine Wiederanlauf-Lücke: Nach einer Panik werden
  vorhandene Sessions gestartet, während inzwischen beendete Sessions nicht
  vollständig abgeglichen werden. Ein korrekter Bestandsabgleich gehört in den
  Neubau, bevor dieser Teil als wiederverwendbar gilt.
- Bisherige Deduplizierung kann Ereignisse anhand derselben ID verwerfen.
  Wiederverwendete IDs für echte Updates erfordern einen eigenen Vertrag.
- Der Bestand startet Chat an Relay-Sessions. Kontoverbindung, Zielaktivierung und
  laufender Chat müssen im neuen Produkt getrennt und verständlich abgebildet sein.

## Ausgeschlossener Altbestand und Betriebsstand

Nicht übernommen wurden alter Ingest, Sessionkern, Encoder/Pusher, Twitch-EB-Code,
Punkte-/Last-Gate, Prozessüberwachung, Konfiguration, Migrationskette, Binaries,
Logs, Datenbankinhalte, Streams oder Git-Historie. Auch die alte `CLAUDE.md`
enthält nach v0.3 überholte SRT-/Codecannahmen und ist kein Neubauvertrag.

Bei der Bestandsaufnahme war rs-relay auf `main` sauber; lokale nicht gemergte
Branches und mehrere Worktrees bestanden weiterhin. Der Dienst
`rs-relay.service` lief als Nutzer-Unit, aus `~/.config/rs-relay`, mit dem Binary
`~/.local/bin/rs-relay` und der `streaming.slice`. Die öffentliche Dock-Strecke
hing an Caddy und dem lokalen Port 8891. Diese Aufnahme ist kein Medien-Live-Test.

Es wurden weder alter Dienst noch Proxy, Daten oder Repository geändert oder
gelöscht. Eine spätere Abschaltung/Löschung des alten Repositorys darf die
kontrollierte Migration vorhandener Nutzerzugänge und die gesicherte Herkunft
dieser übernommenen Assets nicht ersetzen.

Prüfung der Sicherung: Alle vier SHA-256-Hashes stimmen mit den kopierten Bytes
aus dem festgehaltenen Commit überein. Die vier Browser-Skripte wurden ohne
Ausführung syntaktisch geparst. `gitleaks detect --no-git --redact` meldete für
`web/docks` keine Treffer (Exit 0). Es fand kein Browser- oder Plattform-Live-Test
statt; der Scan ersetzt keine spätere Sicherheitsprüfung der aktiven Anbindung.
