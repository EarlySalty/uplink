# Rust-Nachweis: Scuffle demuxt AV1/H.264 und zwei AAC-Spuren

Gemessen am 8. September 2026. Ein neu geschriebenes, eigenständiges
[Rust-Experiment](../experiments/scuffle-probe/README.md) prüft `scuffle-flv`
0.2.2 gegen synthetische E-FLV-Dateien und unabhängig mit FFprobe 8 gewonnene
Referenzlisten. Es ist keine Produktionsabhängigkeit des Uplink-Workspace.

## Gemessenes Ergebnis

Scuffle ordnet in beiden Testdateien das Video und beide AAC-Spuren getrennt zu.
Alle Medienpakete stimmen mit FFprobe in Reihenfolge, Medienart/Tracknummer,
PTS, DTS, Größe und SHA-256 des komprimierten Inhalts überein. Auch die drei
SequenceHeader stimmen als SHA-256 mit FFprobes Extradata überein; die geparste
Video-Konfiguration wird dafür mit Scuffle erneut serialisiert.

| Messwert | AV1 + 2 × AAC | H.264 Baseline + 2 × AAC |
| --- | --- | --- |
| Medienart und Tracknummer | Video 0, Audio 0, Audio 1 | Video 0, Audio 0, Audio 1 |
| Medienpakete | 50 Video, 95 + 95 Audio | 50 Video, 95 + 95 Audio |
| Paketvergleich | 240/240 identisch | 240/240 identisch |
| SequenceHeader-Vergleich | 3/3 identisch | 3/3 identisch |
| Scuffle-Audio-/Video-Demuxaufrufe | 245 | 245 |
| Leere ColorInfo-Metadaten | 1 | 0 |

Die erste AAC-Spur nutzt den alten AAC-Kopf mit implizitem Track 0, die zweite
den erweiterten OneTrack-Kopf mit `mp4a` und Track 1. Video 0 und Audio 0 dürfen
nicht in denselben Eintrag geraten: Der Test belegt drei Schlüssel aus
Medienart plus Nummer, aber nur zwei unterschiedliche Zahlenwerte. FFprobes
globale Streamindizes 0/1/2 werden ausschließlich für diese ausdrücklich so
erzeugten Testdateien auf diese Schlüssel abgebildet.

Die Signale sind zwei Sekunden `testsrc2` in 320 × 180 bei 25 fps und zwei
48-kHz-Mono-AAC-Spuren aus 440 Hz bzw. 880 Hz. Es sind keine CBR- oder
Qualitätsprofile und keine echten Nutzermedien. Das Experiment vergleicht die
Extraktion derselben E-FLV-Datei durch zwei Bibliotheken. Es beweist keine
Live-/VOD-Rolle aus OBS und keine Korrektheit einer Plattformausgabe.

## API-Befund und Sicherheitsgrenzen

`AudioData::demux` und `VideoData::demux` sind vorhanden und werden im Test
tatsächlich aufgerufen. Die erweiterte Audiostruktur liefert FourCC und
`audio_track_id`. Die Dokumentation von `scuffle-flv` 0.2.2 nennt E-RTMP v2
`v2-2024-10-22-b1`; der aktuelle Vertrag ist damit nicht vollständig geprüft.
`scuffle-rtmp` 0.2.3 dokumentiert insbesondere Server-Sessions. Diese Probe
verwendet die RTMP-Crate nicht und belegt keinen Pushclient.
[scuffle-flv](https://docs.rs/scuffle-flv/0.2.2/scuffle_flv/),
[AudioData](https://docs.rs/scuffle-flv/0.2.2/scuffle_flv/audio/struct.AudioData.html),
[VideoData](https://docs.rs/scuffle-flv/0.2.2/scuffle_flv/video/struct.VideoData.html),
[scuffle-rtmp](https://docs.rs/scuffle-rtmp/0.2.3/scuffle_rtmp/).

Vor dem Kopieren eines Tags und dem Bibliotheksaufruf setzt die Probe harte
Grenzen: 2 MiB Dateieingabe, 64 KiB pro Tag, 4 KiB pro SequenceHeader,
4.096 Tags und 16 registrierte Tracks. Der Dateileser allokiert höchstens
2 MiB plus ein Erkennungsbyte für Übergröße. FLV-Magic, Version, Flags,
Headeroffset, StreamID, vollständige Taglängen und PreviousTagSize werden
geprüft. Zu große oder unbekannte Medienköpfe werden in den vorgesehenen
Negativtests mit null Audio-/Video-Demuxaufrufen abgewiesen.

Diese Grenzen begrenzen Eingaben und die Zahl der weitergereichten Objekte;
sie sind kein gemessener Heap-Höchstwert sämtlicher Bibliotheksallokationen.
Der Adapter akzeptiert nur die benötigte Teilmenge. Allgemeine AMF-Scripttags
werden übersprungen und nicht geparst. Das AV1-ColorInfo-Objekt wird nur nach
bytegenauer Prüfung gegen den leeren 38-Byte-Testvektor an Scuffle gegeben.
ModEx, weitere Mehrspurmodi und unbekannte Codecs sind nicht freigegeben.

Die gezielten Tests zeigen außerdem:

- AAC-SequenceHeader sind für diese API opaque Bytes: Eine ungültige
  AudioSpecificConfig aus zwei Nullbytes wird angenommen. Codecvalidierung
  ist zusätzlich nötig.
- Unbekannte Audio-FourCC werden von Scuffle erhalten. Der eigene Adapter
  muss erlaubte Codecs prüfen; der Test kontrolliert ausdrücklich `nope`.
- Der alte AVC-Header liefert die 24-Bit-Kompositionszeit als `u32`.
  `ff ff ff` erscheint als 16.777.215; der Adapter erweitert das Vorzeichen
  korrekt auf −1. Dieser kleine Negativvektor ist kein umfassender B-Frame-Test.
- Einzelne beschädigte AVC-/AV1-/Mehrkanal-Konfigurationsköpfe liefern Fehler.
  Frames vor dem SequenceHeader werden vom Adapter abgelehnt.

### Im Review gefundener Exp-Golomb-Fehler

Ein nur 35 Byte großer AVC-SequenceHeader mit Profil 100 und einem manipulierten
Extended-SPS gelangt ohne zusätzliche Profilprüfung bis
`scuffle-expgolomb` 0.1.5. 64 führende Nullbits, ein Einsbit und weitere
64 Nullbits erzeugen dort einen unzulässigen Zwischenwert. Das unabhängige
Review und die gesicherten Tests beobachten für exakt diesen Vektor:

| Profil | Direkter Scuffle-Aufruf |
| --- | --- |
| Debug mit Überlaufprüfung | Panic: `attempt to subtract with overflow` |
| Release dieses Experiments | Regulärer Fehler: `chroma_format_idc is out of range [0, 3]: 18446744073709551615` |

Der enge Probeadapter erlaubt deshalb für AVC-Konfigurationsköpfe nur das
getestete Baseline-Profil 66. Der problematische Vektor wird in beiden
Buildprofilen vor Scuffle mit null Demuxaufrufen abgelehnt. Der Debug-Test
verwendet `catch_unwind` ausschließlich, um den Bibliotheksfehler reproduzierbar
festzuhalten; der Release-Test hält das abweichende Ergebnis fest. Daraus folgt
weder ein Produktionskonzept zum Abfangen von Panics noch allgemeine
Release-Sicherheit. Größenlimits allein beheben diesen Fehler nicht.
[AVC-Konfigurationsparser](https://docs.rs/crate/scuffle-h264/0.2.2/source/src/config.rs),
[Exp-Golomb-Implementierung](https://docs.rs/crate/scuffle-expgolomb/0.1.5/source/src/lib.rs).

## Reproduktion und Provenienz

Das [Fixture-Manifest](../experiments/scuffle-probe/fixtures/manifest.json)
enthält SHA-256, Größen, Werkzeugstände und die Zuordnung aller vier kleinen
FLV-/JSON-Dateien. Zusammen enthalten die gesicherten Medien rund 205 KiB;
die zwei Referenzlisten enthalten rund 112 KiB. Die Rust-Tests benötigen nur
diese versionierten Dateien und die im eigenen `Cargo.lock` fixierten Crates.
Sie benötigen kein installiertes FFmpeg und greifen auf kein altes Repository,
keinen Dienst und keinen Netzwerkstream zu.

Die Referenzen stammen aus FFprobe
`n8.1.2-50-g1a748fe2cd-20260831`. Der ausführliche
[FFmpeg-Nachweis](mediennachweis.md) hält Werkzeugherkunft und Binärhashes fest.
Es wurde ein bereits vorhandenes Binary benutzt, kein Archiv erneut
authentifiziert und kein produktives Binary ausgetauscht. Die
[Reproduktionsanleitung](../experiments/scuffle-probe/README.md#testdaten-neu-erzeugen)
enthält die exakten Quell-, Mux- und FFprobe-Befehle. Die zweite Erzeugung ergab für alle vier
gesicherten Dateien denselben SHA-256; `cmp` bestätigte Bytegleichheit.

Gemessene Toolchain: Rust/Cargo 1.97.1. Das System-Cargo 1.75 unterstützt
Edition 2024 nicht und wurde für den Versuch nicht verwendet.
`scuffle-flv` 0.2.2 ist mit Crate-Prüfsumme
`77ce3edb7142932d70b064c245df1215be9333ccf83f88cf3a9abe961ca82ef8`
im eigenen Lockfile festgehalten. Es wird keine lokale Cratekopie eingebunden.

| Prüfung | Ergebnis |
| --- | --- |
| `cargo fmt -- --check` | Exit 0 |
| `cargo clippy --all-targets --locked -- -D warnings` | Exit 0 |
| `cargo test --locked` | 13 bestanden, Exit 0 |
| `cargo test --release --locked` | 13 bestanden, Exit 0 |
| `cargo audit --file Cargo.lock`, cargo-audit 0.22.2 | Exit 0, kein Advisory gemeldet |
| Fixture-Erzeugung und vollständiger Bytevergleich | Exit 0 |

Der Audit-Abgleich erfolgte gegen die am 8. September 2026 aktualisierte
RustSec-Datenbank mit 1.242 geladenen Advisories. Ein unauffälliger
Advisory-Abgleich widerlegt den oben selbst gemessenen Parserfehler nicht.

## Folgerung für die Architektur

Scuffle ist für die gemessene Demux-Teilmenge ein belegter Rust-Kandidat.
Vor einer Produktionsauswahl sind Codecvalidierung, Parserfehlerbehandlung,
gezielte Fuzztests und die übrigen benötigten E-RTMP-Formen zu klären.
Der Probeadapter ist bewusst eng und wird nicht als fertiger Ingest übernommen.

Offen bleiben OBS/RTMPS samt Authentifizierung und Audio-Rollenregistrierung,
Twitch-Push und dessen Trackmapping, Live-/VOD-Audio, HEVC, Header- und
Profilwechsel, Zeitstempel-Wrap, Wartebild/Failover, Rückstau und Echtzeitlast.
Die AAC-Priming-/Zeitbasisgrenze des FFmpeg-Berichts bleibt ebenfalls bestehen.
Aus dieser Probe wird keine Enginefreigabe oder Plattform-Livebehauptung abgeleitet.
