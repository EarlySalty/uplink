# Scuffle-E-FLV-Probe

Eigenständiges Rust-Testprojekt für `scuffle-flv = 0.2.2`. Kein Bestandteil des
Produktionsworkspace und kein freigegebener Ingest-Parser. Der Ergebnisbericht
steht unter [`docs/scuffle-nachweis.md`](../../docs/scuffle-nachweis.md).

## Wiederholen

Vom Repository aus mit Rust 1.97.1:

```bash
cargo fmt --manifest-path experiments/scuffle-probe/Cargo.toml -- --check
cargo clippy --manifest-path experiments/scuffle-probe/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path experiments/scuffle-probe/Cargo.toml --locked
cargo test --manifest-path experiments/scuffle-probe/Cargo.toml --release --locked
cargo audit --file experiments/scuffle-probe/Cargo.lock
```

Die Tests benötigen kein FFmpeg, keinen Dienst, keine Konfiguration und keine
Zugänge. Die üblichen Cargo-Abhängigkeiten müssen verfügbar sein; `Cargo.lock`
fixiert ihre Versionen. `cargo-audit` ist ein separates Prüfwerkzeug, beim
Nachweis in Version 0.22.2 verwendet. CI prüft Formatierung, Clippy sowie die
Tests im Debug- und Release-Profil.

`fixtures/` enthält ausschließlich zwei Sekunden künstliches Testbild mit
440-Hz-/880-Hz-Testtönen, zusammen rund 205 KiB E-FLV, und die von FFprobe 8
erzeugten Paket-/Headerlisten. Die Tests vergleichen Medienart, Tracknummer,
Reihenfolge, PTS, DTS, Paketgröße, Payloadhash und SequenceHeader-Hash.
`fixtures/manifest.json` hält Werkzeugversionen, Hashes, Dateigrößen und die
explizite Trackzuordnung dieses Versuchs fest.

Die FLV-Tracknummer ist pro Medienart zu verstehen: Video 0 und Audio 0 sind
unterschiedliche Spuren. FFprobe-Streamindex 2 entspricht hier Audio-Track 1.
Die feste Zuordnung gilt für diese Testdateien; sie ist keine allgemeine
FFprobe- oder Plattformregel und benennt keine Live-/VOD-Audiorollen.

## Testdaten neu erzeugen

Nur dafür werden die drei im Manifest bezeichneten Programme benötigt. Ihre
ausführliche Herkunft steht im [FFmpeg-Bericht](../../docs/mediennachweis.md).
Die normalen Rust-Tests brauchen sie nicht. Die konkreten Dateipfade werden als
Argumente übergeben, keine Umgebungsvariablen für Konfiguration verwendet.

```bash
mkdir /tmp/uplink-neue-fixtures
/pfad/ffmpeg-6.1.1 -nostdin -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=320x180:rate=25:duration=2 \
  -f lavfi -i sine=frequency=440:sample_rate=48000:duration=2 \
  -f lavfi -i sine=frequency=880:sample_rate=48000:duration=2 \
  -map 0:v:0 -map 1:a:0 -map 2:a:0 \
  -c:v libaom-av1 -cpu-used 8 -threads:v 2 -row-mt 1 -crf 40 -b:v 0 \
  -c:a aac -b:a 64k /tmp/uplink-neue-fixtures/av1.mkv
/pfad/ffmpeg-6.1.1 -nostdin -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=320x180:rate=25:duration=2 \
  -f lavfi -i sine=frequency=440:sample_rate=48000:duration=2 \
  -f lavfi -i sine=frequency=880:sample_rate=48000:duration=2 \
  -map 0:v:0 -map 1:a:0 -map 2:a:0 \
  -c:v libx264 -preset ultrafast -threads:v 2 -crf 28 -pix_fmt yuv420p \
  -c:a aac -b:a 64k /tmp/uplink-neue-fixtures/h264.mkv
for uplink_codec in av1 h264; do
  /pfad/ffmpeg-8.1.2 -nostdin -hide_banner -loglevel error \
    -i "/tmp/uplink-neue-fixtures/$uplink_codec.mkv" -map 0 -c copy -f flv \
    "/tmp/uplink-neue-fixtures/$uplink_codec.flv" || exit 1
  /pfad/ffprobe-8.1.2 -v error -show_packets -show_streams -show_data_hash sha256 \
    -show_entries packet=stream_index,pts,dts,size,data_hash:stream=index,extradata_hash \
    -of json "/tmp/uplink-neue-fixtures/$uplink_codec.flv" \
    > "/tmp/uplink-neue-fixtures/$uplink_codec.ffprobe.json" || exit 1
done
cd experiments/scuffle-probe
cmp fixtures/av1.flv /tmp/uplink-neue-fixtures/av1.flv
cmp fixtures/h264.flv /tmp/uplink-neue-fixtures/h264.flv
cmp fixtures/av1.ffprobe.json /tmp/uplink-neue-fixtures/av1.ffprobe.json
cmp fixtures/h264.ffprobe.json /tmp/uplink-neue-fixtures/h264.ffprobe.json
```

Der Ausgabeordner darf vorher nicht existieren. Die Befehle erzeugen MKV-
Zwischenquellen, remuxen mit FFmpeg 8 in E-FLV und schreiben die FFprobe-Referenz.
Sie installieren nichts und laden keine Programme herunter. Am
8. September 2026 waren alle vier regenerierten
Dateien bytegleich mit den gesicherten Vektoren. Andere Encoder-/Librarybuilds
können abweichende Bytes liefern; Referenzen deshalb nicht ungeprüft ersetzen.
Eigene temporäre Medien nach dem Vergleich entfernen.

## Grenzen und absichtlicher Fehlernachweis

Der vorgeschaltete Prüfcode akzeptiert eine enge Teilmenge: das gemessene
AV1-Video oder H.264 Baseline, ein alter AAC-Track und zusätzliche AAC-OneTrack-
Pakete. Dateigröße, Taggröße, Headergröße, Taganzahl und Trackanzahl sind begrenzt.
Allgemeine AMF-Bäume, ModEx und weitere Mehrspurformen werden nicht freigegeben.
Nur das exakte leere ColorInfo-Objekt des AV1-Vektors darf durch den
Metadatenparser. AAC-Konfigurationsbytes werden von Scuffle nicht semantisch
validiert. Das Projekt decodiert keine Medien.

Der Test `direct_scuffle_extended_sps_panics_with_overflow_checks` demonstriert
im Debug-Profil einen Bibliotheks-Panic mit einem 35-Byte-AVC-Header. Der Test
fängt diesen erwarteten Panic ausschließlich zur Dokumentation ab; bei
`--nocapture` kann die Panic-Meldung trotz erfolgreichem Test sichtbar sein.
Er ist im Release-Profil nicht enthalten. Ein eigener Release-Test belegt dort
stattdessen den beobachteten Bereichsfehler mit dem Zwischenwert
`18446744073709551615`; daraus folgt keine allgemeine Release-Sicherheit. Der eigene
`extended_sps_is_rejected_before_scuffle`-Test läuft in beiden Profilen und
belegt die Ablehnung vor dem Bibliotheksaufruf. Das ist kein allgemeines
Panic-Abfangkonzept und keine Absicherung aller möglichen Codec-Eingaben.

Nicht geprüft sind OBS, RTMPS, Pushclients, Plattformrollen, Twitch Enhanced
Broadcasting, HEVC, beliebige Headerwechsel, Failover oder Echtzeitlast.
