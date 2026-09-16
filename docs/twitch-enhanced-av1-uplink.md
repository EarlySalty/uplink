# Twitch Enhanced über einen AV1-Uplink

Stand: 16. September 2026.

## Ziel

Dieser Modus ist **nicht** Twitch Native 2K. Er ist für schwankende oder knappe Uploadleitungen gedacht: OBS sendet genau einen hochwertigen Masterstream zum Uplink-Server. Bevorzugter Testpfad ist `1920×1080 @ 60 fps` als Hardware-AV1. Uplink decodiert diesen Master einmal und erzeugt daraus die von Twitch beim GoLive ausgehandelte H.264-Enhanced-Leiter.

```text
OBS / Streaming-PC
  1080p60 AV1 Hardwareencode
  Live AAC + VOD AAC
          |
          | eine Videospur, niedriger Upload
          v
Uplink
  AV1 decode
  +-----------------> Twitch H.264 1080p60
  +-----------------> Twitch H.264 720p...
  +-----------------> weitere von GoLive verlangte H.264-Stufen
```

Die genaue Twitch-Leiter wird nicht hart codiert. Auflösung, Bildrate, Bitrate, GOP, Track-IDs und Zahl der Stufen kommen weiterhin aus der aktuellen GoLive-Antwort des verbundenen Twitch-Kontos.

## Trennung von Native 2K

- `enhanced`: Upload-Sparmodus. Eingang bis zum freigegebenen Testkorridor AV1 1080p60; Twitch-Ausgänge sind H.264. Kein natives Twitch-2K.
- `native_2k`: OBS liefert 2560×1440@60 HEVC bereits Twitch-kompatibel. Uplink kopiert die HEVC-Topspur und erzeugt nur niedrigere H.264-Stufen.
- `native_2k_av1`: experimenteller AV1→HEVC-2K-Pfad. Auf diesem CPU-Host standardmäßig gesperrt, weil der 1440p-HEVC-Neuencode keine belastbare Echtzeitreserve erreicht.

## Gemessener AV1-1080p-Testkorridor

Der synthetische Belastungstest `enhanced_av1_1080_load_probe` verwendet einen AV1-Master mit 1920×1080@60 und erzeugt fünf H.264-Ausgänge gleichzeitig. Der Encoderprozess läuft absichtlich mit `nice 19`, damit die Messung die übrigen Dienste nicht verdrängt.

Gemessener Lauf auf dem aktuellen EPYC-Host:

- AV1 1080p60 nur decodieren: etwa `10,23×` Echtzeit.
- AV1 1080p60 → fünf H.264-Stufen gleichzeitig: etwa `1,34×` Echtzeit.
- Testleiter: 1080p60 7,5 Mbit/s, 720p60 4,5 Mbit/s, 480p30 2,5 Mbit/s, 360p30 1,2 Mbit/s, 240p30 0,5 Mbit/s.

Das ist eine Kapazitätsmessung für den begrenzten Korridor, keine Behauptung über jede mögliche Twitch-Leiter. Deshalb gilt die separate Admission-Regel:

- Quelle exakt AV1 1920×1080@60,
- nur Querformat,
- zwei bis fünf H.264-Ausgänge,
- kein Ausgang über 1920×1080@60,
- höchstens 7,5 Mbit/s pro Videospur,
- höchstens 20 Mbit/s Videoaggregat,
- keine B-Frames im aktuellen Software-x264-Vertrag.

`av1_1080_enhanced_units = 100` reserviert dafür das vollständige Enhanced-Kapazitätsbudget. Damit läuft höchstens eine solche Testsession gleichzeitig. Außerhalb des Korridors greift weiterhin nur die exakte profilbasierte Lastfreigabe.

## OBS-Testprofil

Für den ersten echten Test:

- Ausgabeauflösung: 1920×1080,
- 60 fps,
- Hardware-AV1 auf der Streaming-GPU,
- Startwert: 5 Mbit/s Video,
- Keyframe-Intervall: 2 s,
- zwei AAC-Spuren für Live und VOD wie im normalen Uplink-Vertrag.

Bei AMD kann HQCBR verwendet werden, wenn die installierte OBS-/AMF-Version den Modus stabil anbietet. Andernfalls CBR verwenden. Der Uplink-Analyzer liest die aktuelle OBS-Logdatei lokal im Browser, erkennt GPU und Encoder und speichert ein vollständiges normalisiertes Hostprofil automatisch; die Logdatei selbst wird nicht hochgeladen.

## Warum nicht fünf AV1-Spuren vom PC zum Server?

Der Zweck ist Upload zu sparen. Mehrere AV1-Renditions auf dem Client würden die Leitung wieder mit mehreren parallelen Videoströmen belasten und die Encoder-/Transportkomplexität auf den Streaming-PC zurückverlagern. Ein einzelner AV1-Master nutzt AV1 dort, wo es für dieses Produktziel den größten Nutzen hat: zwischen Streaming-PC und Uplink. Die Twitch-spezifische Leiter entsteht erst hinter der knappen Leitung auf dem Server.

## Live-Abnahme

Der synthetische Lasttest ersetzt den echten Twitch-Test nicht. Für die Freigabe werden beim verbundenen Konto benötigt:

1. tatsächliche GoLive-Leiter protokollieren, ohne Zugangsdaten zu loggen,
2. prüfen, dass die gesamte ausgehandelte Leiter innerhalb des AV1-1080p-Korridors liegt,
3. mindestens fünf Minuten echten OBS-Betrieb mit 1080p60 AV1,
4. aktive Twitch-Qualitätsstufen und Eingangs-/Ausgangsbitraten im Uplink-Status prüfen,
5. normal in OBS stoppen und einen sauberen Sessionabschluss belegen.
