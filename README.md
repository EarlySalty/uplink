# Uplink

Sauberer Rust-Neubau in einem eigenständigen Repository. Grundlage ist der Produktvertrag v0.3 vom 8. September 2026. Der alte Medienkern und seine Git-Historie werden nicht übernommen.

## Geplanter AV1-/Upload-Spar-Test

[Was wollte ich testen, und wie ist es gebaut?](docs/av1-uplink-test-gedaechtnis.md) hält den persönlichen OBS→Uplink→Twitch-Versuch dauerhaft fest: 1080p-Sparmodus versus AV1-1440p-Lasttest, Quellhardware, vorhandene Implementierung, offene Aktivierung, Testanleitung, Messung und Rückkehr. Einstieg für Nutzer und spätere Agenten; den datierten Stand vor Aktivierung neu prüfen.

## Was bereits läuft

`uplink-core` plant deklarierte Videoausgaben anhand vollständiger Profile, Quellspuren und ausdrücklich angegebener Fähigkeiten. Identische Videoausgaben derselben Sessiongeneration werden gruppiert; verschiedene Audio-Routen teilen weiterhin denselben Video-Encode. Die Planung prüft Decoder, Encoder, Layoutrevisionen, Profilgrenzen und die Anzahl notwendiger Video-Encodes. Sie reserviert noch keine realen Ressourcen.

Videoprofile enthalten Auflösung, rational normalisierte Bildrate, Codec, Profil/Level, Bit-Tiefe, Chroma, Farbsignal, Rate-Control-Budget und GOP. Originalvideo wird nur bei vollständiger Übereinstimmung ohne Layoutänderung zum Kopieren eingeplant. Fehlende Audio-Rollen, unbekannte Fähigkeiten und unzulässige Ausgaben bleiben sichtbar; gespeicherte Wünsche werden nicht herabgesetzt. Fehlender Eingang bleibt ausstehend. Live- und VOD-Audiospuren werden separat zugeordnet, ohne stillen Ersatz.

Eine begrenzte FIFO nimmt komprimierte Pakete je Nutzer, Sessiongeneration und Spur auf. Bytezahl, Paketanzahl, Ankunftsalter und monotone Decodierzeitstempel werden kontrolliert. Rückstau wird als Fehler zurückgegeben, abgelaufene Pakete werden gezählt. Sie ist noch kein GOP-Cache, kein Wartebild-Übergang und kein Stream-Delay. Ein Worker muss den Ablauf auch bei inaktiven Puffern über `expire` auslösen und nach Paketverlust einen gültigen Wiedereinstieg organisieren.

`uplink-cli` liest ausschließlich eine explizit angegebene lokale TOML-Datei und zeigt den daraus abgeleiteten Szenarioplan. Sie öffnet keine Ports, verbindet keine Konten und startet keine Streams.

`uplink-ingest` nimmt lokal echte RTMPS-Verbindungen über Rustls und begrenztes Scuffle-RTMP an. Der [FFmpeg-8-Nachweis](docs/rtmps-nachweis.md) erhält AV1 beziehungsweise H.264 mit zwei AAC-Spuren, komprimierten Nutzdaten und Zeitstempeln. Autorisierung und TLS-Konfiguration werden injiziert; die API bindet ausschließlich Loopback. Das ist noch kein öffentlicher OBS-Eingang oder Plattformausgang.

## Lokal ausführen

Benötigt wird die in `rust-toolchain.toml` festgelegte Rustup-Toolchain 1.97.1 mit Clippy und Rustfmt. `Cargo.lock` ist versioniert.

```sh
cargo run --locked -p uplink-cli -- plan --config config/plan-beispiel.toml
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
```

Das Beispiel deklariert zwei Ausgänge mit gemeinsamem H.264-Video und verschiedenen Audiomischungen. Erwartet wird ein Video-Encode. Die Codec-, Bitraten-, Fähigkeits- und Limitwerte sind ausschließlich Testdaten, keine freigegebene OBS-/Plattformmatrix, Encoder-Voreinstellung oder Qualitätszusage. `allowed_*` beschreibt nur die angenommene Zielmatrix des lokalen Szenarios.

Exitcodes: `0` vollständig geplant, `2` ungültiger Aufruf oder Konfiguration, `3` mindestens ein Ausgang abgelehnt, `4` Eingang für mindestens einen Ausgang ausstehend. Die Konfiguration ist auf 256 KiB begrenzt; unbekannte Felder/Referenzen werden abgelehnt. Fehlermeldungen geben den Konfigurationstext nicht wieder. Lokale Varianten können als `config/local*.toml` abgelegt werden und werden nicht eingecheckt. In Konfigurationen gehören keine Secrets. Es gibt keinen Konfigurationsweg über Umgebungsvariablen.

## Was noch nicht gebaut oder abgenommen ist

Die Planung ist kein ausführbarer Mediengraph. Die angegebenen Fähigkeiten müssen künftig aus reproduzierbaren Proben und autorisierten Plattformadaptern stammen. Profile/Level werden bisher syntaktisch geprüft; zulässige Codec-Kombinationen, Skalierung/Farbkonvertierung und Crop-Detailqualität benötigen einen Mediennachweis. Die modellierten Codecs, HDR-Werte und Audioformate sind keine Produktfreigabe.

Öffentliche Ingest-/Brokeranbindung, Standard-OBS, vollständige Codecvalidierung und Encoder, echte Plattformausgänge für Twitch/Kick/YouTube/TikTok, Twitch Enhanced Broadcasting mit korrekten Audiorollen, Hochkant-Rendering, Layouteditor, Delay, Wartebild, Plattform-Reconnect, Live-Monitoring und Lastnachweise sind noch offen. Ebenso offen sind Konten-/Chat-/Dock-Anbindung sowie VOD-Quelle, Speicher, Uploadworker und Migration/Cutover. Bestehende Browseroberflächen dürfen nach Prüfung weiterverwendet werden; das bedeutet keine Übernahme des alten Rust-Medienkerns.

Die offenen Produktentscheidungen F1–F6 werden durch die aktuelle Modellierung nicht festgelegt. Insbesondere folgt aus einem abgelehnten Startplan bei fehlendem VOD-Ton keine Abschaltregel für bereits laufende Streams. Es gibt noch keinen neuen Dienst, der produktiv gestartet oder anstelle des bestehenden Relays eingesetzt werden könnte.

## Tests

Die Vertragstests decken unter anderem Video-Sharing trotz verschiedener Audiomischungen, vollständige Copy-Prüfung, fehlende VOD-Spur, getrennte Decoder-/Layoutfähigkeiten, Sessionisolation, GOP-/Layoutabweichungen, unbekannte Eingänge, Kapazitätsgrenzen und Pufferfehler ab. CLI-Tests führen das echte Binary mit gültigen, abgelehnten, unbekannten und übergroßen Eingaben aus. Das ist ein lokaler Softwaretest; alle Live-Abnahmen aus dem Produktvertrag bleiben auszuweisen.

Ingesttests prüfen zusätzlich echte lokale TLS-/RTMP-Verbindungen, Autorisierungsabweisung, neue Verbindungsgenerationen, Metadaten-/Trackgrenzen, begrenzte Consumer und getrennte Endgründe. Der externe [FFmpeg-Aufruf](docs/rtmps-nachweis.md#externer-ffmpeg-nachweis) benötigt ausdrücklich FFmpeg 8; seine TLS-Hostnameprüfung ist nur für die gemessene DNS-URL belegt.
