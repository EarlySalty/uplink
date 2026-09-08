# Betriebsvertrag für den Uplink-Wechsel

Stand: 8. September 2026. Read-only Aufnahme durch Betriebsagent. Keine Dienste, Daten, Git-Zweige oder Proxyregeln verändert. Keine Secrets, Prozessumgebungen, ENV-Dateien, privaten TLS-Schlüssel oder Credentialinhalte gelesen. Quellcode ist Funktionsreferenz, keine Freigabe zur Kopie des alten Medienkerns. Der vorhandene Graph wurde zuerst mit `graphify query` befragt; konkrete Angaben danach an den genannten Quellen überprüft.

## Tatsächlich gemessener Betrieb

| Bestandteil | Beobachtung |
|---|---|
| Alter Dienst | User-Unit `rs-relay.service`, active/running seit 7. September; PID 6078 bei Aufnahme |
| Altes Binary | `/home/nathanael/.local/bin/rs-relay`, über `/proc/6078/exe` ohne Argumente ermittelt |
| Arbeitsverzeichnis | `/home/nathanael/.config/rs-relay` |
| API | TCP `127.0.0.1:8891`; `GET /v1/health` liefert `{"ok":true}` |
| Alter Eingang | UDP `0.0.0.0:8899` für SRT; dieser Weg gehört ausdrücklich nicht ins neue Produkt |
| Interner Botbroker | `http://127.0.0.1:8769` aus freigegebenem, nicht geheimem Configfeld |
| Dockbasis | `https://deutsche-deadlock-community.de/uplink` |
| Relaydatenbank | PostgreSQL-Datenbank `deadlock`, Schema `relay` |
| Plattform-/Uploadkonten | Datenbank `twitch_analytics`, Schema `public` |
| Livebestand | 0 offene `relay.sessions`; unmittelbar vor dem Wechsel erneut abfragen |
| Nutzer | 3 Zeilen in `relay.users`, alle freigeschaltet |
| Ziele | Twitch, Kick und YouTube jeweils 1 aktiviertes Ziel; TikTok 1 gespeichertes, deaktiviertes Ziel. TikTok-RTMP-Adresse vorhanden: ja; verschlüsselter Key vorhanden: ja. Ausschließlich boolesch abgefragt |
| Plattformverbindungen | Kick 1, YouTube 2; zum Messzeitpunkt keine `needs_reauth`, Ablaufmetadaten jeweils in der Zukunft. Beide YouTube-Verbindungen tragen tatsächlich `https://www.googleapis.com/auth/youtube.force-ssl`. Kein API-Funktionstest daraus ableiten |
| Twitch-Grantmetadaten | 69 Zeilen in `twitch_raid_auth`, keine mit `user:read:chat` im gespeicherten Scope-Text. Der Speicher ist laut `tb-raid/src/token_store.rs:150` leerzeichengetrennter Text. Vor Chatfreigabe tatsächliche Rechte am autorisierten Broker prüfen |
| VOD-Einstellungen | 2 vorhandene, aktivierte Archivkonfigurationen |
| YouTube-Uploadkonten | 2 aktivierte Datensätze mit `youtube.upload`; bei beiden fehlt eine gespeicherte `platform_user_id`. Zielkanal vor neuen Aufträgen über die autorisierte Verbindung bestätigen |

Die Datenaufnahme erfolgte mit lokalen PostgreSQL-Socketabfragen als `postgres`, ausschließlich Aggregaten und Schemainformationen. Es wurden keine IDs, Nutzernamen, Tokens, Schlüssel, verschlüsselten Werte oder Verbindungszeichenketten aus der Datenbank ausgegeben. Ein gespeichertes oder nicht abgelaufenes Konto ist kein Nachweis eines laufenden Plattformstreams.

## Proxy und bestehende Oberfläche

In `/etc/caddy/Caddyfile:317` werden folgende öffentliche Pfade nach Entfernen von `/uplink` an `127.0.0.1:8891` weitergegeben:

- `/uplink/dock/*`
- `/uplink/v1/chat/*`
- `/uplink/v1/stream-info` und `/uplink/v1/stream-info/*`
- `/uplink/v1/points/*`
- `/uplink/v1/webhooks/kick`

`/v1/me`, `/v1/admin/*` und die übrige Steuerung werden dadurch nicht öffentlich gemacht. Der Bot vermittelt authentifizierte Dashboardanfragen. Caddy setzt `X-Forwarded-For` aus der Peeradresse und `X-Forwarded-Proto: https`. Die Dock-CSP lässt bestehende eingebettete Skripte und den eigenen Websocket zu; Referrer-Policy ist `no-referrer`. Der Accesslogger entfernt den Queryparameter `t`. Diese Eigenschaften müssen erhalten bleiben.

Die eigentliche Uplink-Seite lebt im Twitch-Bot: `bot/dashboard_v2/src/pages/Uplink.tsx`, Zielkarte in `UplinkZiel.tsx`. Sie zeigt heute `srt_hint` und SRT-/HEVC-Anleitungen. Ein neuer Daemon allein stellt diese Ansicht nicht auf RTMPS um. Die Oberfläche muss den neuen Serverpfad, OBS-Einstellungen und ehrliche Fähigkeiten anzeigen. Die vier in diesem Repository archivierten Docks sind noch keine aktivierte und geprüfte Oberfläche.

## Steuerung, Identität und HTTP-Verträge

Quelle: altes `src/api/mod.rs:117`, `src/api/user.rs`, `src/api/admin.rs`; Bot `rust/crates/tb-dashboard-api/src/handlers/uplink.rs` und `src/lib.rs:499`.

Die Relaysteuerung nutzt `X-Relay-Auth`, keinen Bearerheader. `/v1/admin/*` braucht ein eigenes Adminsecret ohne Rückfall auf das APIsecret. Bei fehlendem eingerichteten Secret ist der Dienstweg 503, bei falschem Zugang 401. Die API bleibt Loopback. Der Bot setzt `streamer_id` aus seiner authentifizierten Twitch-Identität; eine vom Browser beliebig übernommene ID wäre eine Rechteausweitung. Im bestehenden Bot gibt es noch einen Namensfallback in `partner_id`; der Neubau darf diesen nicht als Identitätsregel übernehmen.

| Relayroute | Methode und Nutzlast |
|---|---|
| `/v1/health` | GET, ohne Secret, `{ok:bool}`; Bereitschaft von Ingest/Broker/Medien gesondert darstellen |
| `/v1/me?streamer_id=` | GET, eigener Status und vorhandene Zugänge |
| `/v1/me/waitlist?streamer_id=` | POST, Warteliste |
| `/v1/me/key/rotate?streamer_id=` | POST, explizite Rotation; beim Wechsel nicht ungefragt ausführen |
| `/v1/me/reconnect-wait?streamer_id=` | PUT, `{reconnect_wait_s}`; bisher 0 bis 300 Sekunden. Neue Produktwerte nicht aus dem Altstandard ableiten |
| `/v1/me/destinations?streamer_id=` | GET |
| `/v1/me/destinations` | PUT, `{streamer_id,destinations:[{platform,rtmp_url?,stream_key?,enabled?,width?,height?,fps?,bitrate_kbps?}]}` |
| `/v1/me/destinations/{platform}?streamer_id=` | DELETE, tatsächliche Wirkung auf laufenden Ausgang melden |
| `/v1/me/schedule?streamer_id=` | GET; PUT nutzt `{streamer_id,entries:[{starts_at,ends_at}]}` |
| `/v1/me/metrics?streamer_id=&session=` | GET, Sessioneigentum prüfen, fremde Session 404 |
| `/v1/caps` | GET, bisher `{platforms:[{platform,recommended_width,recommended_height,recommended_fps,recommended_bitrate_kbps,force_cbr}],ingest:{max_width,max_height,max_fps,max_bitrate_kbps}}` |
| `/v1/me/dock-token/rotate?streamer_id=` | POST, alle vier URLs und Token atomar erneuern, alte Sockets entwerten |
| `/v1/admin/users` | POST, Freischaltung |
| `/v1/admin/users/{id}?confirm=true` | DELETE, vorher Sessions kontrolliert beenden |
| `/v1/admin/destinations` | PUT, administrative Zielverwaltung |
| `/v1/admin/overview` | GET, alte Felder `loadavg,max_points,used_points`; keine neue Kapazität daraus behaupten |
| `/v1/admin/settings` | PUT, alte Felder `max_points?,load_reject_threshold?` |
| `/v1/admin/sessions/{id}/kill?confirm=true` | POST, bestätigter Sessionstopp |
| `/v1/admin/waitlist` | GET; DELETE `/v1/admin/waitlist/{id}` |
| `/v1/admin/streamers/{streamer_id}/twitch_eb/probe` | GET, bisher experimentelle Probe; kein Produktionsnachweis |

`GET /v1/me` hat folgende Form; sämtliche Secretfelder werden nur dem jeweiligen autorisierten Aufrufer geliefert und weder geloggt noch im Prüfprotokoll ausgegeben:

```text
{enabled, waitlisted, ingest_key, srt_hint,
 session: null | {id, started_at, ingest_protocol, ingest_codec},
 public_visible, status_text, reconnect_wait_s, reconnect_wait_max_s,
 dock_url_vorhanden,
 dock_urls: null | {chat, activity, stream_info, points},
 chat: [{platform, eingerichtet, verbunden, hinweis?, fehlende_scopes?}]}
```

`srt_hint` beschreibt das alte Produkt. Es darf keine SRT-Adresse für den neuen Eingang behaupten. Ein neues RTMPS-Feld muss mit dem tatsächlichen Dashboardlesepfad zusammen eingeführt werden. Der Bot ergänzt `live_status` sowie Kontoverbindungsstände; diese Informationen dürfen im Relay nicht als zweite Wahrheit entstehen.

Zielantwort: `{destinations:[{platform,rtmp_url,enabled,requested:{width,height,fps,bitrate_kbps}}],live_quality?:{status,message}}`. Der Plattformstreamkey wird nicht zurückgegeben. Ein fehlender oder leerer Key beim Profilupdate lässt den bestehenden Key erhalten; Änderungen sind atomar zu prüfen und zu schreiben. Gespeicherter Wunsch und tatsächlich laufendes Profil müssen unterscheidbar bleiben. Historische Werte wie `applied_restart`, `too_busy`, `until_end` sind keine Freigabe, Ziele im Neubau still zu ignorieren.

Die Browserrouten unter `/twitch/api/v2/uplink/` umfassen `me`, `reconnect-wait`, `waitlist`, `admin/waitlist`, `admin/users`, `destinations`, `caps`, `connect/:platform/disconnect`, `connect/:platform/streamkey`, `dock-token/rotate`. OAuth startet für Twitch im vorhandenen Raidflow mit Uplink-Scopeprofil, für Kick unter `/twitch/uplink/connect/kick`, für YouTube unter `/twitch/uplink/connect/youtube`; Callbacks `/callback/kick` und `/callback/youtube`. Kein zweiter Login- oder Tokenstore.

## Persistente Zugänge und Datenspeicher

`relay.users`: `streamer_id`, `enabled`, `ingest_key_hash`, `ingest_key_enc`, `srt_passphrase_enc`, `reconnect_wait_s`, `dock_token_hash`, `dock_token_enc`. Ingestauth ist bisher eine direkte DB-Funktion, kein bestehender HTTP-Broker. Der veröffentlichte Key hat die Form `rsr_` plus 32 kleine Hexzeichen; gesucht wird mit dem SHA-256-Hash als Hextext. `enabled` muss vor Sessionstart geprüft werden. Die neue Sessiongeneration entsteht serverseitig und ist kein vertrauenswürdiger Publishparameter.

Docktokens haben die Form `dock_` plus 32 kleine Hexzeichen. Hashsuche und verschlüsselte Anzeige müssen zusammen erhalten bleiben; aus dem Hash allein lässt sich die gespeicherte OBS-Adresse nicht wiederherstellen. Sperren und Rotieren entwerten laufende Sockets.

`relay.destinations`: `streamer_id`, `platform`, `rtmp_url`, `stream_key_enc`, `enabled`, `width`, `height`, `fps`, `bitrate_kbps`. Aktivierte Ziele werden mit `WHERE streamer_id=$1 AND enabled=true` geladen. Decoder-/Encoderplanung muss neu erfolgen; alte Plattformlimits und Punkteformeln sind keine Architekturvorgabe.

Weitere Tabellen: `relay.platform_caps`, `schedule`, `sessions`, `session_metrics`, `settings`, `waitlist`, `_sqlx_migrations`. Es gibt eine partielle Eindeutigkeit für eine offene Session je Streamer. Migrationen sind additiv und dürfen die bestehende, eingefrorene Migrationsbuchhaltung nicht ersetzen. Altes Schema nicht beim Start löschen und nicht beide Dienste gleichzeitig Sessions desselben Nutzers verwalten lassen.

Bestehende Verschlüsselung aus `src/crypto.rs`: AES-256-GCM, neues Blobformat `0x02 || 12-Byte-Nonce || Ciphertext samt Tag`; AAD ist `ingest_key:{streamer_id}`, `dock_token:{streamer_id}` bzw. `destination:{streamer_id}:{platform}`. Altbestand unterstützt zusätzlich ungebundene `Nonce || Ciphertext`; vor einer Abschaltung dessen tatsächlichen Bestand prüfen. Ein neues Format darf die gespeicherten Zugänge nicht still unlesbar machen. Schlüsselmaterial stammt aus dem bestehenden Infisicalsecret und wird ausschließlich im Dienstspeicher verwendet.

## Botbroker, Chat und Docks

`GET http://127.0.0.1:8769/twitch/api/v2/internal/platform-token?streamer=<id>&platform=<name>` mit `X-Internal-Token`; zusätzlich Loopbackprüfung im Bot. Antwort `{access_token,expires_at,platform_user_id,platform_login,scopes[]}`, keine Refresh-Tokens. 404 bedeutet keine geeignete Verbindung, 409 erneute Anmeldung nötig, 401/403 interner Zugang abgelehnt, 503 fehlende Konfiguration. Andere Netzwerk-/Serverfehler sind keine Aussage „nicht verbunden“. Der Broker liefert keine Medienstreamkeys. Diese kommen aus den verschlüsselten Relayzielen, die der vorhandene OAuth-Verbindungsweg setzt.

Weitere vorhandene Brokerwege mit demselben Header: `/twitch/api/v2/internal/stream-kennzahlen?streamer=` und `/twitch/api/v2/internal/chatter-verlauf?streamer=&logins=`. Sie liefern Dockkennzahlen und Hervorhebungsgrundlagen. OAuth-/Refreshzuständigkeit bleibt im Bot. Die Betriebsbasis wurde aus normaler Config bestätigt; kein Brokeraufruf mit echten Secretwerten erfolgte in dieser Aufnahme.

| Dockroute | Vertrag |
|---|---|
| GET `/dock/chat`, `/dock/activity`, `/dock/stream-info`, `/dock/points` | `?t=<Docktoken>`, vor Ausgabe Token und Nutzersperre prüfen |
| GET `/v1/chat/ws` | `?t=&seit=<u64>&gen=<u64>&arten=chat,activity,points,info`; Auth vor Upgrade, Originprüfung, begrenzter Replay und neue Generation nach Neustart |
| POST `/v1/chat/send` | Header `X-Dock-Token`, `{text}`, Antwort `{ergebnisse:[{platform,ok,hinweis?}]}`; alter Dockauftrag sendet an alle verbundenen Plattformen. Produktvorgabe zur gezielten Auswahl mit Root klären, nicht eine leere Erfolgsantwort ausgeben |
| GET/PUT `/v1/stream-info` | `X-Dock-Token`; PUT `{title?,category_id?,tags?}`, Antwort `{ergebnisse:[Plattformergebnis]}` |
| GET `/v1/stream-info/kategorien?q=` | `X-Dock-Token`, Ergebnis je Plattform |
| POST `/v1/points/einloesungen/{id}` | `X-Dock-Token`, `{platform?,reward_id,status}`, Ergebnis `{platform,ok,hinweis?}` |
| POST `/v1/webhooks/kick` | Kick-RSA-Signaturprüfung; Header `Kick-Event-Message-Id`, `Kick-Event-Message-Timestamp`, `Kick-Event-Signature`, `Kick-Event-Type`; Rohbody in Signatur einbeziehen, Replaygrenzen erhalten |

Websocketstatus: `{typ:"status",generation,plattformen:[{platform,eingerichtet,verbunden,hinweis?,fehlende_scopes?}]}`. Ereignisse: `{id:u64,ereignis:{typ:"chat"|"activity"|"points"|"info",...}}`. Chatfelder sind `platform,channel_id,channel_login,message_id,sender_id,sender_login,sender_display,color?,badges[],fragments[],sent_at,is_action,reply_to?,eigene,hervorhebung?`. Fragmenttag `art` umfasst `text`, `emote`, `mention`, `cheermote`. Docks lesen dieses Format bereits; generische JSON-Platzhalter erfüllen es nicht.

Bestehende Adapter: Twitch, Kick-Webhook, YouTube-Polling; TikTok ist weiterhin kein technisch nachgewiesener kompletter Adapter. Ein gespeichertes TikTokziel beweist keine Chat-/Moderationsrechte. Browser-/OBS-Tests wurden hier nicht durchgeführt. Eine bestimmte OBS-Version ist deshalb kein Grund, Entwicklung anzuhalten; ein tatsächlicher OBS-Nachweis bleibt als Abnahme offen.

## Infisical und TLS-Bereitstellung

Bestehende Secret-NAMEN: `RS_RELAY_API_SECRET`, `RS_RELAY_ADMIN_SECRET`, `RS_RELAY_KEY_ENC`, `RS_RELAY_DATABASE_URL`, `RS_RELAY_BOT_INTERNAL_TOKEN`. Keine Werte wurden gelesen. Der bisherige Launcher `rs-relay/scripts/run_rs_relay_service.sh` verwendet systemd `LoadCredential=infisical-token`, normale Projekt-/Umgebungsparameter in `~/.config/deadlock-twitch-bot/infisical.conf` und `~/.local/bin/dl-infisical-env`. Er exportiert Secrets über ENV und ist daher nicht als neuer Startweg geeignet.

Korrektur der anfänglichen Kodierungsannahme: `RS_RELAY_KEY_ENC` enthält gemäß dem tatsächlich verwendeten Bestandsloader `rs-relay/src/config.rs:607–618` Standard-Base64 für genau 32 Schlüsselbytes, keine 64 Hexzeichen. ASCII-Leerraum und optionales Padding sind vorgesehen. Die frühere Hexannahme verursachte den gemessenen Bootstrapfehler nach erfolgreichem Infisical-Zugriff. Der neue Loader übernimmt den Base64-Vertrag ohne Hex-Fallback; ungültiges Alphabet, falsche Länge und nichtkanonische Restbits werden abgewiesen. Quellprüfung und synthetischer Regressionstest sind kein Nachweis erfolgreicher Entschlüsselung aller Bestandsdaten.

Der existierende Rustclient liegt in `Deadlock-Bots/rust/bin/dl-infisical-env/src/lib.rs`. Die reine Fetchfunktion nimmt `Config {project_id,environment,service_token,secret_path,timeout,retry_delay,max_attempts}` entgegen und liefert eine Map im RAM. HTTP-Vertrag: `GET http://127.0.0.1:8080/api/v4/secrets/` mit Query `projectId,environment,secretPath,viewSecretValue=true,includeImports=true,recursive=false`, Bearerauth des Servicecredentials. Antwort `secrets:[{secretKey,secretValue}]` sowie gleichartige importierte Secrets. `config_from_env` und `command_with_secrets` werden für den Neubau nicht übernommen. Zugriff gezielt begrenzen und Antwortgrößen/Timeouts/Fehlerredaktion prüfen.

systemd 255 ist vorhanden. Der geschützte Laufzeitordner `/run/user/1000/credentials/rs-relay.service` existiert (read/execute ausschließlich Besitzer), die darin bereits materialisierte `infisical-token`-Credential ist 0400 für `nathanael`. Root hat für den Neubau ausdrücklich entschieden: keine neue RAM-Credentialdatei materialisieren, stattdessen einen bereits autorisierten Zugang per vorgeöffnetem FD übernehmen und Infisical direkt im Rustspeicher abfragen. Ein Startprovider mit passender Identität kann die bestehende Credential öffnen, ohne Inhalt auszugeben oder zu kopieren. Dieses FD muss vor dem Stoppen des alten Dienstes geöffnet werden; dessen Credentialverzeichnis hängt an der alten Unit. Für unabhängige spätere Neustarts ist ein eigener autorisierter Bootstrapprovider notwendig. Der ursprüngliche Bootstrapquellpfad wurde nicht offengelegt. Eine reine FD-Schnittstelle braucht einen tatsächlich gebauten Startprovider und ist für sich noch kein Deploynachweis.

Caddy verwaltet ein vorhandenes Zertifikat für `deutsche-deadlock-community.de`, gültig 12. August bis 10. November 2026. Zertifikatsmetadaten wurden aus dem öffentlichen Zertifikat geprüft, private Schlüssel nie geöffnet. Speicher: `/var/lib/caddy/.local/share/caddy/certificates/acme-v02.api.letsencrypt.org-directory/deutsche-deadlock-community.de/`, Verzeichnis 0700, `.crt` und `.key` 0600, Besitzer `caddy:caddy`. Der neue Userdaemon kann die Schlüssel nicht einfach lesen. Sichere Bereitstellung muss vor dem öffentlichen Start gebaut und geprüft werden, beispielsweise eng berechtigter FD-/Credentialprovider oder bewusst passender Dienstkontext mit Erneuerung. Keine breiten Zugriffsrechte und keine Klartextkopie in das Repository. Ob ein passendes TLSsecret schon in Infisical liegt, ist unbekannt; es wurde nicht danach durch Secretwerte gesucht. Caddys vorhandener HTTP-Reverseproxy ist kein RTMPS-L4-Proxy.

## VOD: neue Nutzerentscheidung und vorhandene Integration

Root hat in dieser Runde weitergegeben: zwei wählbare Quellen, Uplink-Mitschnitt oder Twitch-VOD; automatischer YouTube-Upload nach Streamende; lokale Quelle erst nach sicher bestätigtem Upload und erfolgreicher Verarbeitung löschen, bei Fehler nicht löschen. Diese Vorgabe ersetzt ältere allgemeine Archivdefaults.

Vorhanden ist `Deadlock-Twitch-Bot/rust/crates/tb-vod-archive`. Öffentliche Rustschnittstellen: `VodArchiveWorker`, `VodArchiveConfig`; `TeilHochlader` mit `resumable_offset`, `start_resumable_upload`, `upload_chunk`, `video_status`; `HochladerQuelle::fuer(streamer_login)`. `StreamerZugang` verwendet den bestehenden `CredentialManager` und verwirft dessen globalen Kontofallback für VODs. Die Zuordnung ist bisher loginbasiert; neue Aufträge brauchen die bestätigte Plattform-ID und eine geprüfte YouTube-Verbindungsidentität.

Bisherige HTTP-Schnittstelle ist nur GET/PUT `/social-media/api/admin/settings/vod-archive`. Trotz Pfadname dürfen dort berechtigte Partner ihre eigene Einstellung ändern, serverseitig über `required_streamer_scope`, Social-Media-Freigabe und Partnerguard. Felder: `streamer_login`, `enabled`, `privacy`; Antwort zusätzlich `privacy_options`, `privacy_forced`. Es gibt noch keine allgemeine interne HTTP-Schnittstelle, die ein autorisiertes Uplink-Medienobjekt als Exportjob annimmt. Ein solcher Übergabevertrag muss gebaut werden, statt fremde Repo-Dateipfade vorauszusetzen.

Zustand liegt in `social_media_vod_archive`, `twitch_vod_archive_vods` und `twitch_vod_archive_parts`. Teile halten resumable Sessionreferenz, bestätigten Offset und YouTubevideo-ID. Die Sessionreferenz ist sensibel und darf nicht in normalen Statusausgaben erscheinen. Bestehende Archivzugänge bleiben in `social_media_platform_auth`; Uplink-Verbindungen liegen in `platform_connections`. Die beiden tatsächlich gespeicherten Uplink-Grants tragen `youtube.force-ssl`. Root hat anhand von S18 bestätigt, dass `videos.insert` neben `youtube.upload` auch `youtube` und `youtube.force-ssl` akzeptiert: Der vorhandene Plattformbroker kann somit für diese Verbindungen grundsätzlich bereits Uploadberechtigung liefern. Die frühere pauschale Aussage „Uplink-Verbindung kann nicht hochladen“ ist nicht zu übernehmen. Konkrete Kanalidentität, tatsächliche API-Annahme, Audit-/Projektgrenzen und Verarbeitung bleiben zu prüfen. Die getrennten zwei Archivkonten tragen `youtube.upload`, ihre tatsächliche Kanalidentität muss mangels gespeicherter Kanal-ID erneut an der autorisierten Verbindung bestätigt werden. Keine manuell eingegebene Channel-ID darf den Zielkanal ersetzen.

Twitchquelle: `tb-vod-archive/src/twitch.rs` lädt über einen begrenzten `yt-dlp`-Prozess von `https://www.twitch.tv/videos/{id}`; es werden in diesem Pfad keine Twitch-OAuthcredentials übergeben. Das ist der bestehende öffentliche Downloadweg, kein allgemeiner autorisierter Download für private oder eingeschränkte VODs. Eigene Kanalzuordnung, tatsächliche Zugänglichkeit und vollständiges Streamende müssen vor dem Download geprüft werden. Ein Twitch-VOD liefert seinen vorhandenen Plattformton; zusätzliche nie gespeicherte Mischungen kann es nicht herstellen.

Wesentliche Lücken des Altworkers zum neuen Auftrag:

- Er läuft bisher periodisch (Configdefault 12 Stunden), nicht durch einen verbindlichen Streamendejob.
- Er kennt keinen Uplink-Eingangsmitschnitt als auswählbare Quelle und keinen allgemeinen Medienobjektvertrag.
- Uploadabschluss setzt `uploaded`; die spätere Nachprüfung prüft `uploadStatus` auf `rejected`/`failed`. `TeilHochlader::video_status` fragt nur `part=status`, nicht erfolgreiches `processingDetails.processingStatus`.
- Aufräumen wählt `status='uploaded'` und Alter; es verlangt nicht positiv bestätigte Verarbeitung. Bei einem Nachprüfungsfehler kann es daher nicht als sichere neue Löschregel gelten.
- Eine andere Uploadermethode `get_video_status` liest schon `status,processingDetails`; sie ist nicht mit der sicheren Löschentscheidung des VODworkers verdrahtet.
- Betriebsconfig des Altworkers ist ENV-basiert. Der Neubau braucht normale Config und getrennte Live-/VOD-Ressourcenlimits.

Der Bestand liefert nutzbare Upload-/Refreshverträge, aber keine unverändert übernehmbare Freigabe für die neue Quellwahl und Löschregel. Es wurden keine VODs heruntergeladen, Uploads ausgelöst oder bestehende Archivjobs verändert.

## Wechsel und Rückkehr

1. Neuen Rustdaemon auf separatem Loopback-Steuerport und separatem geprüften RTMPS-Port betreiben; bestehende OAuth-/DBdaten erhalten. Keine öffentlich sichtbare Bereitschaft aus bloßem TCP-/HTTP-Start ableiten.
2. Gleiche Steuer-, Dock- und Sessionverträge einschließlich Negativfällen, TLS-Vertrauen, Zielschlüsselredaktion und Wiederanlauf prüfen. Dashboard-SRT-Hinweise im selben Release auf neue reale RTMPS-Einrichtung ändern.
3. Produktionsdaten nur über additive Migrationen erweitern. Zuordnung gespeicherter Ingest-/Dock-/Zielzugänge vor und nach Neustart ohne Ausgabe der Werte prüfen. Ingestschlüssel nicht durch den Wechsel automatisch rotieren.
4. Unmittelbar vor Wechsel offene Sessions und alte Prozessaktivität sicher zählen. Die Aufnahme mit null Sessions ist keine dauerhafte Freigabe.
5. Geprüftes neues Binary und normale Config in eine unabhängige, versionierte Laufzeitablage bringen. Alter Binary-/Unitstand als Rückkehrpunkt erhalten, ohne Secretdateien zu kopieren. Neuer Betrieb darf keine Pfade unter dem später zu löschenden rs-relay-Repository voraussetzen.
6. Kontrolliert alten Dienst stoppen, neuen Dienst auf kompatiblem APIport 8891 starten oder Proxy und Botbasis gemeinsam atomar auf den neuen Port wechseln. Nur einen Sessionbesitzer zulassen. Öffentlichen RTMPS-Port mit echtem TLSvertrauen prüfen. Port443 ist durch Caddy belegt; Port8899TCP ist außerdem bereits von einem anderen lokalen Dienst gebunden und nicht pauschal frei.
7. Dashboard, vorhandene Dock-URLs, Websocket-Reconnect, verbundenes Konto und die beauftragten Medienausgänge live prüfen. Anschließend Commit/Binaryversion, tatsächlich empfangene Tracks, Zielpublikation und Fehlerstatus festhalten. Einen erfolgreichen Socket nicht als vier laufende Plattformen ausgeben.
8. Bei nachgewiesenem Wechselproblem neuen Dienst kontrolliert stoppen, unveränderten alten API-/Proxy-/Unitstand wieder aktivieren und mit `systemctl --user restart rs-relay` plus Health-/Dock-/Sessionprüfung verifizieren. Alte SRT-Einstellungen sind nur der Rückkehrweg, keine Funktion des neuen Produkts. Additive Datenmigrationen müssen diesen Rückweg offenhalten.

Ein heutiger Wechsel braucht diese Prüfungen sowie die neu implementierten Plattform-/Audio-/VODfunktionen. Diese Betriebsaufnahme allein ist kein Cutover- oder Kapazitätsnachweis.
