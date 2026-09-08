# Offline-Nachweis: AV1/H.264 mit zwei AAC-Spuren in E-FLV

Gemessen am 8. September 2026. Dieser Versuch prüft Dateimuxing, Demuxing,
Streamcopy und lokale Decodierbarkeit mit künstlichen Medien. Er legt keine
Produktionsengine fest und ist kein OBS-, RTMPS- oder Plattform-Live-Test.

## Ergebnis

Das installierte FFmpeg 8.1.2 transportiert ein AV1-Video und zwei unterschiedliche
AAC-Spuren gemeinsam in FLV und wieder zurück. Derselbe Versuch funktioniert mit
H.264. Alle drei komprimierten Spurinhalte bleiben über die geprüften Remuxwege
unverändert. Das System-FFmpeg 6.1.1 lehnt beide Mehrspurversuche ab.

| Prüfung | AV1 + 2 × AAC | H.264 + 2 × AAC |
| --- | --- | --- |
| Quelldatei synthetisch erzeugen | Exit 0 | Exit 0 |
| FFmpeg 6.1.1: MKV → FLV | Exit 234, Audioanzahl abgelehnt | Exit 234, Audioanzahl abgelehnt |
| FFmpeg 8.1.2: MKV → E-FLV | Exit 0 | Exit 0 |
| FFmpeg 8.1.2: E-FLV → MKV | Exit 0 | Exit 0 |
| FFmpeg 8.1.2: E-FLV → E-FLV | Exit 0 | Exit 0 |
| Komprimierte Streamhashes über alle vier Dateien | Alle drei identisch | Alle drei identisch |
| Vollständiges lokales Decodieren aller Spuren | Exit 0 | Exit 0 |
| E-FLV-Pakete laut ffprobe | Video 50, Audio 95 + 95 | Video 50, Audio 95 + 95 |

Der konkrete Fehler von 6.1.1 war `at most one audio stream is supported in flv`.
Diese gemessene Versionsgrenze darf nicht zur Behauptung werden, FFmpeg könne
grundsätzlich kein Mehrspur-FLV.

## Quellen und Kandidaten

Im FFmpeg-Quellstand `1a748fe2cd` verwendet der FLV-Muxer getrennte Zähler für
Video und Audio sowie `track_idx_map`. Weitere AAC-Spuren werden mit erweitertem
Audiokopf, `AudioPacketTypeMultitrack`, FourCC `mp4a` und Tracknummer geschrieben.
Der Demuxer liest diese Tracknummern. Bereits der eingesehene Release-Quellstand
`n8.0` enthält diese Mechanik. Das ist Quellcodebefund; der obige konkrete
Dateiversuch liefert den zusätzlichen Laufnachweis.
[FFmpeg flvenc.c am Build-Commit](https://github.com/FFmpeg/FFmpeg/blob/1a748fe2cd/libavformat/flvenc.c),
[FFmpeg flvdec.c am Build-Commit](https://github.com/FFmpeg/FFmpeg/blob/1a748fe2cd/libavformat/flvdec.c),
[FFmpeg n8.0 flvenc.c](https://github.com/FFmpeg/FFmpeg/blob/n8.0/libavformat/flvenc.c).

`scuffle-rtmp` 0.2.3 ist laut eigener Dokumentation ein Baustein für
RTMP-Serververbindungen; `ServerSession`/`SessionHandler` liefern empfangene
Medien und Publish-Ereignisse. Daraus folgt kein fertiger Twitch-Pushclient.
`scuffle-flv` 0.2.2 dokumentiert Demuxing einschließlich alter und erweiterter
Audiodaten über `AudioData::demux`. Beide Crates nennen als E-RTMP-v2-Bezug
`v2-2024-10-22-b1`. Der im Produktvertrag genannte jüngere v2-Stand ist damit
nicht automatisch vollständig abgedeckt.
[scuffle-rtmp 0.2.3](https://docs.rs/scuffle-rtmp/0.2.3/scuffle_rtmp/),
[Server-API](https://docs.rs/scuffle-rtmp/0.2.3/scuffle_rtmp/session/server/),
[scuffle-flv 0.2.2](https://docs.rs/scuffle-flv/0.2.2/scuffle_flv/),
[AudioData](https://docs.rs/scuffle-flv/0.2.2/scuffle_flv/audio/struct.AudioData.html).

Aus diesen Befunden ergibt sich eine begrenzte Kandidatenwahl: Scuffle für einen
neu geschriebenen Rust-Ingest mit expliziter Trackprüfung untersuchen; FFmpeg 8.1
als unabhängig gemessenen Mux-/Demux- und Verarbeitungsbaustein berücksichtigen.
Dieser Versuch führt keinen Scuffle-Code aus und beweist keine Interoperabilität
zwischen Scuffle, OBS und Twitch. Ein eigener alter Relay-Medienkern wird dafür
nicht kopiert. Ein endgültiger Medienworker braucht weitere Kombinationstests.

## Gemessene Werkzeuge und Herkunft

| Werkzeug | Version | SHA-256 des ausführbaren Programms |
| --- | --- | --- |
| `/usr/bin/ffmpeg` | `6.1.1-3ubuntu5` | `ed16af623947494a72e284b6eb8ff225f2da22b38b5d5069c2fd4b4ba3384e41` |
| `/usr/bin/ffprobe` | `6.1.1-3ubuntu5` | `272f6ebc634a63d9c8b4ca68e964119d980f25154e5aa2c35e5487da48e9a58f` |
| Isoliertes `ffmpeg` | `n8.1.2-50-g1a748fe2cd-20260831` | `ad7a8c8e8fe4f50972f32f63705cfcc57f44cd3531f57aa8defe388372242f5e` |
| Isoliertes `ffprobe` | `n8.1.2-50-g1a748fe2cd-20260831` | `150bfd75016992a8d495a5f5c16cd93387a21c059f4309ed1e6342659aef48b3` |

Die isolierten Programme lagen bereits unter
`/opt/rs-relay/ffmpeg8/releases/c733b4b2951e5957e15505f788b2c65a7a41b6da4b289e295852cc38079b4d2b/`.
Die vorhandene Installationsquelle nennt das BtbN-Asset
`ffmpeg-n8.1.2-50-g1a748fe2cd-linux64-gpl-8.1.tar.xz` aus
`autobuild-2026-08-31-13-27`; dieses Asset und der bezeichnete FFmpeg-Commit sind
bei den jeweiligen Herausgebern auffindbar. Die lokalen Binärhashes wurden jetzt
erfasst. Es wurde kein neues Binary geladen, kein Archiv erneut authentifiziert
und kein produktives Binary ausgetauscht.
[BtbN-Release](https://github.com/BtbN/FFmpeg-Builds/releases/tag/autobuild-2026-08-31-13-27),
[FFmpeg-Commit](https://github.com/FFmpeg/FFmpeg/commit/1a748fe2cd).

## Material, Identität und Zeitgrenzen

Die Quelle enthält zwei Sekunden `testsrc2`, 320 × 180 bei 25 fps, sowie zwei
Mono-AAC-Spuren mit 48 kHz und jeweils 64 kbit/s. Spur A wurde aus 440 Hz, Spur B
aus 880 Hz erzeugt. AV1 stammt aus `libaom-av1`, H.264 aus `libx264`.
Die schnellen CRF-Einstellungen dienen nur der kleinen Testdatei und sind keine
freigegebenen Upload-, CBR- oder Qualitätsprofile.

Die Hashwerte aus `-map 0 -c copy -f streamhash -hash sha256` sind für Quelle,
E-FLV, zurückgeschriebenes MKV und erneut geschriebenes E-FLV jeweils gleich:

| Inhalt | SHA-256 der komprimierten Spur |
| --- | --- |
| AV1-Video | `48b65a5c16fecd5e46ce455f51b10609ef7b077aa620dee433fab81117890b8f` |
| H.264-Video | `0c53b8b35afebe4d3faf18794b8606c79be7931be44dd9302db20301b2297456` |
| AAC aus 440 Hz | `9c95644a5f5557bc8b0697175e20ef8caa3c146f069a38151378185690a15ef9` |
| AAC aus 880 Hz | `44a9b9c37c707aa7c2f4f77d48c8ec88f9ac5171a626d286b61f66f081c32fbe` |

ffprobe meldet im E-FLV Video als Streamindex 0, die beiden AAC-Spuren als
Streamindex 1 und 2. Es gibt in der abgefragten Ausgabe keine Rollen-Titel und
kein ausgegebenes `id`-Feld. Die Zuordnung im Versuch ist durch die explizite
Map-Reihenfolge und die verschiedenen Spurinhalte belegt. Sie beweist keine
Twitch-Live-/VOD-Rolle. Die Binärinspektion des AV1-E-FLV zeigt für die zweite
AAC-Spur den erweiterten Header `95 00 6d 70 34 61 01`: Multitrack-SequenceStart,
FourCC `mp4a`, Track 1. Die erste AAC-Spur verwendet den alten AAC-Kopf und damit
den impliziten Track 0. Medienart und Tracknummer gehören zusammen; ein globaler
Streamindex ist kein plattformübergreifender Rollenvertrag.

Nach AAC-Decodierung wurden im Ausschnitt 0,5–1,5 Sekunden 48.000 Samples je Spur
gemessen. `astats` zählt 880 bzw. 1.761 Nulldurchgänge. Das bestätigt die
unterschiedlichen Testtöne nach dem Transport; die genaue Grenzphase kann einen
zusätzlichen Nulldurchgang liefern.

Eine wichtige Abweichung ist ausdrücklich gemessen: Das MKV enthält beim ersten
AAC-Paket PTS/DTS −21 ms und `Skip Samples=1024`; das erste Videopaket liegt bei
0 ms. Nach dem FLV-Mux liegen die ersten AAC-Pakete bei 0 ms und das erste Video
bei 21 ms. Die Paketfolge mit Streamindex, PTS, DTS, Größe und Flags stimmt für
beide Codec-Fälle nach genau dieser gemeinsamen Verschiebung um 21 ms überein
(Vergleich Exit 0). Beim anschließenden FLV→FLV-Remux bleibt diese Paketliste
ohne weitere Verschiebung gleich (Exit 0).

Die Skip-Samples-Metadaten der MKV-Quelle erscheinen im E-FLV nicht. Ein gleicher
komprimierter Hash belegt deshalb keine identische Audio-Anfangsbehandlung und
keine vollständig erhaltene Containersemantik. AAC-Priming, Padding, Zeitbasis
und die explizite Rollenregistrierung müssen in der tatsächlichen Eingangskette
behandelt und an OBS sowie Plattformausgaben geprüft werden.

## Reproduktion

Die folgenden Shellvariablen sind lokale Namen für Testwerkzeuge und einen
temporären Ordner. Es werden keine Umgebungsvariablen für Dienstkonfiguration
oder Secrets verwendet. Alle Medien bleiben künstlich; kein Netzwerkziel ist
Teil der Befehle.

```bash
uplink_testdir=$(mktemp -d /tmp/uplink-mediennachweis-XXXXXX)
uplink_ff8=/opt/rs-relay/ffmpeg8/releases/c733b4b2951e5957e15505f788b2c65a7a41b6da4b289e295852cc38079b4d2b/ffmpeg
uplink_probe8=/opt/rs-relay/ffmpeg8/releases/c733b4b2951e5957e15505f788b2c65a7a41b6da4b289e295852cc38079b4d2b/ffprobe

erzeuge_uplink_testquelle() {
  uplink_codec=$1
  shift
  /usr/bin/ffmpeg -nostdin -hide_banner -loglevel warning \
    -f lavfi -i testsrc2=size=320x180:rate=25:duration=2 \
    -f lavfi -i sine=frequency=440:sample_rate=48000:duration=2 \
    -f lavfi -i sine=frequency=880:sample_rate=48000:duration=2 \
    -map 0:v:0 -map 1:a:0 -map 2:a:0 "$@" \
    -c:a aac -b:a 64k \
    -metadata:s:a:0 title=live_440_hz -metadata:s:a:1 title=vod_880_hz \
    -y "$uplink_testdir/quelle-$uplink_codec.mkv"
}
erzeuge_uplink_testquelle av1 -c:v libaom-av1 -cpu-used 8 \
  -threads:v 2 -row-mt 1 -crf 40 -b:v 0
erzeuge_uplink_testquelle h264 -c:v libx264 -preset ultrafast \
  -threads:v 2 -crf 28 -pix_fmt yuv420p

# Negativtest: jeweils Exit 234 erwartet, keine erfolgreiche Datei.
for uplink_codec in av1 h264; do
  /usr/bin/ffmpeg -nostdin -hide_banner -loglevel error \
    -i "$uplink_testdir/quelle-$uplink_codec.mkv" \
    -map 0:v:0 -map 0:a:0 -map 0:a:1 -c copy -f flv \
    -y "$uplink_testdir/alt-$uplink_codec.flv"
  printf 'Negativtest %s: Exit %s\n' "$uplink_codec" "$?"
done

for uplink_codec in av1 h264; do
  "$uplink_ff8" -nostdin -hide_banner -loglevel error \
    -i "$uplink_testdir/quelle-$uplink_codec.mkv" \
    -map 0:v:0 -map 0:a:0 -map 0:a:1 -c copy -f flv \
    -y "$uplink_testdir/neues-$uplink_codec.flv" || exit 1
  for uplink_container in mkv flv; do
    "$uplink_ff8" -nostdin -hide_banner -loglevel error \
      -i "$uplink_testdir/neues-$uplink_codec.flv" -map 0 -c copy \
      -y "$uplink_testdir/rueck-$uplink_codec.$uplink_container" || exit 1
  done
  for uplink_datei in "quelle-$uplink_codec.mkv" "neues-$uplink_codec.flv" \
      "rueck-$uplink_codec.mkv" "rueck-$uplink_codec.flv"; do
    "$uplink_ff8" -nostdin -hide_banner -loglevel error \
      -i "$uplink_testdir/$uplink_datei" -map 0 -c copy \
      -f streamhash -hash sha256 -y "$uplink_testdir/$uplink_datei.hash" || exit 1
    "$uplink_probe8" -v error -show_packets \
      -show_entries packet=stream_index,pts,dts,size,flags -of compact=p=0 \
      "$uplink_testdir/$uplink_datei" > "$uplink_testdir/$uplink_datei.zeit"
  done
  for uplink_datei in "neues-$uplink_codec.flv" "rueck-$uplink_codec.mkv" \
      "rueck-$uplink_codec.flv"; do
    cmp "$uplink_testdir/quelle-$uplink_codec.mkv.hash" \
      "$uplink_testdir/$uplink_datei.hash" || exit 1
  done
  "$uplink_probe8" -v error -count_packets \
    -show_entries stream=index,id,codec_name,codec_type,nb_read_packets:stream_tags=title \
    -of json "$uplink_testdir/neues-$uplink_codec.flv"
  "$uplink_ff8" -nostdin -hide_banner -loglevel error \
    -i "$uplink_testdir/neues-$uplink_codec.flv" -map 0 -f null - || exit 1
  cmp "$uplink_testdir/neues-$uplink_codec.flv.zeit" \
    "$uplink_testdir/rueck-$uplink_codec.flv.zeit" || exit 1
done

# Priming/PTS-Abweichung sichtbar machen: Quelle beginnt mit -21 ms,
# E-FLV mit 0 ms; das Video verschiebt sich gleichzeitig von 0 auf 21 ms.
head -n 4 "$uplink_testdir/quelle-av1.mkv.zeit"
head -n 4 "$uplink_testdir/neues-av1.flv.zeit"

for uplink_ton in 0 1; do
  "$uplink_ff8" -nostdin -hide_banner -loglevel info \
    -i "$uplink_testdir/neues-av1.flv" -map "0:a:$uplink_ton" \
    -af atrim=start=0.5:end=1.5,astats=metadata=0:reset=0 -f null - \
    2>&1 | rg 'Zero crossings|Number of samples|Error'
done

# Ausschließlich den gerade selbst erzeugten Testordner entfernen.
rm -r -- "$uplink_testdir"
```

## Noch nicht bewiesen

- Standard-OBS über RTMPS einschließlich Authentifizierung und zweitem Audiomix.
- Interoperabilität von Scuffle mit dieser E-FLV-Ausgabe und dem vereinbarten
  aktuellen E-RTMP-v2-Vertrag.
- Twitch-Enhanced-Broadcasting-Sitzung, 1440p, serverseitige Leiter,
  Trackzuordnung und tatsächliches Live-/VOD-Audio.
- Beliebige Tracknummern, Umordnung, Profilwechsel, Headerwechsel,
  fehlende Spuren, Zeitstempel-Wrap und beschädigte Eingaben.
- Wartebildwechsel, Reconnect, Ausgänge mit Rückstau oder Zielausfall.
- Echtzeitlast, lange Sessions, Hardwareencoder, Bildqualität und Uploadersparnis.
- VOD-Archivformat, Speicherung, YouTube-Upload oder Veröffentlichung.

Die temporären Medien, Hashdateien und Paketlisten dieses Laufs wurden nach der
Dokumentation entfernt. Im Repository verbleibt nur dieser Bericht; keine
Binärdatei, Medienaufnahme, Laufzeitkonfiguration oder Zugangsdaten.
