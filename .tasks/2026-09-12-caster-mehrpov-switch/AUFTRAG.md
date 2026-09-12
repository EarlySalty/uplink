# Caster-Mischer: Mehr-POV-Switching ueber Uplink

Stand: 12. September 2026. Auftraggeber Nani. Orchestrierung Hauptsession, Bau ueber die Pyramide.

## Ziel in einem Satz

Mehrere Leute (Caster, Spieler, Kameras) streamen gleichzeitig zum Server; ein Regisseur
sieht alle als Live-Vorschau, waehlt Szenen wie in OBS (mit oder ohne Cam), und der Server
rendert nur die aktive Szene und schickt genau eine Program-Ausgabe an einen Twitch-Kanal.

## Nutzer-Entscheidungen (12.09., verbindlich)

1. Ingest der POVs: WebRTC/WHIP. Kameras nativ wie vdo.ninja ohne Room; OBS kann per WHIP rausstreamen.
2. Vorschau: alle POVs sollen als Live-Vorschau sichtbar sein, nur die Programm-Szene wird voll encodet.
3. Szenen wie in OBS: Szenen mit Cam ueber Gameplay und Szenen ohne Cam, umschaltbar zur Laufzeit.
4. Host: laeuft auf der vorhandenen Uplink-Infrastruktur (gleiche Kiste). Siehe Last-Gate unten.

## Harte Realitaets-Leitplanken

- Compositing (dekodieren, zusammensetzen, neu encoden) ist keine Relay-Arbeit. Die CPU-Kiste
  haelt einen 1080p60-Encode, nicht drei. Deshalb: genau EIN Program-Encode zur Zeit, Vorschau
  darf keinen zweiten Voll-Encode je Quelle erzwingen.
- Empfohlene Vorschau-Zustellung (nutzt die WHIP-Wahl): die niedrige Simulcast-Schicht jeder
  WebRTC-Quelle wird per SFU-Forward direkt an den Regisseur-Browser gereicht. Server-seitiges
  Dekodieren nur fuer die Quellen der aktiven Szene (Program). Das ist ressourcensparender als
  serverseitiges Dekodieren aller Quellen und liefert trotzdem ein fluessiges Vorschaubild.
  Falls ein Sender kein Simulcast liefert, faellt diese Quelle auf niedrig aufgeloestes
  Vorschau-Forwarding zurueck, nie auf einen zweiten Voll-Encode.
- Program-Latenz Ziel unter 2 s bei 2-s-Keyframe, wie beim bestehenden Uplink.
- Ein laufender Program-Ausgang darf beim Szenenwechsel nie abreissen (nahtloser Cut).
- Encoder-Pfad von Anfang an austauschbar (Software x264 jetzt, HW/GPU spaeter ohne Umbau),
  damit ein Umzug auf einen GPU-Server keine Neuentwicklung ist.

## Wiederverwendung (Bestandssuche erledigt)

Repo `~/repos/uplink`. Nicht neu bauen, sondern anbinden:

- `uplink-ingest` (RTMPS via scuffle): Muster fuer authentifizierten Ingest, Sessiongeneration.
  WHIP kommt als zweiter Ingest-Weg daneben, nicht als Ersatz.
- `uplink-media` (`engine.rs`, `graph.rs`, `pusher.rs`, `queue.rs`): Mediengraph und Plattform-Pusher.
  Program-Ausgang nutzt den bestehenden Pusher fuer den einen Twitch-Ausgang.
- `uplink-core`: deklarative Encode-Gruppen, Kompositionslayout, `shared_video_decode_count`.
  Szenen-Layouts hier abbilden, kein zweites Layout-Modell.
- `uplink-service` (`api.rs`, `store.rs`, `crypto.rs`, DB `deadlock` Schema `relay`): API auf
  127.0.0.1:8891, X-Relay-Auth, Bot proxied. Neue Routen additiv, keine zweite Tokenablage.
- Dashboard: `Deadlock-Twitch-Bot/bot/dashboard_v2/src/pages/Uplink.tsx` als Anbindungsmuster.
  Regisseur-Oberflaeche ist eine neue Seite in derselben Shell, kein zweites Frontend.
- Secrets: bestehende `RS_RELAY_*` aus Infisical, kein neues Secret ohne Not.

## Neues Subsystem (das eigentlich Neue)

1. WHIP-Ingest (WebRTC-HTTP Ingest): oeffentlicher Endpunkt je Show/POV-Slot, authentifiziert
   ueber Ingest-Key analog `relay.users`. Nimmt Kamera-Browser und OBS-WHIP an. Simulcast lesen.
2. Show-/Produktionsmodell: eine "Show" besitzt N Eingangs-Slots (POVs) plus Szenen. Neue Tabellen
   additiv in Schema `relay` (shows, show_inputs, show_scenes, scene_sources). Genau ein aktiver
   Program-Ausgang je Show.
3. Preview-Bus: SFU-Forward der niedrigen Simulcast-Schichten an den Regisseur-Browser.
4. Program-Bus: Compositor rendert die aktive Szene (Layout aus `uplink-core`), einmal encodet,
   an den Twitch-Pusher. Szenenwechsel = Umschalten der Compositor-Eingaenge ohne Encoder-Neustart.
5. Szenen-Modell wie OBS: Szene = geordnete Quellen + Layout (Vollbild-POV, Cam-ueber-Gameplay,
   feste Karten wie Starting/BRB). Zur Laufzeit anlegen, bearbeiten, umschalten.
6. Regisseur-Dashboard: Vorschau-Raster aller POVs, Program-Monitor, Szenen-Buttons, Cut/Swap.

## Phasen

- Phase 0 (Last-Spike, PFLICHT zuerst): auf genau dieser Kiste messen, wie viele WHIP-Eingaenge
  gleichzeitig dekodiert/geforwardet werden koennen und ob ein Program-Compositor-Encode
  (Cam ueber Gameplay, 1080p) neben 2-3 Preview-Forwards haelt. Ergebnis entscheidet, ob und wie
  viele POVs realistisch sind und ob der GPU-Server noetig wird. Zahlen in REPORT-P0.md.
- Phase 1: WHIP-Ingest + Show-Modell + ein einziger Program-Cut zwischen zwei Vollbild-POVs, live an Twitch.
- Phase 2: Szenen mit Layout (Cam ueber Gameplay, Szene ohne Cam), Vorschau-Raster, Regisseur-UI.
- Phase 3: mehr POVs, feste Karten, Feinschliff, Live-Nachweis bei einem echten Turnier-Cast.

## Offene Punkte fuer spaeter (nicht Phase 0/1)

- Audio-Routing: welche POV liefert den Program-Ton, Mischung mehrerer Mikros, Discord-Ton.
- WHIP-TLS-Bereitstellung fuer den oeffentlichen Eingang (Caddy ist HTTP-Proxy, kein Medien-L4).
- Bildgestaltung der Cam-ueber-Gameplay-Layouts (Groesse, Position, Rahmen) entscheidet Nani.
