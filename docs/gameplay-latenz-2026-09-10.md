# Kampfclip: Echtzeitreserve und Moduswahl

Lokaler, nicht öffentlicher RTMPS-Nachweis auf dem Produktionshost. Quelle war
ein vorhandener 1080p60-Deadlock-Kampfclip, für den Vergleich auf 1440p60
hochskaliert und vorab als AV1 mit 10 Mbit/s Zielbitrate und AAC 160 kbit/s
codiert. Hochskalieren erzeugt keine zusätzlichen Bilddetails. Derselbe
40-Sekunden-Ausschnitt lief in beiden Versuchen zeitgesteuert durch den normalen
Coordinator und echten TLS-/RTMP-Publisher; Ziel war H.264 1080p60, 6 Mbit/s,
GOP 120, AAC unverändert. Der lokale Empfänger publizierte nirgendwohin.

## Befunde

- Der anfängliche Empfängerabbruch nach 120 Frames war ein Testfehler:
  `local_probe()` begrenzte RTMP-Nachrichten auf 65.536 Byte, der folgende
  Keyframe hatte 74.754 Byte (`PartialChunkTooLarge`). Danach verwendete der
  Empfänger die regulären `Config::ingest_limits()`.
- Mit zwei Encoderthreads wuchs der Rückstand auf ungefähr 2,8 Sekunden.
  Die zusätzliche 256-Event-Queue in `runtime.rs` erreichte nachweislich
  `Full`, Kapazität 0. Rund 3,1 MB Eingangsmedien waren dabei gehalten.
  `stop_consumer()` beendete TLS, der Quellclient meldete `UnexpectedEof`/`Io`.
  Dies war weder ein ACK-Fehler noch ein erneuter Parserabbruch.
- Eine Kontrolle mit 16 Tokio-Workerthreads wie beim Dienst und ohne eigene
  parallele Compiler reproduzierte den Rückstau ebenfalls. FFmpeg belegte
  etwa 208 % CPU. Die vorherigen Ergebnisse mit einer einzelnen Tokio-Thread
  und zusätzlicher Buildlast sind deshalb keine alleinige Grundlage des Fixes.
- Mit vier Encoderthreads, denselben Puffern und 16 Tokio-Threads kamen alle
  2.400 Videoframes sowie 1.876 byteidentische AAC-Pakete an. Beide RTMPS-Seiten
  endeten regulär; die Eingangssession meldete `ExplicitStop` nach 42,433 s.
  Der FFmpeg-Prozess erreichte in den Stichproben maximal 270 % CPU.
- Erster Frame: 1,815 s nach Verbindungsstart. Im Abschnitt 8–32 s lag der
  gemessene Serverversatz meist bei 150–365 ms. Im schweren Abschnitt 33–39 s
  stieg er vorübergehend bis 1,551 s und sank wieder. Über 8–39 s betrug der
  Mittelwert der Sekundenstichproben 397 ms. Der Start erreichte maximal
  1,896 s Zeitstempelversatz. Es gab keinen Queueüberlauf.
- Der Test hielt die Quelle nach dem letzten Paket absichtlich zwei Sekunden
  offen. Der letzte Frame wurde beim Abschluss ausgegeben (2,471 s Versatz).
  Diese Abschlussphase ist vom laufenden Medienversatz zu unterscheiden.

## Konsequenzen

`media.worker_threads` ist explizit konfigurierbar und auf 1–64 begrenzt.
Der kompatible Default bleibt zwei; der gemessene Betrieb verwendet vier.
Puffer wurden nicht vergrößert. Queueüberlast bleibt zusätzlich zum finalen
Coordinatorfehler als `input_backpressure` sichtbar und im Sessionprofil erhalten.
Dieser Einzelstreamtest ist keine Freigabe einer Enhanced-Leiter oder
gleichzeitiger Mehrmandantenlast.

Die Messung enthält lokale TLS-Peers und einen Debug-Testdienst, keinen
Twitch-Zuschauerplayer. Sie belegt Serververhalten und den relativen A/B-Effekt.
Eine Zuschauerverzögerung von zwei Sekunden wird daraus nicht behauptet.

Die Sekundendaten des erfolgreichen Laufs stehen in
`gameplay-latenz-2026-09-10.csv`. Die ursprünglichen Probeprotokolle und der
vorcodierte Clip liegen nur im temporären Arbeitsbereich des Hosts; weder
Streamkeys noch OAuth-Daten sind Teil dieser Artefakte.
