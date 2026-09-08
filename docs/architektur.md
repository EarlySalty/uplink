# Architektur und nächste Arbeit

Stand: 8. September 2026. Der [Produktvertrag v0.3](produktvertrag-v0.3.md) beschreibt den vollständigen Umfang; [ADR 0001](adr/0001-neues-repository-und-rust-neubau.md) hält die neuere Repoentscheidung fest. Die [Abnahmematrix](abnahme.md) trennt Entwicklung von nachgewiesener Produktfunktion.

## Tatsächlich vorhandene Grundlage

Der Repo-/Rust-Grundlagenbaustein und der isolierte Scuffle-Nachweis sind über [PR #1](https://github.com/EarlySalty/uplink/pull/1) und [PR #2](https://github.com/EarlySalty/uplink/pull/2) gemergt. Der geprüfte funktionale Main-Stand nach PR #2 ist `001724fdacded263f16681ba0aec8e5f348de601`; der Dateibaum entspricht dem geprüften Stand `32653ca0a74d3c6a886d0f1a9b700268953153d2`. Die [Main-CI](https://github.com/EarlySalty/uplink/actions/runs/34221066802) ist erfolgreich. Die Branches aus PR #1/#2 wurden lokal und auf GitHub gelöscht; anschließend begann der RTMPS-Baustein auf `feat/rtmps-ingest`. Beide Bausteine sind abgeschlossen, der vollständige Live-Neubau bleibt offen.

`uplink-core` enthält lokale Fachregeln für variable Profile mit rational normalisierten Bildraten, getrennte Audiozuordnung, Mandanten-/Session-/Generationstrennung und deklarative Encode-Gruppen. Workerfähigkeiten werden für Decoder, Encoder und Kompositionslayout getrennt geprüft. Bekannte Audioverbote und fehlende Layoutrevisionen lehnen ein Ziel auch dann ab, wenn der Eingang noch aussteht. `shared_video_decode_count` zählt ausschließlich benötigte Video-Decoder; Audioverarbeitung ist darin nicht enthalten. Das Ergebnis ist ein Szenarioplan, kein ausführbarer Mediengraph; konkrete Codec-/Transform-Kompatibilität bleibt nachzuweisen.

Der komprimierte FIFO-Puffer prüft Scope/Track, Byte-, Anzahl- und Ankunftsaltergrenzen sowie monotone DTS. Er ist **kein** fertiger GOP-Cache, Delay-Regler oder Wartebildpfad. `uplink-cli` liest normale TOML-Dateien und zeigt hypothetische Planung: Exit 0 geplant, 2 ungültig, 3 abgelehnt, 4 Eingang ausstehend. Deklarierte Beispielprofile beweisen keinen gemessenen OBS-Eingang und keine Plattformfreigabe.

Der [Offline-Mediennachweis](mediennachweis.md) belegt Dateimuxing/Remuxing und lokale Decodierbarkeit von AV1 bzw. H.264 mit zwei AAC-Spuren in E-FLV mit dem geprüften FFmpeg 8.1.2. Die gemeinsame Zeitverschiebung und verlorene Skip-Samples-Metadaten sind dort ausdrücklich dokumentiert. Das entscheidet nicht über die Produktionsengine und beweist keine OBS-, RTMPS- oder Plattformstrecke.

Die [isolierte Scuffle-Probe](scuffle-nachweis.md) ergänzt 13 Debug-/13 Release-Tests mit kleinen künstlichen Fixtures und statischen FFmpeg-8-Paketmanifesten. Sie verändert das Root-Cargo nicht. Ein 35-Byte-AVCC-Header zeigte einen Abhängigkeitsfehler; deshalb beschränkt der Testadapter H.264 vor Scuffle auf den geprüften Baseline-SequenceHeader. Diese lokale Absicherung ist keine Produktionsfreigabe des Parsers.

`uplink-ingest` ist ein [lokal gemessener RTMPS-Eingang](rtmps-nachweis.md) mit Rustls, begrenztem Scuffle-RTMP, injizierter Autorisierung und serverseitig frischen Verbindungsgenerationen. AV1/H.264 mit zwei AAC-Spuren erhält pro Test 240 komprimierte Pakete und drei Header mit unveränderten Nutzdaten und Zeitstempeln. Metadaten zählen zum Trackbudget und übernehmen keine fremde Codec-Revision. Öffentliche Brokeranbindung, Standard-OBS, Encoderbetrieb und vollständige Plattformausgänge sind weiter offen.

## Technische Grenzen der Zielarchitektur

| Verantwortung | Vertrag / Invariante |
| --- | --- |
| Identität und Rechte | Authentifizierte Plattform-ID, vorhandene OAuth-Verbindung und autoritativer Tokenbroker; keine zweite Tokenablage |
| RTMPS-/E-RTMP-Eingang | AV1 bevorzugt, H.264 unterstützt, freizugebendes HEVC; Limits vor Decode, explizite Tracks, Zeitbasis und Sessiongeneration; kein SRT |
| Profilplanung | Gemessene Quelle × belegte Ziel-/Kontofähigkeiten × Medienfähigkeiten × reservierte Ressourcen; gewünschte und aktive Profile getrennt |
| Medienverarbeitung | Video-/Audiopassthrough separat; einmaliges Decode soweit möglich, ein Encode je identischem Videoprofil einer Session |
| Plattformausgänge | Je Ziel eigene Sitzung, Verpackung, Audio-Routing, Puffer, Status und Reconnect; langsames Ziel blockiert keine anderen |
| Oberfläche/Chat | Geprüfte Assets neu anbinden; zentraler autorisierter Kanalstrom, Ereignisupdates und sichtbare Aktionsbestätigung; persistente OBS-Zugänge |
| VOD-Übergabe | Wirklich vorhandenes autorisiertes Medienobjekt, explizite Audio-Rollen und Exportauftrag; Quelle/Speicher/Repository/Aufbewahrung offen |
| Betrieb | Ressourcenreservierung, eindeutige Sessionzuständigkeit und Messung; gemeinsame Arbeit nicht je Ausgang doppelt zählen |

Diese Verantwortlichkeiten sind keine Vorgabe für ebenso viele Crates oder Microservices. Weitere Grenzen entstehen erst aus nachgewiesenem Bedarf.

Der Video-Identitätsschlüssel berücksichtigt Mandant, Session/Generation, Quellspur, Layoutrevision, Auflösung, Bildrate, Codec/Profil/Level, Farbsignal/Bit-Tiefe, Rate-Control-/Bitratenparameter und GOP/Keyframes. Andere Audioauswahl allein erzeugt keinen weiteren Video-Encode. Abweichende Bilder oder Qualitätsanforderungen bleiben getrennt. Keine private Medienwiederverwendung zwischen Mandanten oder Generationen.

Live-/VOD-Mix sind ausdrückliche Rollen mit eigener Zuordnung zwischen OBS, Eingang und Plattform. Kein stiller Live-Mix-Ersatz bei fehlendem VOD-Audio. Wartebild und Rückwechsel müssen dieselben Regeln erfüllen. F4 bestimmt das Verhalten betroffener Ausgänge.

Gameplay und Kamera werden serverseitig aus einem fertigen Quellbild ausgeschnitten. Layoutrevision und Übernahmezeitpunkt bleiben nachvollziehbar; keine zweite Kamera oder KI-Verfolgung voraussetzen. Der frühere Screenshot liegt dieser Arbeitsrunde nur als Textbeschreibung vor. Vorschauen, komprimierter Delay, kurze Reserve, Rohbildpuffer und Archiv bekommen getrennte Budgets. F5 entscheidet Delay-Spanne, aktive Änderung, Fristen und Stop-Semantik.

VOD-Objekte sollen Nutzer-/Session-/Medienidentität, geschützte Speicherreferenz, Vollständigkeit/Integrität, bekannte Dauer, Videoeigenschaften, Audio-Rollen, zeitlich gültige Layoutrevisionen und Aufbewahrung tragen. Exportaufträge referenzieren eine geprüfte YouTube-Verbindung und explizite Metadaten/Sichtbarkeit. Resume-Fortschritt und unklarer Abschluss müssen dauerhaft abgeglichen werden; Upload, Verarbeitung, Veröffentlichung und Löschung sind getrennte Zustände. F1–F3 legen Quelle, Auslöser/Ziel und Speicherentscheidung fest.

Konfiguration kommt aus normalen Dateien, Secrets aus Infisical bzw. dem vorhandenen autorisierten Broker. Keine ENV-Konfiguration, unnötige Rechte, neue Tokenkopie oder ungefragt eingebaute LLM-Abhängigkeit. Medienparser und öffentliche Eingaben erhalten reproduzierbare Fehlerfälle, Fuzztests und Limits vor teurer Arbeit.

## Reihenfolge der weiteren Umsetzung

1. **Medienkombination beweisen:** Kandidaten mit versionierten Vektoren auf E-RTMP, AV1/H.264/freigegebenes HEVC, Mehrspur-Audio, Zeitstempel und ausgehende Verpackung prüfen. Keine Enginewahl allein aus Bibliotheksbeschreibung. Parallel echte Plattform-/Kontorechte und bestehenden Broker prüfen.
2. **Eingang vervollständigen:** den lokal nachgewiesenen RTMPS-Eingang mit vorhandenem Broker und Standard-OBS verbinden; freigegebene Profile, reale Live-/VOD-Zuordnung und lange Sessions nachweisen. Eine konkrete OBS-Lücke offenlegen statt Plugin-Pflicht oder Verlust der VOD-Spur zu erfinden.
3. **Medien und vier Ausgänge bauen:** gemeinsame Encodes und kompatibles Passthrough; Twitch Enhanced Broadcasting/HEVC-1440p/Dual Format/Audio, YouTube-Broadcast-Lifecycle und gegebenenfalls natives Dual Stream, belegte Kick-Profile sowie tragfähige TikTok-Integration. Ein Socket ist kein öffentlicher Livestream.
4. **Bedienung und Störungen integrieren:** erhaltene Docks/Overlays und Konten neu anbinden, Hochkant/Layouts, Chat/Aktivitäten, gewünschte gegen aktive Profile, Puffer/Delay/Wartebild/Stop zusammen prüfen. F4–F6 begrenzen nur die davon abhängige Arbeit.
5. **VOD vervollständigen:** F1–F3 entscheiden, Medienobjekt an reale Quelle anbinden, autorisierten YouTube-Upload mit Resume/Abschlussabgleich/Verarbeitung/Aufräumen bauen. Verbindung autorisiert keine automatische Veröffentlichung.
6. **Qualität und Betrieb belegen:** H.264-Referenz mit 6.000 kbit/s gegen AV1-Versuchspunkte, sichtbare Qualität und Metriken, Last/Störungen, Ressourcen und sichere Migration/Rückkehr messen. Keine garantierten 40 Prozent oder alte Kundenzahl übernehmen.
7. **Produktiv umstellen:** nach erfüllten nötigen Abnahmen mergen/pushen, deployen, tatsächlich zuständige User-Unit neu starten und live prüfen. Vor endgültiger Entfernung des alten Repositorys alle Laufzeitabhängigkeiten und benötigten Daten/Assets klären; kein unfertiges Gerüst auf den alten Ingest schalten.

Diese Reihenfolge verkürzt den Vier-Plattform-Umfang nicht. Das vollständige Ergebnis ist erst mit den zugehörigen Nachweisen produktionsfähig.
