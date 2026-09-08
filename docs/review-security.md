# Sicherheitsreview des neuen Vertragskerns

Stand: 8. September 2026. Geprüfter Commit:
`0c9bc4e5ae4064f5b4cbeb16866f5e223e3877c6` gegen den leeren Initialstand
`3111cf3fc24c1ac1c3989cb84ae70af77c8d293b`.

Ergebnis: Im vorhandenen lokalen Szenarioplaner und Vertragskern wurde kein
konkret ausnutzbarer Sicherheitsfehler festgestellt, der den Merge dieses
Grundstands blockiert. Das ist keine Freigabe für einen öffentlichen Dienst,
Medien-Ingest oder die aktive Auslieferung der gesicherten Docks.

## Geprüfter Umfang

- `uplink-cli`: Der einzige Produkteingang ist eine ausdrücklich angegebene
  lokale TOML-Datei. `main.rs:26` prüft Dateityp und Größe und begrenzt den
  tatsächlichen Lesevorgang zusätzlich auf 256 KiB plus ein Prüfbyte.
  `config.rs:86` unterdrückt Parsertexte mit Eingabewerten; unbekannte Felder
  und Referenzen scheitern mit neutralen Meldungen. Ausgegebene Ausgangs-IDs
  werden vor der Ausgabe auf maximal 64 ASCII-Zeichen aus einer festen
  Zeichenmenge geprüft (`model.rs:287`, `planner.rs:113`). Kein Shellaufruf,
  Netzwerkzugriff, Schreibzugriff oder Secret-Lesen im Produktpfad.
- `uplink-core`: `FrameRate` und `SessionScope` erzwingen ihre Invarianten
  auch bei Deserialisierung über `try_from`; ihre Felder sind privat.
  Der Planer prüft eindeutige Track-IDs und Audio-Rollen. Der private
  Encode-Schlüssel enthält Nutzer, Session, Generation und Quellspur.
  Paketpuffer prüfen dieselbe Sessionidentität und ihre einzelne Spur vor
  Zustandsänderungen. Byte- und Paketgrenzen sowie monotone Zeitwerte werden
  kontrolliert; Byteadditionen erfolgen erst nach einer Differenzprüfung.
- Secrets und Protokollierung: Kein Secret-Speicher und keine
  Umgebungsvariablen für Produktkonfiguration. `std::env::args_os` liest
  Kommandozeilenargumente; `env!("CARGO_BIN_EXE_uplink")` gehört ausschließlich
  zum Cargo-Integrationstest. Diese beiden Vorkommen sind kein ENV-Configweg.
  Es gibt keine Medienpaket- oder Konfigurations-Debugausgabe im Produktpfad.
- Übernommene Oberfläche: Vier statische Quellassets ohne Einbindung in einen
  Server. Die gesichteten dynamischen Textpfade verwenden `textContent` bzw.
  Textknoten; `innerHTML` dient festen SVG-Pfaden. Bildadressen werden auf den
  vorgesehenen Twitch-CDN-Ursprung begrenzt. Das ist eine Quellcodeprüfung,
  kein vollständiger Browser- oder Berechtigungsnachweis. Der bisherige
  Dock-Zugang im WebSocket-Query ist im Übernahmedokument ausdrücklich
  beschrieben, einschließlich notwendiger Logfilterung vor Aktivierung.
- Abhängigkeiten und CI: Kleiner Cargo-Workspace mit versioniertem Lockfile,
  festgelegter Toolchain und verbotenen eigenen `unsafe`-Blöcken. CI hat
  ausschließlich `contents: read`; kein Deployment, keine Secret-Einbindung,
  kein `pull_request_target` und keine Shellinterpolation aus PR-Metadaten.
  Ein npm-Projekt bzw. aktive npm-Abhängigkeiten sind nicht vorhanden.

## Ausgeführte Prüfungen

| Prüfung | Ergebnis |
| --- | --- |
| `gitleaks git /home/nathanael/repos/uplink --redact --no-banner` | Exit 0; ein befüllter Commit, 282114 Bytes, keine Treffer |
| `cargo audit --file Cargo.lock` | Exit 0; frisch aktualisierte RustSec-Datenbank mit 1242 Meldungen, 18 Cargo-Abhängigkeiten geprüft |
| `git diff --check` | Exit 0 |
| Gezielte Quellcodeprüfung von Produktpfaden, Tests, CI und Übernahmedokument | Keine aktuellen Sicherheitsblocker im beschriebenen Umfang |

## Grenzen der Aussage

Der Planer verarbeitet lokale, vom Betreiber deklarierte Szenarien. Seine
Limits und Fähigkeiten sind keine vertrauenswürdigen Angaben eines zukünftigen
externen Clients. Der Dateigrößenschutz ist keine allgemeine Prozessspeicherquote;
die Konfigurationsauflösung klont Profile vor der semantischen Planprüfung.
Eine spätere entfernte Konfigurationsschnittstelle muss ihre eigenen
Autorisierungs- und Ressourcenobergrenzen vor dieser Verarbeitung erzwingen.

`Packet` enthält bereits allokierte Nutzdaten. Ein künftiger Ingest muss deshalb
die Paketgröße vor dieser Allokation begrenzen und Identitäten aus der
authentifizierten Session liefern. Die FIFO übernimmt keine Authentifizierung.
Ihr zeitlicher Ablauf benötigt bei Inaktivität den dokumentierten `expire`-Aufruf.

Diese Integrationsgrenzen sind noch nicht vorhandene Produktpfade und werden
hier nicht als aktuelle Schwachstellen eines öffentlichen Dienstes gewertet.
Auth, TLS, Medienparser, Plattformrechte, Datenmigration und aktive Docks brauchen
bei ihrer Implementierung ein eigenes Review. Die fachlichen Planerbefunde und
Vertragstests werden zusätzlich im separaten Rust-Review behandelt.
