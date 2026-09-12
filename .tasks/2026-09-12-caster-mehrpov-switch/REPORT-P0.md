# Phase 0 Last-Spike: Compositor-Encode auf dem jetzigen Host

Stand: 12. September 2026. Gemessen auf dem Produktions-Host (16 Kerne, kein GPU,
1blu-Container) mit laufendem Bot/Dashboard/Postgres. Material: echtes Deadlock-Gameplay
`v2869791220.mp4` (h264, 1920x1080, 60 fps). Aufbau: zwei Quellen dekodieren (Gameplay +
Cam-Overlay 480x270 unten rechts), zusammensetzen, libx264 CBR 6000k, 30 s, Faktor gegen
Echtzeit ueber Wanduhr. Das ist EIN Program-Encode ohne jede Vorschau-Last.

| Variante | Faktor Echtzeit |
| --- | --- |
| 1080p60 veryfast | 0,78x |
| 1080p60 ultrafast | 0,99x |
| 1080p30 veryfast | 1,04x |
| 720p60 veryfast | 0,91x |

## Urteil

Der jetzige Host schafft in Software nicht einmal einen einzelnen Compositor-Encode mit
verlaesslicher Reserve. Alle Varianten liegen um 1,0x, mehrere sogar darunter. Da fuer die
Regie zusaetzlich mehrere POV-Eingaenge geforwardet und die Programm-Quellen dekodiert werden
muessen, gibt es kein Kopfpolster. Fuer echtes Casting ist der jetzige Host in Software raus.

Die in ffmpeg gelisteten `h264_nvenc`, `av1_nvenc`, `*_qsv`, `*_vaapi` sind nur einkompiliert;
der Container hat keine GPU, sie scheitern zur Laufzeit.

## Folge

Der Mischer braucht Hardware-Encode, also einen GPU-Server. Das passt zum ohnehin geplanten
eigenen Restream-/Uplink-Server. Der Bau bleibt host-agnostisch (austauschbarer Encoder-Pfad),
sodass er ohne Neuentwicklung auf den GPU-Server faellt. Der jetzige Host taugt nur als
Funktions-/Integrations-Testumgebung fuer 1 POV bei niedriger Aufloesung, nicht fuer Live-Casts.
