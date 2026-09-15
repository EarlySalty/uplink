# Uplink Cast Studio

## Ziel

Mehrere autorisierte POV-/Kameraquellen senden gleichzeitig zu Uplink. Ein Operator sieht alle Quellen als Vorschau, ordnet sie Szenen zu und schaltet genau eine Programmszene auf den bestehenden Twitch-Ausgang. Nicht ausgewählte Quellen dürfen keinen normalen Plattform-Encode starten.

## Festgelegte Produktsemantik

- Zwei, drei oder mehr Quellen pro Streamer sind erlaubt, begrenzt durch die bestehende Session- und Kapazitätskonfiguration.
- Jede Quelle erhält einen eigenen widerrufbaren Uplink-Streamschlüssel. Der normale Einzelstream-Schlüssel bleibt unverändert.
- Eine Szene referenziert zunächst genau eine POV-/Kameraquelle. Das Datenmodell bleibt für spätere Kompositionen erweiterbar.
- `Program` ist die ausgewählte Live-Szene. `Preview` ist die nächste Szene, die der Operator vorbereitet. Preview und Program sind getrennte Zustände.
- Nur Program darf einen normalen Plattformausgang beanspruchen. Standby-Quellen werden angenommen und beobachtet, aber nicht an Twitch/Kick/YouTube/TikTok weitergereicht.
- Ein Wechsel ist generationsgesichert. Ein veralteter Browser darf eine neuere Programmauswahl nicht überschreiben.
- Der erste Bauabschnitt erfindet keinen nahtlosen Medienwechsel: Steuerung, Quellenidentität, Standby-Semantik und Dashboard werden zuerst beweisbar gemacht. Ein echter unterbrechungsfreier Program-Mux braucht einen eigenen Mediennachweis.

## Vorschau-Architektur

Die ressourcensparende Zielarchitektur ist zweistufig:

1. Wenn der Browser den Eingangscodec direkt dekodieren kann, wird der komprimierte Videostrom nur umverpackt und im Operator-Browser dekodiert. Das vermeidet einen zusätzlichen Server-Encode.
2. Für inkompatible Codecs darf ein begrenzter, nur bei aktiver Operator-Ansicht laufender Low-FPS-/Low-Resolution-Previewworker verwendet werden. Kein dauerhafter Vollbild-Encode für jede Standby-Quelle.

Dieser Task legt die Steuer-/API-Verträge dafür an. Ein JPEG-/WebRTC-/MSE-Fallback wird nicht als fertig behauptet, solange kein echter Browser-Mediennachweis existiert.

## Bauabschnitte

1. Persistenz für Cast-Quellen, Szenen und Studiozustand.
2. Eigene Source-Keys und Ingest-Authentisierung mit Source-ID.
3. Registry-/Sessionstatus um Source-ID und Standby/Program-Rolle erweitern.
4. API für Quellen, Szenen, Preview- und Program-Auswahl.
5. Standby-Session darf nicht den normalen Plattformgraph starten.
6. Twitch-Dashboard-Seite `Uplink Studio` mit Program/Preview, Quellenraster, Szenenliste und Quellschlüsseln.
7. Tests, rustfmt/clippy/test, Frontend-Tests.
8. Danach separater Medien-Slice für echten persistenten Program-Mux und Browser-Previews.

## Nicht vortäuschen

- Kein angeblich nahtloser Twitch-Schnitt, bevor Zeitstempel, Codecheader, Audio und Reconnect live bewiesen sind.
- Kein VDO.Ninja-Klon per Browserkamera ohne WHIP/WebRTC-Nachweis.
- Keine dauernden Preview-Encodes für jede Quelle.
