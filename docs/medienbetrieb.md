# Ausführbare Medienkette

Stand: 8. September 2026. Lokaler Integrationsnachweis, keine Plattform- oder Produktionsfreigabe.

`uplink-media` nimmt autorisierte `MediaEvent`s aus `uplink-ingest` entgegen. `prepare_and_start` untersucht einen begrenzten Vorlauf mit FFprobe und liefert `SourceObservation`. Fehlende Farbangaben, Rate-Control und GOP bleiben unbekannt. Die deklarative `start(SessionSpec)`-Schnittstelle ist davon getrennt; ihre Eingangseigenschaften müssen der Aufrufer nachweisen.

Ein FFmpeg-Prozess pro Session dekodiert die einzelne Eingangs-Videospur. Ein Filtergraph erzeugt einen Encode je identischem Ausgabeprofil und Layout. Jede Ausgabe gelangt über einen privaten Unix-Socket mit überprüfter Prozessidentität zurück nach Rust. Native RTMP(S)-Pusher verwenden die begrenzten Scuffle-Chunk-/AMF-Bausteine. Jeder Zielpuffer besitzt eigene Byte- und Ereignisgrenzen; zurückgehaltene und gerade geschriebene Pakete zählen mit. Unterschiedliche AAC-Auswahlen teilen den Videoencode.

Publish-Zugänge bleiben in Rust-RAM und werden redigiert dargestellt. FFmpeg/FFprobe bekommen weder Zugangsdaten noch Environment-Konfiguration. Ihre Standardfehlerausgaben werden verworfen. Binär- und Laufzeitpfade kommen ausdrücklich aus der Dienstkonfiguration. Der Laufzeitordner benötigt Modus 0700. Es werden nur selbst erzeugte Socketdateien und eigene leere Unterordner entfernt.

Reproduktion mit dem ausdrücklich ausgewählten Testbuild:

```sh
/home/nathanael/.cargo/bin/cargo test -j2 -p uplink-media
/home/nathanael/.cargo/bin/cargo clippy -j2 -p uplink-media --all-targets -- -D warnings
/home/nathanael/.cargo/bin/cargo run -j2 -p uplink-media --example media_probe -- \
  --ffmpeg /opt/rs-relay/ffmpeg8/releases/c733b4b2951e5957e15505f788b2c65a7a41b6da4b289e295852cc38079b4d2b/ffmpeg \
  --ffprobe /opt/rs-relay/ffmpeg8/releases/c733b4b2951e5957e15505f788b2c65a7a41b6da4b289e295852cc38079b4d2b/ffprobe
```

Dieser Pfad bezeichnet ausschließlich den vorhandenen lokalen Testbuild `n8.1.2-50-g1a748fe2cd-20260831`; er wird kein Laufzeitdefault und ist keine Abhängigkeit auf das alte Repository.

Bereits gemessen: H.264 320×180/25 mit zwei AAC-Feeds über echten RTMPS-Eingang, ein Decode und ein H.264-Encode 256×144, zwei echte RTMPS-Ausgänge. Beide Empfänger erhielten 50 identische Videopakete. Live und VOD enthielten jeweils die 95 richtigen AAC-Pakete; das zweite Ziel erhielt ausschließlich den ausgewählten VOD-Mix. Native Pusher-Tests prüfen zusätzlich AV1/H.264 mit zwei AAC unverändert, CA-/Hostnamefehler vor Autorisierung, Publish-Ablehnung, ACK-/Bandbreitenkontrolle, Schließen und Abbruch.

Das ältere synthetische AV1-Fixture enthält `yuv420p` mit `color_space=gbr`. Der neue gemessene Encodepfad weist diesen widersprüchlichen Stand ab. Sein bisheriger Transport-/Passthrough-Nachweis behauptet keine richtige Farbinterpretation.

Ein zusätzlich im Prozess erzeugtes synthetisches AV1 mit ausdrücklich gesetzter BT.709-Matrix durchlief dieselbe vollständige Kette erfolgreich: 50 identische Videoframes an beiden Zielen und die beiden unveränderten AAC-Auswahlen. Ein weiterer Lauf mit absichtlich blockiertem drittem Empfänger ließ die beiden anderen Ausgänge vollständig weiterlaufen. Die Probe prüft ihre Werte aus den tatsächlich empfangenen Paketen und schlägt bei Abweichungen fehl.

Die strikte Zeitlinienregression fand zunächst Video +19 ms bei unverändertem Audio: Der FPS-Filter rundete den Videoursprung von 21 auf 40 ms. Der korrigierte Filter resampelt relativ zum ersten echten Keyframe-PTS und stellt dessen gemessenen Ursprung bei Millisekunden-Encoderzeitbasis wieder her. Danach hatten alle vier gekoppelten Fälle exakt 0 ms gemeinsamen DTS-/PTS-Versatz, einschließlich vertauschter Audioheader, Wire-IDs 7/12 und zusätzlicher 137-ms-Verschiebung sämtlicher Quell-DTS. Ein unabhängiger FFmpeg-RTMP-Empfänger bestätigte zusätzlich drei Spuren und 240 identische komprimierte Pakete mit 0 ms Versatz. Das ist kein Nachweis für beliebige andere Bildraten oder Decoder-/Encoderkombinationen.

Eine echte Startup-Regression wies zusätzlich blockierende serielle Zielstarts nach. `RunningPusher::spawn` legt jetzt die begrenzte Zielqueue vor DNS/TLS/RTMP an; Encoder, Medienkonsum und andere Ziele warten darauf nicht. Auch der Abschluss läuft pro Ziel unabhängig. Im lokalen Nachlauf erhielt ein gesundes Ziel nach 109 ms sein erstes Videopaket; beide gesunden Ziele waren nach 121 ms vollständig beendet, während das stumme erste Ziel weiter im Handshake stand. Die 15-s-Startfrist dieses Ziels wurde nicht abgewartet. Kurze Streams dürfen mit `finish` bereits angenommene Header und Medien noch innerhalb der Abschlussfrist nach einem verzögerten Publish vollständig ausliefern; Fristablauf endet sichtbar mit Fehler und bestätigter Transportfreigabe. `stop`/`cancel` bleiben davon getrennt. Vier zusätzliche native Tests prüfen diese Fälle einschließlich einer vollen Startupqueue.

Noch offen für den vollständigen Produktumfang: kontogeprüfte Twitch-GoLive-Konfiguration und Mehrvideo-Zuordnung innerhalb einer Plattformsession, generelle Profil-/Passthrough-Freigaben aus vollständigen Nachweisen, HEVC-Eingang, Audio-Transcoding, normalisierte Layoutbedienung, Delay-/Reconnect-/Wartebildsemantik sowie Aufzeichnung/VOD. Der derzeitige gemessene Startpfad erlaubt passende AV1/H.264-SDR-Eingänge, AAC mit 48 kHz und ein oder zwei Kanälen sowie begrenzte H.264-Ausgaben. Unbekannte Quelleigenschaften erzeugen kein automatisch freigegebenes Passthrough.

Protokollgrundlagen: [Enhanced RTMP V2](https://veovera.org/docs/enhanced/enhanced-rtmp-v2), [FFmpeg-Filter und Streamcopy](https://ffmpeg.org/ffmpeg.html), [FFmpeg-Unix-Protokoll](https://ffmpeg.org/ffmpeg-protocols.html#unix). Dokumentierte Protokollmöglichkeiten ersetzen die obigen Ausführungsnachweise nicht.

Bei einer Quell-Audiospur bekommen Twitch-Livestream und Twitch-VOD dieselbe Spur (Wire 0). Bei zwei oder mehr Quellspuren erhält der Livestream Wire 0 und das Twitch-VOD Wire 1; weitere Spuren werden ignoriert, andere Plattformen erhalten immer Wire 0. Über den derzeit freigegebenen RTMPS-Eingang kann heute keine zweite Audiospur ankommen, deshalb bekommt das Twitch-VOD dort denselben Ton wie der Livestream.

Gespeicherte Ausgabewünsche werden vor dem Start auf die Grenzen aus `relay.platform_caps` begrenzt, anschließend auf die tatsächlich mögliche Quellgröße, Bildrate und Engine-Grenzen. Der laufende Zielstatus nennt das begrenzte Profil und den Grund. Die gewünschte Einstellung bleibt gespeichert; ein zu großer Wunsch beendet den Stream nicht.
