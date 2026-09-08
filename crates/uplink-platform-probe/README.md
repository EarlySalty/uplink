# Begrenzte Plattformprobe

Dieses Rustprogramm startet keinen Stream und schreibt keine Datenbankdaten.
`--hardware-only --ffmpeg /absoluter/ffmpeg --ffprobe /absoluter/ffprobe`
misst ausschließlich öffentliche Linuxwerte und je acht synthetische Bilder
mit libx264, libx265 und libsvtav1. Das Ergebnis ist ein Initialisierungsnachweis,
keine Echtzeit-, GPU-, HDR- oder Plattformfreigabe.

Der autorisierte Kontotest benötigt zusätzlich `--credential-fd 3` und
`--infisical-metadata /home/nathanael/.config/deadlock-twitch-bot/infisical.conf`.
Vorher ohne Credential-FD bauen; danach nur das fertige Binary mit dem bereits
geöffneten Credential-FD starten. Das Credential darf nicht an Cargo oder dessen
Buildprozesse vererbt werden. Der bestehende Secretloader setzt vor dem ersten
Encoderprozess auch am originalen FD `CLOEXEC`.

Der Kontotest verwendet ausschließlich den vorhandenen Infisical-Fetchvertrag,
genau ein aktives Twitch-Ziel eines freigeschalteten Besitzers, die bestehende
v2-Entschlüsselung mit `destination:{streamer_id}:twitch` und die autoritative
Brokeridentität. Eine abweichende bestätigte Plattform-ID führt zum Abbruch.
Der Bestandsbroker verlangt Chat-Rechte; sein HTTP 404 wird deshalb ausdrücklich
als unbestätigte Identität ausgewiesen (`null`). Die separat autorisierte reine
GoLive-Abfrage darf dann den eindeutig zugeordneten Publishzugang verwenden.
401/403/409, andere Antworten und Transportfehler werden nicht übergangen.
Ausgabe und Fehler enthalten keine IDs, Tokens, Schlüssel oder Endpunkte.
Das verschlüsselte Ziel und die Zugangswerte bleiben ausschließlich im Speicher.

Die ausdrückliche Probeanfrage ist 2560×1440/60 plus 1080×1920/60, Live-/VOD-Audio,
höchstens acht Videospuren und 20.000 kbit/s Gesamtbudget. Die Einheit folgt dem
gepinnten OBS-Stand `6b3e550729f125b6c5b3767df88c08f5aef9d264`:
[OBSBasicSettings.ui](https://github.com/obsproject/obs-studio/blob/6b3e550729f125b6c5b3767df88c08f5aef9d264/frontend/forms/OBSBasicSettings.ui),
[BasicOutputHandler.cpp](https://github.com/obsproject/obs-studio/blob/6b3e550729f125b6c5b3767df88c08f5aef9d264/frontend/utility/BasicOutputHandler.cpp)
und [GoLiveAPI_PostData.cpp](https://github.com/obsproject/obs-studio/blob/6b3e550729f125b6c5b3767df88c08f5aef9d264/frontend/utility/GoLiveAPI_PostData.cpp).
Diese Parameter sind eine Anfrage, keine gemessene Serverkapazität.

Die lokale Hardwareprobe greift weder auf Infisical noch Datenbank, Broker oder
Twitch zu. Nachweise und Grenzen der tatsächlich ausgeführten Kontoprobe stehen
in [Plattform-Medien](../../docs/plattform-medien.md).
