# VOD-Automatik

Die Rust-Komponente übernimmt automatisch angelegte VOD-Aufträge für zwei Quellen: den komprimierten Uplink-Eingang und ein eigenes, eindeutig zugeordnetes Twitch-VOD. Sie verwendet die bestehende Kontoverwaltung über einen injizierten Broker. Im Modul gibt es weder OAuth-Callbacks noch einen zweiten Token-Speicher, ENV-Konfiguration oder einen Produktiv-Downloader mit fremden Repositorypfaden.

## Anschluss an den Dienst

`Store::enqueue_in_transaction(&tokio_postgres::Transaction, &LogicalSessionEnded)` gehört in dieselbe Transaktion wie der dauerhafte Abschluss von `relay.sessions`. Die ID ist die persistente `bigint`-Session-ID, die Streamer-ID eine positive Twitch-Plattform-ID. Die Tabellen prüfen die Zuordnung zusätzlich über `relay.sessions`. Wiederholte Abschlussereignisse erzeugen durch `UNIQUE(session_id)` keinen weiteren Auftrag. Ein einzelner TCP-Abbruch ist kein logisches Streamende.

Die additiven Migrationen liegen in `migrations/202609080001_vod.sql` und `202609080002_vod_consistency.sql`; `store::MIGRATION` exportiert beide in dieser Reihenfolge. Der Dienst muss sie ausdrücklich vor Aktivierung anwenden. `Database::query` und `Database::transaction` adaptieren dessen vorhandene begrenzte PostgreSQL-Verbindungen. Es wird keine eigene Datenbankkonfiguration geladen. `Settings` werden nach Authentifizierung und bestätigter YouTube-Kanalidentität gespeichert. Die aktuelle Konfiguration wird beim logischen Abschluss im Auftrag eingefroren.

`PlatformBroker::grant` liefert nur Access-Token, Ablaufzeit, bestätigte Plattform-ID und tatsächlich gewährte Scopes. `Cipher` adaptiert den vorhandenen Dienstschlüssel. Die Uploadsession wird ausschließlich verschlüsselt in `upload_session_enc` gespeichert; AAD lautet `uplink-vod:streamer:{id}:job:{job_id}:upload-session:v1`. Tokens und Uploadsession-URLs werden weder protokolliert noch an Unterprozesse übergeben. Statusantworten enthalten diese Werte nicht.

`Worker::new(store, storage, sources, youtube, cipher)` verarbeitet über `run_one().await` einen fälligen Auftrag. Der Dienst ruft dies in seiner beendbaren Hintergrundschleife auf; ein Worker führt höchstens einen Auftrag gleichzeitig aus. Mehrere Instanzen sind durch zeitlich begrenzte, erneuerte Datenbank-Leases und Besitzerprüfungen getrennt. `with_upload_budget(bytes_per_second)` setzt das eigene Uploadbudget; die technische Vorgabe beträgt 1 MiB/s. Downloadgeschwindigkeit, Objektgrößen, Aufnahmequeue und Werkzeugfristen werden separat konfiguriert. Diese Grenzen ersetzen keinen gemeinsamen Kapazitätsnachweis mit Live-Streams.

## Eingangsaufnahme

`Storage::new` wird pro Speicherwurzel einmal aufgerufen; alle lokalen Worker und Recorder erhalten Klone desselben Adapters. Eine Prozess-Dateisperre verhindert die mehrfache Vergabe desselben freien Bytebudgets. Eine zusätzliche Objektsperre verhindert konkurrierende Aufbereitung durch einen alten und einen neuen Lease-Besitzer. Verschiedene Nodes benötigen ausdrücklich zugeordneten Speicher; ein transparenter Mehrserver-Dateizugriff ist damit nicht zugesagt.

Der Tap sitzt vor der Medienvorprüfung und der Audio-Normalisierung. `Recorder::start` liefert `RunningRecording`; dessen `sink()` darf an der logischen Session gehalten werden. Vor dem Kopieren eines Events wird `sink.reserve(wire_len)` aufgerufen, danach erst `FlvTag::from_event` und `permit.submit(RecordingPacket)`. Originale Ingest-Permits dürfen nicht in der Aufnahme verbleiben. Das Bytebudget bleibt bis zum abgeschlossenen Schreiben belegt. Abgebrochene Kopien, volle Queues, offene Permits bei Abschluss und Schreibfehler verhindern eine vermeintlich vollständige Aufnahme.

`RecordingSpec` bindet Session, Nutzer, Live-/VOD-Rollen und Metadaten. `update_metadata` übernimmt gemessene Quelle und Layout-/Zeitinformationen mit einer 64-KiB-Grenze. Die Konfiguration muss Veränderungen zeitlich zuordnen; ein nie aufgenommener anderer Bildausschnitt wird dadurch nicht nachträglich verfügbar. Das Material wird in komprimierten FLV-Segmenten gespeichert; vorhandene Audio-Wire-IDs bleiben erhalten. Neue Generationen kennzeichnen eine Lücke. Der gegenwärtige Export hält lückenhafte Aufnahmen an und ersetzt fehlenden VOD-Ton nie durch den Live-Mix.

Am logischen Ende wird `finish().await` vor der Jobanlage abgeholt und das Manifest mit `register_object` gespeichert. Auch ein fehlgeschlagener Abschluss ist mit `complete=false` und seinem Fehler dauerhaft zu speichern; andernfalls würde der Job nur auf eine Quelle warten. Drop lässt das zuvor geschriebene Manifest unvollständig. Ein Recorder gehört zur logischen Session und wird bei einer kurzzeitigen Neuverbindung nicht durch ein zweites Objekt für dieselbe Session ersetzt.

Der Export liest ausdrücklich den gewählten VOD-Track, normalisiert nur dessen Transport-ID und verpackt Video/AAC mittels FFmpeg-Streamcopy in MP4. Er legt keine fehlenden Audiomischungen an und führt keinen zusätzlichen Video-Encode aus. Browser oder OBS müssen dafür nicht geöffnet bleiben.

Segmentlängen und beim Schreiben berechnete SHA-256-Prüfsummen werden vor der Aufbereitung geprüft. Das fertige Exportobjekt erhält ebenfalls eine Prüfsumme, die vor jedem Upload-Wiederanlauf erneut geprüft wird. Geänderte Codec-Header oder mehrere nicht eindeutig gewählte Videoquellen halten die Streamcopy-Aufbereitung sichtbar an. Ein Twitch-Download mit mehreren unbekannten Audiomischungen wird nicht durch Auswahl der ersten Spur freigegeben.

## Twitch-Quelle

`bind_twitch_stream` speichert die tatsächliche Broadcaster-/Stream-ID aus der autorisierten Twitch-Verbindung sowie den Nachweis des verwendeten VOD-Mixes. Der Worker sucht `Get Videos` nach genau `stream_id` und `user_id`. Ein neuerer Listeneintrag wird nicht ersatzweise verwendet. Widersprüchliche Stream-IDs innerhalb einer logischen Uplink-Session machen die einzelne VOD-Bindung dauerhaft mehrdeutig. Mehrere Twitch-Teil-VODs werden derzeit nicht ungeprüft zu einem scheinbar vollständigen Uplink-Stream erklärt.

Ein während der Session verlorener VOD-Mix bleibt dauerhaft als fehlend gespeichert. Ein späteres `vod_audio_confirmed=true` macht die davor fehlenden Audiodaten nicht rückwirkend verfügbar. Der Bindungsaufruf liefert bei gültigen Identitäten auch dann `Ok`, wenn er diese Sperre oder einen Stream-ID-Konflikt persistiert; die umgebende Diensttransaktion muss das Ergebnis committen.

Ein noch laufender zugeordneter Twitch-Stream, fehlende Zuordnung oder ein noch wachsendes VOD bleiben sichtbar offen. Die konfigurierte Stabilitätsfrist wird bei gleicher Beobachtung nicht bei jedem Poll neu gestartet. Vor und nach dem Download werden Kanal, Stream und Dauer geprüft; die gemessene Datei darf von der Twitch-Dauer höchstens drei Sekunden abweichen. Das ist eine technische Vollständigkeitsprüfung, kein visueller Qualitätsnachweis jedes Frames.

Der vorhandene öffentliche Downloadweg nutzt das bereits verfügbare `yt-dlp` als begrenztes Werkzeug. Es erhält ausschließlich eine validierte öffentliche Twitch-VOD-ID, ignoriert Nutzerkonfiguration und Cache und schreibt Medien über stdout an den Rust-Prozess. Keine Cookies und keine OAuth-Tokens in Argumenten oder Dateien. Private oder gesperrte VODs werden nicht umgangen. FFmpeg, FFprobe und yt-dlp kommen als explizite absolute Werkzeugpfade aus normaler Konfiguration. Der neue Worker übernimmt keinen alten 12-Stunden-Poll und keinen Google-Drive-Fallback.

## Upload, Wiederaufnahme und Löschung

Die YouTube-Kanalidentität wird tatsächlich geprüft. `youtube.upload`, `youtube` und `youtube.force-ssl` werden als Uploadscopes akzeptiert; Kanal-/Statusabfragen benötigen dazu passende Rechte. Eine Verbindung erzeugt keine automatische öffentliche Veröffentlichung. Ohne ausdrücklich autorisierte Veröffentlichungsregel ist das Ziel privat.

Auch eine ausdrücklich öffentliche Regel startet den Upload zunächst privat. Nach `processingStatus=succeeded` folgt der getrennte Zustand `publishing`. Die Freigabe verwendet die bestätigte Video-ID, prüft den aktuellen Kanal und Nachweis erneut und bewahrt andere veränderbare Statusfelder. `videos.update` benötigt `youtube` oder `youtube.force-ssl`; der reine Uploadscope reicht dafür nicht. Erst die bestätigte Sichtbarkeit setzt `publication_confirmed=true` und danach `ready`. Bei privatem Ziel ist kein Veröffentlichungsschritt erforderlich.

Vor dem Start wird die Absicht dauerhaft gespeichert. Bricht der Prozess ab, bevor die neue Sessionreferenz dauerhaft bekannt ist, wird der Abschluss als unklar blockiert; es wird kein unbelegter zweiter Upload gestartet. Bekannte Sessions werden nach Neustart zuerst mit `Content-Range: bytes */Gesamtgröße` abgeglichen. Bestätigter Fortschritt stammt ausschließlich aus dem Server-Range. Ein vollständig gesendeter Request ohne Video-ID ist kein Erfolg.

Erst nach bestätigter Video-ID, vollständiger Übertragung und `processingStatus=succeeded` erreicht ein Auftrag `ready`. Fehlgeschlagene, terminierte oder unbekannte Verarbeitung gibt keine Löschung frei. Die Löschfreigabe ist ein eigener dauerhafter Zustand mit eigener Lease. Andere noch auf das Objekt angewiesene Aufträge verhindern die Löschung. Bei Twitch wird ausschließlich die lokale Uplink-Kopie entfernt; es gibt keinen API-Aufruf zum Löschen des Original-VODs. Ein Fehler oder Abbruch zwischen Dateilöschung und DB-Abschluss kann wiederaufgenommen werden.

## Nachweise und Grenzen

```sh
/home/nathanael/.cargo/bin/cargo test -j2 -p uplink-vod --target-dir /eigener/build-pfad
/home/nathanael/.cargo/bin/cargo clippy -j2 -p uplink-vod --all-targets --target-dir /eigener/build-pfad -- -D warnings
/home/nathanael/.cargo/bin/cargo run -j2 -p uplink-vod --example recording_probe --target-dir /eigener/build-pfad -- --ffmpeg /pfad/ffmpeg8 --ffprobe /pfad/ffprobe8 --yt-dlp /pfad/yt-dlp
```

Die Datenbanktests starten ausschließlich eine eigene PostgreSQL-16-Instanz im temporären Verzeichnis. Sie prüfen echten Rollback, Mandantenbindung, wiederholten Abschluss, verlorene Uploadantwort mit danach bestätigten Bytes, Worker-Neustart und die Löschsperre vor Verarbeitung bzw. bei weiteren offenen Aufträgen. HTTP-Mocks senden keine Nachrichten oder Videos an echte Plattformen.

Die ausführbare Medienprobe verwendet vorhandene synthetische H.264-/AV1-Fixtures mit zwei AAC-Mischungen und einen privaten temporären Speicher. Für beide Codecs sind im MP4 exakt die 95 AAC-Pakete der gewählten sparse Wire-ID 12 erhalten. Der Twitch-Pfad läuft zusätzlich durch lokale Helix-/Downloadfixtures und die echten Rust-/FFprobe-/FFmpeg-Schritte. Der verwendete Downloader in dieser Teilprobe ist ausdrücklich eine lokale Fixture, kein Nachweis eines heutigen Twitch-Netzdownloads.

Die tatsächliche Dienstverdrahtung, autorisierte Produktionskonten, echter Twitch-Download, YouTube-Annahme/Auditgrenzen, reale Verarbeitung sowie Lastschutz gemeinsam mit Live-Streams gehören zum anschließenden Integrationsnachweis. Ein erfolgreicher lokaler MP4-Export allein gibt den Produktivwechsel nicht frei. Der zentrale Workspace-Gate und unabhängige Rust-/Sicherheits-/Datenbankreviews folgen auf den Integrationscommit.

Primärquellen geprüft: [YouTube fortsetzbare Uploads](https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol), [Videos insert und akzeptierte Scopes](https://developers.google.com/youtube/v3/docs/videos/insert), [Video-Verarbeitung](https://developers.google.com/youtube/v3/docs/videos), [Twitch Get Videos](https://dev.twitch.tv/docs/api/reference/#get-videos). Die vorhandene Archivreferenz wurde gezielt unter `tb-vod-archive` gelesen; dessen Login-Zuordnung, alter Polltakt und bisherige Löschentscheidung wurden nicht übernommen.

Die Metadatenfreigabe wurde zusätzlich gegen [Videos update](https://developers.google.com/youtube/v3/docs/videos/update) geprüft. Die Korrekturen aus dem ersten Review und der genaue Transaktionsvertrag stehen in [FIX-NACHWEIS.md](FIX-NACHWEIS.md).
