# Plattform- und Mediennachweis

Arbeitsstand vom 8. September 2026. Die folgenden lokalen Proben ersetzen weder OBS- noch Plattform- oder Kapazitätsabnahmen.

## Ausführbare lokale Strecke

```sh
cargo run -j2 -p uplink-media --example media_probe -- \
  --ffmpeg /absoluter/pfad/ffmpeg --ffprobe /absoluter/pfad/ffprobe
```

Getestet mit FFmpeg `n8.1.2-50-g1a748fe2cd-20260831`; SHA-256 des Binaries: `ad7a8c8e8fe4f50972f32f63705cfcc57f44cd3531f57aa8defe388372242f5e`. Der Pfad unter `/opt/rs-relay/ffmpeg8/releases/` ist ausschließlich der vorhandene Testbuild. Die Engine verlangt einen expliziten eigenständig bereitgestellten FFmpeg-/FFprobe-Pfad.

Die Probe erzeugt Medien und TLS-Schlüssel im Speicher, bindet ausschließlich Loopback und verwendet den nativen Rust-Pusher. Empfänger messen komprimierte Nutzdaten und Zeitstempel. Quellen, Aufgaben und Fremdprozesse sind begrenzt und werden beim Abbruch beendet. Es werden keine Plattformzugänge benötigt.

| Tatsächlich geprüfter Weg | Ergebnis |
| --- | --- |
| Rust-RTMP-Pusher → unabhängiger FFmpeg-Empfänger | 3 Spuren, 240 identische komprimierte Pakete, 0 ms Versatz |
| H.264, AV1 oder HEVC-Eingang → H.264 256×144/25 → zwei RTMPS-Ziele | Ein Decode, ein gemeinsamer Encode, jeweils 50 identische Videopakete sowie 95 Live- und 95 VOD-Audiopakete |
| HEVC-Eingang mit 2560×1440/25 | Synthetisches SDR, gültige H.264-Ausgänge und getrennte Audiomischungen |
| H.264-Eingang → HEVC bzw. AV1 256×144/25 | Je ein gemeinsamer Encode, 50 Videopakete, beide AAC-Mischungen erhalten |
| Mehrvideo in einer RTMPS-Verbindung | Video-IDs 0/5 und Audio-IDs 2/9; Querformat und Hochkant aus einem Quellbild |
| Zweites Ziel mit gleichem Querformatprofil | Derselbe komprimierte Videobitstrom; unabhängig gewählter VOD-Ton |
| Fehlende benötigte VOD-Spur | Nur das betroffene Ziel schlägt vor seiner Verbindung fehl |
| Langsamer Empfänger bzw. stiller Handshake | Gesunde Ziele erhalten und beenden ihre Medien unabhängig |
| Audio-IDs 7/12, vertauschte Header, um 137 ms verschobene gesamte Quelle | Korrekte Mischungen; Video-DTS/PTS und Audio-DTS ohne zusätzlichen gegenseitigen Versatz |

Die vollständige Probe verlangt identische Paketanzahlen und einen gemeinsamen konstanten Zeitversatz; sie verwendet keine Toleranz für zusätzliche Audio-/Video-Verschiebungen. Die Mehrvideo-Probe zählt zwei Encodes und einen Decode und prüft alle vier Header. Sie ist ausdrücklich kein Beleg einer erfolgreichen Twitch-Publikation.

## Fähigkeiten und Quellenmessung

Die Hardwareaufnahme intersectiert CPU-Datensätze mit der tatsächlich erlaubten CPU-Menge und startet die Softwareencoder mit synthetischen Bildern. Der Testhost weist 16 sichtbare physische/logische Kerne und 48 GiB RAM aus. libx264, libx265 und libsvtav1 wurden tatsächlich initialisiert; eine GPU wurde nicht nachgewiesen. Das ist keine Echtzeit- oder Streamerzahlzusage. SVT `lp=2` bezeichnet eine Parallelitätsstufe, nicht exakt zwei Betriebssystemthreads.

Die Medienvorprüfung misst Codec, Maße, Bildrate, Pixelformat, bekannte Farbangaben und Audioeigenschaften. Unbekannte Werte bleiben unbekannt. Im getesteten AV1-/HEVC-FLV-Weg liefert FFprobe Matrix BT.709 und begrenzten Wertebereich, aber keine bestätigten Primärfarben bzw. Transferfunktion. Ein vollständiges angefordertes BT.709-Profil wird dadurch nicht automatisch freigegeben. Der Mehrvideo-Nachweis verwendet deshalb einen tatsächlich vollständig erkannten H.264-BT.709-Eingang. CBR und eine dauerhafte GOP-Grenze werden nicht aus einem kurzen Vorlauf erfunden. Vollständig geprüfter H.264-/AV1-Passthrough bleibt ein eigener offener Nachweis.

## Twitch-Konfiguration

`platform::twitch::GoLiveClient` fragt ausschließlich den offiziellen Konfigurationsendpunkt ab. Der Request verwendet den eigenen Clientnamen, reale Hostdaten und tatsächlich initialisierte Codecs. Bitraten des GoLive-Vertrags sind **kbit/s**. Antworttexte, Konfigurations-IDs, Endpoints und Authentifizierung werden nicht ungefiltert ausgegeben. Temporäre Antwortzugänge bleiben im RAM; Weiterleitungen und Proxys sind deaktiviert, Größen und Fristen begrenzt.

Die ausführbare autorisierte Kontoprobe liegt in `uplink-platform-probe`. Sie verwendet den gemeinsamen Infisical-Leser, die vorhandene verschlüsselte Zielablage und den bestehenden Plattformbroker. Eine Konfigurationsabfrage startet keinen Stream. Der erste Bestandslauf hielt vor Datenbank/Broker/Twitch am falschen Hex-Format des gemeinsamen Schlüssellesers an; der Bestandsvertrag verlangt Standard-Base64 mit genau 32 dekodierten Bytes. Nach dem gemeinsamen Loaderfix wurden das genau einmal vorhandene aktive Twitch-Ziel und dessen freigeschalteter Besitzer geprüft und der Zugang mit bestehender AAD entschlüsselt.

Der reale Chat-/Identitätsbroker antwortete mit 404. Sein bestehender Handler `platform_token.rs` verlangt `user:read:chat`; die Antwort beweist daher keine ungültige Publishauth. Für die ausdrücklich autorisierte reine Konfigurationsabfrage wurde der Status separat als `not_confirmed_http_404` und die Identitätsbestätigung als `null` ausgewiesen. Abweichende bestätigte Identitäten, 401/403/409 und Netzwerkfehler bleiben Abbruchgründe. Es wurde weder Chat freigegeben noch ein anderer Tokenstore gelesen.

Die anschließende reale Twitch-Abfrage lieferte HTTP 200 mit Status `error`, leere Video-/Audio-Konfigurationen und einen RTMPS-Endpunkt. Der eigene CPU-Client hatte 1440p60 und Hochkant60 mit 20.000 kbit/s Gesamtbudget sowie VOD-Audio angefragt. Aus dem Statushinweis wurden ausschließlich feste Suchbegriffe `client`, `gpu`, `required` und `information` ausgegeben; kein Roh-HTML, URL oder Zugang. Auch mit explizitem `null` für unbekannte optionale GPU-/Gaming-Fähigkeiten blieb die Antwort gleich. Dies ist ein konkreter negativer Konfigurationsnachweis für diesen Request, noch kein allgemeiner Nachweis über sämtliche Twitch-Konten oder Serverhardware. Eine positive 1440p-Freigabe oder Twitch-Publikation liegt nicht vor.

Die gegenwärtige ausführbare Übersetzung akzeptiert eng geprüfte `obs_x264`-Konfigurationen. Fremde Hardwareencoder werden nicht auf Software umgebogen. Video-Wire-IDs stammen aus der Reihenfolge der Antwort; Canvas-Indizes bleiben davon getrennt. Der geprüfte OBS-Code vergibt Audioausgangsindizes fortlaufend aus Live- und danach VOD-Konfigurationen. Davon abweichende Antwort-IDs werden vor einer Publikation sichtbar abgewiesen. Der native Medienrouter kann unabhängig davon bereits beliebige nachgewiesene Wire-IDs transportieren.

Quellenstand:

- [OBS GoLive-Request](https://github.com/obsproject/obs-studio/blob/6b3e550729f125b6c5b3767df88c08f5aef9d264/frontend/utility/GoLiveAPI_PostData.cpp), [Antwortmodell](https://github.com/obsproject/obs-studio/blob/6b3e550729f125b6c5b3767df88c08f5aef9d264/frontend/utility/models/multitrack-video.hpp), [Ausgangszuordnung](https://github.com/obsproject/obs-studio/blob/6b3e550729f125b6c5b3767df88c08f5aef9d264/frontend/utility/MultitrackVideoOutput.cpp).
- [Enhanced RTMP v2](https://veovera.org/docs/enhanced/enhanced-rtmp-v2): Tracksignalisierung, hvc1/avc1/av01, signierter Kompositionszeitversatz.
- [FFmpeg-SVT-Anbindung](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/libsvtav1.c) und [SVT-AV1-Parameter](https://gitlab.com/AOMediaCodec/SVT-AV1/-/blob/master/Docs/Parameters.md): CBR-Anbindung, Level, geschlossene GOP und Parallelitätsstufe.
