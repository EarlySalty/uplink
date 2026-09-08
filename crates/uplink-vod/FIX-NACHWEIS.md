# VOD-Korrekturen auf a7d44cf

Die folgenden Nachweise verwenden synthetische Dateien, eigene temporäre PostgreSQL-Instanzen und lokale HTTP-Server. Es wurde kein zusätzlicher Nutzer-Testweg eingerichtet und kein Video produktiv hochgeladen. Der Anschluss bleibt der normale Dashboard- und Serviceweg.

| Befund | Änderung | Nachweis |
|---|---|---|
| Später entzogener Twitch-VOD-Mix blieb nach `prepare` wirksam | DB-Trigger speichern eine dauerhafte Jobsperre und entziehen die Lease. Alle nachfolgenden Schreibschritte und externen Wirkungen beachten dieselbe persistente Sessiongrenze. | Ursprünglicher Repro schlägt vor dem Fix mit `prepared` fehl und endet danach `blocked/missing_vod_audio`. Echte getrennte PostgreSQL-Verbindungen konkurrieren während Upload und Löschung. |
| Fehlende Mandantenindizes | Indizes für Jobs `(streamer_id,id DESC)`, Objekte und Bindungen `(streamer_id)`. | PostgreSQL-Katalogprüfung auf alle drei Indizes. |
| `ready` erlaubte unbekannte Gesamtgröße durch SQL-NULL | Eigene Constraint verlangt `total_bytes IS NOT NULL`; öffentliche Regeln brauchen zusätzlich bestätigte Veröffentlichung. | Direkter ungültiger SQL-Übergang wird abgewiesen; erneutes Anwenden der Migrationen getestet. |
| Twitch-Quelle traf vollständiges Inputobjekt | Eindeutigkeit gilt für `(session_id,source)`. Beide Quellen behalten eigene Objekte. | Bestehendes vollständiges Inputmanifest bleibt unverändert; Twitch erzeugt ein zweites Objekt und wartet korrekt auf Stabilität. |
| Fehlgeschlagene und veraltete Zwischendateien blockierten dauerhaft das Budget | Capture-Guard entfernt eigene fehlgeschlagene Teildateien; Wiederanlauf räumt exakt erkennbare frühere Teildateien auf. Veraltete Download-/Exportzwischenstände werden ersetzt. | Acht aufeinanderfolgende fehlgeschlagene Prozesse und echter Prozessabbruch geben Budget frei. Wiederanlauf erhält die Eingangssegmente. |
| Verlorener Abschlussbody blockierte bekannte Uploadsession dauerhaft | Transportfehler bleiben wiederholbar. Bekannte Uploadsessions werden abgeglichen; ein unbekannter Start bleibt gesperrt. | Tatsächlich abgeschnittener HTTP-Body: vorher `Ambiguous`, danach `Network`. DB-Test trennt unbekannten Start von Wiederaufnahme derselben geschützten Session. Bestehender Worker-Neustarttest prüft akzeptierte Bytes und spätere Verarbeitung. |

## Transaktionsvertrag für den Service

Zusätzlich zu `Database::query` wird benötigt:

```rust
async fn transaction(&self, task: &mut (dyn TransactionTask + Send)) -> Result<()>;
// TransactionTask, mit async_trait:
async fn run(&mut self, tx: &tokio_postgres::Transaction<'_>) -> Result<()>;
```

Der Adapter leiht eine vorhandene Verbindung, beginnt die Transaktion, führt `task.run(&tx).await` aus und committet ausschließlich bei `Ok`. Fehler und Cancellation müssen die Transaktion zurückrollen. Keine neue Verbindungskonfiguration, Tokenkopie oder ENV-Konfiguration. Der Aufrufer darf keine weitere Verbindung aus demselben Pool innerhalb dieser Callback-Funktion anfordern; alle benötigten SQL-Schritte verwenden die übergebene Transaktion.

Der Bibliotheksaufruf begrenzt das Warten bis zum Callback-Eintritt auf fünf Sekunden und anschließend die Transaktion auf 55 Sekunden. SQL setzt `lock_timeout=5s` und `statement_timeout=10s`; ein externer Schritt hat höchstens 45 Sekunden. Ein Schritt umfasst einen begrenzten Uploadblock oder die Metadatenfreigabe, niemals den gesamten stundenlangen Upload. Die lange Medienaufbereitung hält keine Transaktion. Der Heartbeat bleibt gleichzeitig pollbar; ein vorübergehend belegter Pool blockiert die Medienfuture nicht.

Sperrreihenfolge: persistente Sessionzeile, danach Job-/Objektschreibschritt. Bindungsänderungen nehmen dieselbe Sessionzeile über einen Datenbanktrigger. Die Aufräumtransaktion sperrt alle abhängigen Sessions nach aufsteigender ID und anschließend das Objekt. Neue Jobs dürfen ein Objekt im Zustand `deleting` nicht übernehmen. Ein später ungültiger Mix bzw. Streamkonflikt bleibt gesperrt und lässt sich weder per Retry noch durch einen direkten Statuswechsel zurücksetzen.

Der Konkurrenztest für Upload hält eine echte HTTP-Antwort an, startet den Bindungsschreiber auf einer zweiten PostgreSQL-Verbindung und prüft, dass dieser bis zum Ende der freigegebenen Wirkung wartet. Nach seinem Commit ist der Job gesperrt und die lokale Quelle erhalten. Der Löschtest hält nach dauerhafter Kandidatenwahl an, startet einen konkurrierenden Bindungsentscheid und prüft beide Ausgänge: Commit der Sperre erhält die Quelle; Rollback erlaubt den geprüften Löschabschluss. Geprüft werden die Dateizustände und dauerhaften Jobzustände.

Uploads starten immer privat. Der gesonderte `publishing`-Schritt prüft unter derselben Sperre die aktuelle Freigabe; `JobStatus` enthält zusätzlich `publication_confirmed`. Ein bereits zuvor bestätigter externer Veröffentlichungsschritt wird durch eine erst später gespeicherte Sperre nicht rückwirkend aufgehoben. Eine solche spätere Sperre verhindert weitere Verarbeitung und Löschung der lokalen Quelle; eine automatische rückwirkende Plattformlöschung ist nicht Bestandteil dieses Auftrags.

`Store::register_object` bleibt der öffentliche Input-Aufnahmevertrag. Der Twitch-Adapter verwendet intern einen quellenbezogenen Objektvertrag. Ursprüngliche Inputsegmente werden beim Quellenwechsel und bei der Bereinigung fehlgeschlagener Zwischenstände erhalten. Die Löschregel für Originalquellen bleibt unverändert: bestätigte Übertragung, erfolgreiche Verarbeitung, gegebenenfalls bestätigte Veröffentlichung und keine abhängigen offenen Jobs.

## Ausführung

```sh
cargo test --locked -j2 -p uplink-vod --target-dir /eigener/build-pfad
cargo clippy --locked -j2 -p uplink-vod --all-targets --target-dir /eigener/build-pfad -- -D warnings
cargo run --locked -j2 -p uplink-vod --example recording_probe --target-dir /eigener/build-pfad -- --ffmpeg /pfad/ffmpeg8 --ffprobe /pfad/ffprobe8 --yt-dlp /pfad/yt-dlp
```

Die aktuelle Gesamtabnahme, der gekoppelte unabhängige Nachreview und der Autoren-Gate beziehen sich erst auf den anschließend durch Root festgehaltenen Commit. Die lokalen Nachweise ersetzen diese Schritte nicht.

## Zweiter Datenbank-Nachreview auf 560b2f6

Drei weitere Befunde wurden mit vier Regressionen vor der Korrektur reproduziert: abgelaufene Workerzuständigkeit trotz gescheiterter Heartbeats, scheiternde Migration alter öffentlicher/freigegebener Uploads sowie unbekannte Sichtbarkeit sowohl im Constraint als auch beim echten Löschen einer künstlichen Datei. Alle vier Tests waren zuerst rot; der Leasefall verwendete die tatsächlichen 120 Sekunden in PostgreSQL und eine zweite Verbindung zur anschließenden Übernahme.

Die lokale Zuständigkeitsfrist verwendet eine monotone Uhr und beginnt bereits vor dem Datenbankaufruf. Erst eine bestätigte Lease-Erneuerung einschließlich Transaktionsabschluss verlängert sie. Anfrage-/Poolwartezeit wird nicht nachträglich als zusätzliche Laufzeit angerechnet. Auch ein hängender Heartbeat bleibt durch die bisherige Frist begrenzt. Bei deren Ablauf wird die laufende Vorbereitung vor weiterer Datenbankarbeit gedroppt; damit werden die Quellsperre und kooperativ abbrechbare Unterprozesse freigegeben. Ein unbestätigter Heartbeat führt zu konservativem Abbruch, selbst wenn eine verlorene Antwort eine serverseitige Erneuerung verdeckt. Die Datenbank prüft die Erneuerung gegen `clock_timestamp()`, unabhängig vom Beginn einer möglicherweise wartenden Transaktion.

Migration 002 behandelt vormals gültige `ready`-Zeilen vor der verschärften Constraint: public/unlisted mit vollständigen Voraussetzungen und autorisierter Regel gehen ohne erfundene Veröffentlichungsbestätigung zurück nach `processing`. Unbekannte Gesamtgröße oder Sichtbarkeit bzw. fehlende erforderliche Freigabe werden `blocked`. Die vorhandenen Einstellungen und unbekannten Größen bleiben erhalten. Ein gültiger privater, vollständig bestätigter Auftrag bleibt `ready`. Wiederholtes Anwenden ändert diese Zustände nicht erneut. Früher bereits gelöschte Quelldateien kann eine Schemareparatur nicht wiederherstellen.

Die Fertig-Constraint sowie beide Löschprüfungen verlangen einen ausdrücklich wahren Sichtbarkeitsnachweis; fehlende JSON-Felder und JSON-null gelten nicht als Zustimmung. Nichtprivate Regeln brauchen sowohl `publication_authorized=true` als auch `publication_confirmed=true`. Der zusätzliche Laufzeittest entfernt ausschließlich in seiner isolierten Datenbank die Constraint, setzt eine alte ungültige Zeile ein und belegt, dass auch dann die echte Quelldatei erhalten bleibt. Es wurde keine produktive Migration oder Löschung ausgeführt.
