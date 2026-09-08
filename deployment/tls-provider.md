# TLS-Provider

`uplink-tls-provider` übernimmt die vorhandene Caddy-PEM-Kette und ihren privaten Schlüssel in genau zwei bestehende Infisical-Namen: `UPLINK_TLS_CERTIFICATE` und `UPLINK_TLS_PRIVATE_KEY`. Caddy bleibt für Ausstellung und Erneuerung verantwortlich. Das Programm stellt keine eigenen Zertifikate aus und verändert Caddy nicht.

Der Root-Oneshot liest ausschließlich normale TOML-Konfiguration sowie die dort ausdrücklich benannten Quelldateien. Cert und Key werden bei jedem Lauf neu geöffnet; atomar ersetzte Caddy-Dateien werden dadurch beim folgenden Lauf übernommen. Symlinks, andere Dateitypen, unerwartete Eigentümer, fremd beschreibbare Dateien und zu große Inhalte werden abgewiesen. Private Quelldateien brauchen Modus 0600 oder strenger. Paar, DNS-Name, Vertrauenskette und aktuelle Gültigkeit werden vor Netzwerkzugriffen mit Rustls/WebPKI geprüft. `keys_match()` muss ausdrücklich erfolgreich sein; unbekannte Schlüsselkonsistenz reicht nicht.

Die Schreibidentität bleibt die vorhandene `serviceToken`-Identität aus `Documents/Infisical/service_token.json`, direkt vom Rustprozess in den Speicher gelesen. Die Datei wird nicht kopiert oder durch Shell/Python ausgewertet. Projekt, Umgebung und Secretpfad sind normale Configwerte. Der bewährte Protokollvertrag aus `update_gpt_secret.py` wird verwendet: v3-PATCH, nur bei HTTP 404 anschließend POST, niemals nach 401/403. Das Programm kann nur die zwei festen TLS-Namen schreiben. Es hält keine zweite Tokenablage vor und verwendet keine Umgebungsvariablen.

Vor dem Schreiben liest es die zwei Zielwerte einzeln über die dokumentierte v4-Secretabfrage. Unveränderte Werte verursachen keinen Schreibzugriff. Nach Änderungen liest es beide zurück, vergleicht sie mit der Quelle und validiert das Paar erneut. Ein 2xx der Schreibanfrage allein wird nicht als Erfolg ausgegeben. Fehlerkörper können Secrets spiegeln und werden nie ausgegeben; Fehlermeldungen enthalten ausschließlich deutsche Fehlerkategorien und den HTTP-Status einer Abweisung.

Der Transport verwendet ausschließlich den geschützten Unixsocket der [Infisical-Bridge](infisical-bridge.md). Der Provider erwartet UID 0 als Socketbesitzer und lehnt ungeschützte Pfade ab, bevor Zugangsdaten versendet werden. Eine bloße Loopback-IP oder ein HTTPS-Proxy mit ungeschütztem Hostport dahinter ist keine Ersatzkonfiguration. `infisical_port` wird durch `infisical_socket` ersetzt; ein HTTP-Fallback existiert nicht.

Zwei Infisical-Schreibvorgänge sind keine atomare Transaktion. Ein Teilfehler beendet den Provider sichtbar mit Exit 1. Beim nächsten Lauf werden ausschließlich noch abweichende Werte aktualisiert. Der separat gebaute Dienstresolver muss ein zwischenzeitlich inkonsistentes Paar ablehnen und das vorherige noch gültige Paar behalten. Er veröffentlicht ein neues Paar ausschließlich atomar im eigenen RAM. Ohne diese getestete Leserregel darf der öffentliche Testeingang nicht freigegeben werden.

## Installation nach Review

Das Binärprogramm wird eigenständig unter `/opt/uplink/current/bin/uplink-tls-provider` bereitgestellt. Configvorlage vor Übernahme gegen aktuelle Datei-UIDs/Pfade prüfen; die dokumentierten Bestandsdateien gehören UID 996 und sind reguläre 0600-Dateien. Die normale Config gehört root und darf nicht für Gruppe/Andere schreibbar sein. Provider-Binary, Unit und Config dürfen keine vom Dienstnutzer beschreibbaren Vorfahren haben. Die Schreibidentität wird nicht in dieses Repository oder nach `/opt/uplink` kopiert.

Die Vorlagen `tls-provider.service` und `tls-provider.timer` sind eigenständige System-Units, keine User-Units. Der Timer wartet 60 Sekunden nach einem abgeschlossenen Lauf und startet denselben Oneshot nicht parallel. Root liest bestehende Dateien mit eng begrenzten Fähigkeiten; Änderungen an Caddy-Dateirechten oder breite ACL-Freigaben sind nicht nötig. Home-Verzeichnisse sind verborgen, ausschließlich die bestehende Identity-Datei wird am ursprünglichen Pfad schreibgeschützt eingebunden, ohne Kopie ihres Inhalts. Dieser Bindpfad muss zum normalen Configpfad passen. Der Prozess darf nur Loopback ansprechen; das Ziel ist die vorhandene lokale Infisical-Instanz. Die Quelle bleibt schreibgeschützt. Core-Dumps sind gesperrt.

Erster echter Sync und Aktivierung erfolgen nach Sicherheitsreview. Noch kein echter Secret-Schreibzugriff oder öffentlicher TLS-Nachweis wurde mit diesem Paket ausgeführt. Danach sind erfolgreicher Paarabgleich, Dienstimport, echter TLS-Handshake für den vorgesehenen Host und Zertifikatswechsel gemeinsam zu prüfen. Caddy bleibt dabei unverändert; der bisherige Relaydienst wird nicht ersetzt.

```sh
cargo test --locked -j2 -p uplink-tls-provider
cargo clippy --locked -j2 -p uplink-tls-provider --all-targets -- -D warnings
```

Quellen: vorhandener lokaler Schreibhelper `Documents/Infisical/update_gpt_secret.py` (ausschließlich Quellcode geprüft), [Infisical Einzelabruf](https://infisical.com/docs/api-reference/endpoints/secrets/read), [Rustls CertifiedKey](https://docs.rs/rustls/0.23.44/rustls/sign/struct.CertifiedKey.html). Keine Secretdatei wurde für die Entwicklung als Text ausgegeben.
