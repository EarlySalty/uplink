# Twitch Native 2K über Uplink

Stand: 15. September 2026.

## Ziel

Der Modus `native_2k` ist ausschließlich für echtes Twitch-2K über Enhanced Broadcasting gedacht. OBS schickt genau **einen** Videostream zum Uplink-Server: `2560×1440 @ 60 fps`, **HEVC**, 8-Bit YUV420/BT.709, plus die getrennten AAC-Spuren für Live und VOD. AV1 und H.264 werden für diesen Modus nicht auf dem Server in HEVC umkodiert.

Uplink reicht die 1440p-HEVC-Spur komprimiert als Twitch-Topspur weiter. Nur niedrigere H.264-Stufen, die Twitch im aktuellen GoLive-Vertrag verlangt, werden serverseitig mit `libx264` erzeugt. Die konkrete Leiter wird nicht hart verdrahtet: Twitchs GoLive-Antwort bleibt die Quelle für Anzahl, Auflösung, Bildrate, Bitrate, GOP und B-Frames. Ein bereits gemessener Accountvertrag bestand im Querformat aus 1440p60 HEVC sowie 1080p60/720p60/360p30 H.264; dieser Messwert ist ein Nachweisfall, keine universelle Twitch-Zusage.

## Hardwarevertrag des Quellrechners

Twitchs GoLive-Konfiguration verlangt GPU-Informationen. Da der Uplink-Server die 1440p-Spur **nicht** erzeugt, darf er dafür nicht seine eigene leere GPU-Liste als Encoderhardware melden. Pro Streamer wird deshalb ein explizites `Native2kClientProfile` gespeichert:

- CPU: physische/logische Kerne, optional Name/Takt,
- RAM: gesamt/frei,
- Betriebssystem: Name, Version, Release, Revision, Architektur/Build,
- GPU: Modell, Vendor-/Device-ID, dedizierter und geteilter Speicher, Treiberversion,
- tatsächliche OBS-Encoder-IDs für HEVC und H.264.

Die Daten werden über `GET|PUT|DELETE /v1/me/twitch/native-2k-hardware` verwaltet. Sie sind keine Zugangsdaten. Uplink validiert sie beim Speichern und unmittelbar vor jedem GoLive-Aufruf erneut. Für `native_2k` werden gegenüber Twitch ausschließlich HEVC und H.264 beworben; AV1 wird nicht angeboten.

## Freigaberegeln

`native_2k` startet nur, wenn **alle** Bedingungen erfüllt sind:

1. Das Twitch-Ziel ist ausdrücklich auf `twitch_output_mode = "native_2k"` gestellt.
2. Die aktuelle Twitch-Verbindung/Generation ist für Publish bestätigt.
3. Ein gültiges Quellrechner-Hardwareprofil ist gespeichert.
4. Der gemessene Eingang ist exakt 2560×1440@60 HEVC, 8-Bit YUV420/BT.709.
5. Der Server hat `libx264` tatsächlich initialisiert.
6. Das konfigurierte Enhanced-Limit erlaubt mindestens fünf Videospuren und genügend Aggregatbitrate für eine 2K-Leiter.
7. Das für Native 2K reservierte Kapazitätsbudget ist frei.
8. Twitch liefert genau eine 2560×1440@60-HEVC-Topspur mit dem angegebenen Quellencoder; alle weiteren Querformatspuren sind H.264 und höchstens 1920×1080@60.

Fehlt eine Bedingung, wird das Twitch-Ziel sichtbar blockiert. Der Modus fällt **nicht** still auf 1080p zurück und kodiert **nicht** AV1/H.264 zu HEVC um.

## Mediengraph

Für eine passende GoLive-Antwort gilt:

```text
OBS HEVC 2560×1440@60 ──────────────┐
                                      ├─ copy → Twitch HEVC 1440p60
                                      ├─ decode/scale/x264 → Twitch 1080p60
                                      ├─ decode/scale/x264 → Twitch 720p60
                                      └─ decode/scale/x264 → Twitch 360p30
OBS AAC Live ─────────────────────────── copy → Twitch Live-Audio
OBS AAC VOD  ─────────────────────────── copy → Twitch VOD-Audio
```

Die HEVC-Topspur erzeugt **keine** `EncodeProfile`-Gruppe. H.264-Unterstufen teilen denselben Decoder und übernehmen den von Twitch gelieferten B-Frame-Vertrag. Bei `bf > 0` wird x264 nicht gleichzeitig mit `zerolatency` gestartet.

## Kapazität

Das Deployment verwendet getrennte Budgets: `native_2k_units` für HEVC-Passthrough und `native_2k_av1_units` für den experimentellen AV1-Eingang. Die aktuelle Produktivkonfiguration setzt `native_2k_av1_units = 0`; AV1→Twitch-2K kann damit nicht versehentlich live starten.

Die frühere Servermessung HEVC-1440p dekodieren plus 1080p60/720p60/360p30 mit x264 lag oberhalb Echtzeit. Der erneute AV1-Test am 15./16. September 2026 zeigt dieselbe Grenze noch deutlicher: AV1-Decoding allein schafft etwa 8–10× Echtzeit, der vollständige Pfad AV1 1440p60 → HEVC 1440p60 + H.264 1080p/720p/360p erreicht mit x265 `fast` nur etwa 0,27–0,42×. Selbst x265 `ultrafast` erreicht für die vollständige Leiter nur etwa 0,81–0,83×. Nur die einzelne AV1→HEVC-Topspur erreicht mit 8 Threads und `ultrafast` knapp 1,01× und hat damit keine belastbare Reserve. Auf diesem CPU-Host bleibt AV1-2K deshalb gesperrt.

Der Modus `native_2k_av1` ist trotzdem als expliziter Versuchspfad modelliert: OBS liefert 2560×1440@60 AV1, Uplink handelt mit Twitch weiterhin eine HEVC/H.264-Leiter aus und würde die 1440p-HEVC-Spur selbst erzeugen. Die Quellhost-Hardwaredaten werden dabei genauso an Twitch weitergegeben wie beim HEVC-Passthrough. Eine spätere Freigabe braucht eine neue Lastmessung auf der tatsächlich eingesetzten Serverhardware und ein eigenes positives Kapazitätsbudget.

## Noch echter Live-Nachweis

Lokale Tests können GoLive-Schema, Routing, HEVC-Passthrough, H.264-Encode-Graph, Audiozuordnung und Kapazitätsgrenzen beweisen. Offen bleibt bis zu einem kontrollierten Twitch-Livestream, ob Twitch eine GoLive-Leiter, die anhand der Quell-GPU (z. B. AMF) ausgehandelt wurde, akzeptiert, wenn die niedrigeren H.264-Bitstreams auf dem Uplink-Server mit `libx264` erzeugt werden. Uplink zeigt deshalb tatsächliche Serverencoder im Laufzeitgraph und behauptet vor diesem Live-Test keine Twitch-2K-Produktionsfreigabe.
