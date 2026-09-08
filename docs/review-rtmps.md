# Gemeinsamer Rust- und Sicherheitsreview: lokaler RTMPS-Eingang

Stand: 8. September 2026. Unabhängiger Review von `feat/rtmps-ingest` gegen
`main` auf `02567c2`. Geprüft wurden beide lokalen Scuffle-Forks,
`uplink-ingest`, die TLS-Testunterstützung und `rtmps_probe` gemeinsam.
Die bisherige Core-/CLI- und Offline-FLV-Implementierung wurde nicht erneut
als neuer Code bewertet. Die zentralen Quelldateien sind unten per SHA-256
dem nachgeprüften Stand zugeordnet.

**Ergebnis:** Im nachgeprüften lokalen Baustein sind keine offenen kritischen
oder hohen Befunde verblieben. Die Freigabe gilt für den beschriebenen
Loopback-Eingang mit expliziten Grenzen. Sie ist keine öffentliche
Ingest-, OBS-, Plattform- oder Produktionsfreigabe. Merge setzt weiterhin den
zentralen Gate, erfolgreiche CI und einen konfliktfreien Stand voraus.

## Behobener Reviewbefund

**Track- und Konfigurationsinvarianten bei Metadaten:**
`Handler::media` in [server.rs](../crates/uplink-ingest/src/server.rs) nahm
Metadaten ursprünglich ohne Trackregistrierung und ohne Codec-/Endzustandsprüfung
an. Zwei unabhängige Tests über echtes Loopback-TLS/RTMP belegten:

- Bei `max_tracks = 1` wurden AAC-Metadaten für die Wire-IDs 1 und 2 beide als
  Ereignis geliefert. Die Meldung `track_count` blieb dabei bei null.
- Nach einem H.264-SequenceHeader auf Video 0 erhielten AV1-Metadaten für Video 0
  dessen Headerrevision statt einer Codecabweichung.

Beide unabhängigen Regressionen schlugen vor dem Fix fehl und bestanden danach.
Der Fix registriert Metadaten vor dem ersten Header mit Revision 0 und zählt sie
zum Trackbudget. Auch Metadaten müssen zum Codec und Sequenzzustand passen.
Frames und SequenceEnd benötigen weiterhin einen wirklichen SequenceHeader;
nur ein neuer Header eröffnet eine neue Konfiguration. Vier dauerhaft
versionierte Transporttests decken Tracklimit, Codecwechsel, fehlenden Header,
beendete Sequenz und den gültigen Ablauf Metadaten → Header → Frame ab.

Ein zwischenzeitlicher Formatierungsfehler im gleichzeitig bearbeiteten
Probe-Beispiel wurde ebenfalls korrigiert; die Pflichtprüfungen wurden danach
erneut ausgeführt.

## Nachreview der Gate-Befunde auf `baf240c`

Die zwei anschließend vom zentralen Gate benannten Korrekturen wurden gemeinsam
mit ihren Regressionstests gezielt nachgeprüft:

- `finish()` hält den Producer-JoinHandle jetzt bis zum abgeschlossenen `await`
  im Besitzer. Beim Abbruch der Future erreicht dessen `Drop` den Handle und
  beendet den Producer. Der Transporttest verwendet einen stillen TCP-Peer und
  eine Startfrist von 60 Sekunden: Nach dem Abbruch muss innerhalb von 500 ms
  EOF eintreffen und eine Ersatzverbindung wieder einen Slot erhalten.
- Die Budgetbelegung wird vor `try_send` erfasst, solange die eigene Byte-Permit
  noch beim Producer liegt. Ein deterministischer Test blockiert dessen
  Reportaktualisierung, lässt den Consumer das Ereignis freigeben und prüft
  anschließend den erhaltenen Messwert von vier Bytes. Der Wert bleibt das
  Maximum beobachteter Messpunkte; gleichzeitige Freigaben älterer Ereignisse
  erlauben keine Zusage eines atomar erfassten historischen Höchststands.

Beide Tests waren laut Fixer vor der Korrektur rot. Im unabhängigen Nachreview
bestanden sie zusammen mit allen Ingest-/Beispieltests in Debug und Release;
Check, Clippy und Formatierung waren ebenfalls grün. Die übrigen unveränderten
Vendorpfade wurden dafür nicht erneut breit geprüft. Keine weiteren offenen
Befunde aus diesem Nachreview.

## Gemeinsam geprüfte Grenzen

- RTMP-Nachrichtenlänge, AMF-Nachrichtenlänge, Chunkstreamzahl und die Summe
  reservierter Teilnachrichten werden vor der betreffenden Payload-/Map-Allokation
  geprüft. Unzulässige Chunkgrößen werden abgewiesen. Abwechselnd übertragene Chunkstreams
  behalten eigene Payloads; ein veränderter Header ersetzt keine aktive
  Teilnachricht. Type-1/2/3-Zeitstempel verwenden definiertes Wrapping.
- Native AMF- und Serde-Wege teilen Byte-, String-, Eintrags-, Werte- und
  Tiefengrenzen. Ungeprüfte Arraylängen erreichen weder Reservierung noch
  Serde-`size_hint`. ECMA-Längen sind kein Allokationsauftrag; tatsächliche
  Einträge, Endmarker, vollständiger Verbrauch und Lesefortschritt werden geprüft.
  Optionale Argumente bekannter RTMP-Kommandos umgehen diese Prüfung nicht.
- EOF beendet den Handshake. Absolute Handshake-/Startfristen begrenzen auch
  langsame Eingaben und eine hängende Autorisierung. Write und tatsächliches
  TLS-Flush liegen innerhalb einer gemeinsamen Schreibfrist. ACK-Fenster null
  wird abgewiesen; ACK-Zähler bleiben beim Überlauf konsistent.
- TLS-Verarbeitung beginnt erst nach Slotreservierung. Neue Verbindungen erhalten
  frische serverseitige Generationen. Medien kommen erst nach erfolgreicher
  Publish-Autorisierung durch. Identität enthält Session, Generation, Medienart
  und Wire-ID; Audio 0 und Video 0 werden nicht gleichgesetzt.
- Ereignis- und Bytebudgets bleiben an bereits abgegebene, noch gehaltene
  Medienereignisse gebunden. Der Verbindungsslot bleibt bis zu deren Freigabe
  belegt. Kompakte Bodies halten keine größere Parser-Backingallocation fest.
  Volle Budgets und ein geschlossener Verbraucher werden sichtbar beendet.
- Äußere Fehler enthalten keine App-/Publishkennung oder ungefilterten
  AMF-/Serde-Inhalt. Testschlüssel entstehen ausschließlich im Prozess.
  Die Probe schreibt nur das öffentliche Testzertifikat, begrenzt Prozessausgaben
  und Laufzeit und sammelt den FFmpeg-Prozess auch auf Fehlerpfaden ein.
- Der Medienpfad liest kleine Transportköpfe mit Längenprüfung. Er ruft keinen
  SPS-/AVCC-/AV1-/AAC-Codecparser auf. Native AAC-Kanalmetadaten entsprechen dem
  begrenzten geprüften Format aus [Enhanced RTMP V2](https://veovera.org/docs/enhanced/enhanced-rtmp-v2).
  Kompositionszeiten von AVC bleiben vorzeichenrichtig. Revision 0 ist kein
  bestätigter Codec-Header und keine Live-/VOD-Audiorolle.

Die Freigabe umfasst die tatsächlich verwendeten ServerSession-/Ingest-Pfade.
Sie behauptet keine allgemeine Panic-Sicherheit aller öffentlichen
Upstream-Hilfsfunktionen. Eventbytes sind kein Gesamt-Heap-Nachweis;
`max_queued_bytes` ist eine beobachtete Budgetbelegung. Der Ingest rekonstruiert
den langfristigen DTS-Wrap noch nicht und weist sinkende DTS innerhalb einer
Konfiguration ab; diese Grenze ist ausdrücklich dokumentiert.

## Unabhängige Ausführung

`cargo check`, `cargo clippy -- -D warnings` und `cargo fmt --check` bestanden
für die beiden Forks sowie Ingest einschließlich Beispiel. Alle Aufrufe
verwendeten die Rustup-Toolchain und höchstens zwei Cargo-Baujobs je Aufruf.

| Testbereich | Debug | Release |
| --- | --- | --- |
| Scuffle RTMP | 78 Tests + 1 Doctest | 78 Tests + 1 Doctest |
| Scuffle AMF mit Serde | 61 Tests + 1 Doctest | 61 Tests + 1 Doctest |
| Uplink-Ingest inklusive TLS-Transport | 24 Tests | 24 Tests |
| FFmpeg-Probe, portable Referenztests | 3 Tests | 3 Tests |

Zwei geerbte RTMP-Medientests benötigen fehlende Upstream-Testdateien und sind
ausdrücklich ignoriert. Sie zählen nicht als bestanden. Zusätzlich wurden die
zwei unabhängig geschriebenen Metadata-Regressionsfälle vor und nach dem Fix
gegen die öffentliche Ingest-API ausgeführt; die dauerhaften Regressionen liegen
in [transport.rs](../crates/uplink-ingest/tests/transport.rs).

Die echte Probe wurde nach dem Metadata-Fix vom Reviewer nochmals vollständig
ausgeführt: FFmpeg `n8.1.2-50-g1a748fe2cd-20260831`, Prozessende **Exit 0**.
Je AV1/H.264 kamen 240 Pakete an: 50 Video-, 95 erste AAC- und 95 zweite
AAC-Pakete, jeweils drei SequenceHeader auf drei eindeutig zugeordneten Spuren.
Größen und SHA-256 stimmen mit den versionierten Referenzen überein; jedes
Paket behält DTS/PTS mit gemeinsamer Verschiebung **0 ms**. Beide Sessions enden
mit `ExplicitStop` und verschiedenen Generationen.
Dieser eigenständige FFmpeg-Lauf erfolgte vor den beiden Gate-Korrekturen;
Medienparser, TLS-Testaufbau und Probequelltext sind seitdem unverändert.
Der korrigierte Lebensdauer-/Metrikpfad wurde anschließend wie oben beschrieben
in beiden Buildprofilen nachgeprüft.

Die unabhängige Negativmessung über den DNS-Namen `localhost` bestätigte:
falsche CA sowie falscher SAN bei ausdrücklich vertrautem Zertifikat führen
jeweils zu FFmpeg **Exit 251**, `TlsRejected`, null Publish-Autorisierungen und
null Medienereignissen. Die vom Probeautor festgestellte Einschränkung des
konkreten FFmpeg/OpenSSL-Clients bei numerischen URL-Hosts bleibt davon getrennt:
`127.0.0.1` ist kein freigegebener Hostnamenprüfungsnachweis dieser Probe.

Wiederholung und Einschränkungen stehen im
[RTMPS-Nachweis](rtmps-nachweis.md); Patchherkunft und ursprüngliche
Parserregressionen in [Third-Party-Patches](third-party-patches.md).

## Eingefrorene zentrale Quelldateien

SHA-256 des unabhängig ausgeführten und nachgeprüften Stands:

| Datei | SHA-256 |
| --- | --- |
| `crates/uplink-ingest/src/server.rs` | `258ab908b77d9d42c0d38f04f732087559ca6cd6bfb702c6406d085aa3af410d` |
| `crates/uplink-ingest/src/media.rs` | `f6efa48adb63e4359f32ae82f5d8c8279d268d4637803945c09245f6cc956ac0` |
| `crates/uplink-ingest/examples/rtmps_probe.rs` | `00c05835548d818244868f1d43cca84bf3c0fd8fc7a277395267fa0bfd995740` |
| `crates/uplink-ingest/tests/support/tls.rs` | `3d93965beeec5befa0d84433ad8f96dab8ebfbc6ddc351d8b1f06f9b0963f2f3` |
