# Rust-Dienst

`uplink-service --config <Datei>` lädt normale TOML-Konfiguration, bezieht vorhandene Dienstzugänge aus Infisical über einen injizierten Credential-FD und öffnet danach TLS-Ingest und die ausschließlich lokale Steuerung. `--check-config` validiert nur die Datei und öffnet keine Zugänge oder Ports.

Implementiert sind der bestehende SHA256-/AES-GCM-Datenvertrag, begrenzte Nutzerreservierungen, statische Fehlermeldungen, vorhandene Ingest-/Dockzugänge in `GET /v1/me`, Zielabfrage und atomare Profil-/Keyänderungen, Wartelisteneintrag sowie Keyrotation. `X-Relay-Auth` bleibt erforderlich; Zielzugänge erscheinen nicht in Fehlern oder Statusdaten. Ein ausgelassenes oder leeres Streamkeyfeld verändert bei einem Profilupdate den bestehenden Schlüssel nicht. Profiländerungen gelten sichtbar für den nächsten Stream.

Der Medienadapter lädt aktivierte Ziele aus `relay.destinations` und kombiniert gespeicherte Wünsche mit expliziter Host-, Codec- und Audiozuordnung aus TOML. Er ruft den neuen gemessenen Medienvorlauf auf und liefert `source_observation` sowie die tatsächlichen Worker-Ausgangszustände über `/v1/me/status` und `uplink_sessions` zurück. Fehlende Wünsche und nicht freigegebene Profile werden sichtbar abgewiesen. Die gemessene Runtime unterstützt derzeit die im Mediennachweis genannten SDR-H.264-Ausgaben; das ist kein vollständiger Plattformnachweis. Hostfreigaben sind exakte DNS-Namen, keine Domain-Wildcards. Beispielnamen müssen anhand des autorisierten Plattformendpunkts geprüft werden.

Nutzerreservierungen bleiben über Quellenende hinaus an noch konsumierte Medien gebunden. Neue vollständig beendete Verbindungen bekommen frische Generationen. Das Wiederanknüpfen an eine weiterlaufende logische Session, Wartebild, aktiver Delay sowie persistente Session-/Metrikbuchhaltung sind noch nicht integriert; die gegenwärtigen Status-IDs gelten nur innerhalb des Dienstprozesses. Chat-, Dock- und weitere Adminrouten werden anschließend über eigene injizierte Module in denselben Router eingebunden. Der bisherige Dienst wird dafür nicht als Ersatzbackend weitergereicht.

Lokale Prüfungen:

```sh
cargo test -p uplink-service -j2
cargo test -p uplink-service --test database -j2 -- --ignored
cargo run -p uplink-service -j2 -- --config config/uplink-beispiel.toml --check-config
```

Der zweite Befehl benötigt PostgreSQL 16 unter `/usr/lib/postgresql/16/bin`, `ffmpeg`/`ffprobe` unter `/usr/bin` und Linux. Er erzeugt ausschließlich eine private künstliche Datenbank ohne TCP-Bindung und löscht sie nach dem Lauf. Er prüft echte SQL-/HTTP-Verträge, zwei authentifizierte TLS-/RTMP-Eingänge mit frischen Generationen sowie das tatsächlich gestartete Dienstbinary mit künstlichem Infisical-Provider, anonymem RAM-FD, TLS-Material nur im Prozess und regulärem SIGTERM-Ende. Der Binary-Smoke startet keine Encoder und behauptet daher keine FFmpeg-Versionseignung. Der eigene Mediennachweis verwendet separat den geprüften FFmpeg-8-Build. Der explizite Datenbanktest wird in CI gesondert ausgeführt; ein im Standardlauf ignorierter Test gilt nicht als bestanden.

Die Produktionsbereitstellung, Infisical-TLS-Erneuerung, öffentliche Plattformverbindung und der vollständige Produktwechsel bleiben gesonderte Abnahmen. Vorlagen liegen unter `deployment/`; sie sind noch nicht installiert.
