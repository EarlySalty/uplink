# AV1-Uplink: Was wollte ich testen, und wie ist es gebaut?

Stand der Übergabe: **18. September 2026**. Diese Datei hält den Nutzerauftrag und die Fortsetzung für spätere Agenten fest. Den tatsächlichen Release-, Konfigurations- und Kontostand vor jeder Aktivierung neu prüfen. Ein Repository-Commit ist kein Nachweis eines laufenden Deployments.

## Antwort auf „Was wollte ich noch mal testen?“

Der Nutzer möchte von seinem eigenen Streaming-PC mit **AMD Radeon RX 7900 XT** über seinen eigenen verbundenen Twitch-Account testen, ob ein einzelner hochwertiger AV1-Stream zu Uplink den schwankenden Heimupload entlastet. Uplink soll daraus die von Twitch benötigten Ausgabeformate und Enhanced-Broadcasting-Qualitätsstufen erzeugen. Zusätzlich zum getrennten 1080p-Sparmodus will er ausdrücklich **AV1 2560×1440 bei 60 fps → serverseitiges HEVC → Twitch-1440p** ausprobieren und dabei die echte Serverlast messen. Twitch soll bei der GoLive-Aushandlung die gespeicherten Hardwaredaten seines Streaming-PCs erhalten, nicht ersatzweise die Serverhardware.

Er hat Enhanced Broadcasting bereits mit zwei Grafikkarten betrieben; nach seiner Aussage sind PC und Encoderleistung nicht das Hauptproblem, sondern die an manchen Tagen schwankende Uploadleitung. Nicht erneut nach dem grundsätzlichen Ziel oder der bekannten GPU fragen. Die konkrete numerische Konto-ID wurde in dieser Arbeit nicht bestimmt und darf nicht geraten werden.

## Drei getrennte Wege, nicht drei Namen für dasselbe

| Zweck / gespeicherter Twitch-Modus | PC → Uplink | Uplink → Twitch |
| --- | --- | --- |
| Normaler 1080p-Upload-Sparmodus: `enhanced` | Eine 1920×1080@60-AV1-Videospur | Angeforderte H.264-Enhanced-Leiter; kein natives Twitch-1440p in diesem Pfad |
| 1440p-Referenz ohne HEVC-Neuencode: `native_2k` | Eine 2560×1440@60-HEVC-Videospur | Passende HEVC-Topspur kopieren; angeforderte kleinere H.264-Stufen erzeugen |
| Persönlicher AV1-1440p-Lasttest: `native_2k_av1` | Eine 2560×1440@60-AV1-Videospur | AV1 decodieren, HEVC-Topspur und angeforderte kleinere H.264-Stufen auf dem Server erzeugen |

„2K“ meint in diesem Auftrag **2560×1440**, nicht 2048×1080. AV1-Eingang bei Uplink ist nicht dasselbe wie AV1-Ausgabe zu Twitch. Die hier implementierte Native-2K-Aushandlung bietet HEVC/H.264 an.

```text
Streaming-PC / OBS                         Uplink                         Twitch
AV1-Hardwareencode ── eine Videospur ──> AV1-Decoding ── HEVC 1440p60 ──> Topspur
                                             └──── H.264-Unterstufen ──> GoLive-Leiter
AAC Live + AAC VOD ───────────────────── getrennt weiterreichen ────────> Live / VOD

Quell-PC-Profil aus OBS-Log ───────────── GoLive-Anfrage mit Quellhardware
Server-Encode und Serverlast ─────────── separat messen und ausweisen
```

**Nicht fünf Videospuren vom PC hochladen.** Der Nutzer nannte zunächst fünf Spuren; die uploadsparende Architektur erzeugt diese erst auf dem Server. Anzahl, Auflösungen, Bildraten und Bitraten bestimmt die tatsächliche GoLive-Antwort des verbundenen Kontos, keine fest erfundene Fünferleiter. Auch ein Vertrag mit nur einer HEVC-Topspur ist lokal getestet; dass Twitch ihn diesem Konto liefert, ist damit nicht bewiesen.

**Bitraten nicht verwechseln:** 25 Mbit/s entsprechen 25.000 kbit/s, nicht 25 kbit/s. Ein mit 25 Mbit/s eingestellter AV1-CBR-Stream braucht nicht allein wegen AV1 weniger Upload. Für die Einsparung muss bei vergleichbarer Bildqualität ein niedrigeres Eingangsbudget getestet werden. Heimupload, Audio/Transportanteil und gesamte Server→Twitch-Bitrate getrennt protokollieren. Die genannte 25-Mbit-Zahl ist kein freigegebenes Heimupload-Preset. HQCBR ist eine Encoder-Einstellung, kein zusätzlicher Transportcodec und keine garantierte Prozentersparnis.

## Hardwareprofil: Was wird weitergegeben?

Im Dashboard des Twitch-Bots wird das aktuelle OBS-Log lokal im Browser analysiert. Der implementierte Ablauf speichert ein vollständig erkanntes normalisiertes Quellprofil; das OBS-Log selbst soll nicht hochgeladen werden. Vor dem Test den tatsächlich gespeicherten Status prüfen, nicht lediglich die Dateiauswahl als Erfolg werten.

Das Profil enthält CPU, RAM, Betriebssystem, GPU-Liste mit Modell, Vendor-/Device-ID, Speicher und Treiberversion sowie OBS-HEVC-/H.264-Encoder-IDs. Verwaltung im Uplink-Dienst: `GET|PUT|DELETE /v1/me/twitch/native-2k-hardware`. Die GoLive-Anfrage übernimmt die Quellhardware; Regressionstests prüfen die vollständigen Felder und zwei GPUs.

Das macht die Radeon **nicht** zu einer GPU des Servers. Beim AV1-Test erzeugt der Server HEVC mit seinem tatsächlichen Encoder. Die Anfrage benennt den Client weiterhin als `uplink`. Hardwarewerte nicht erfinden, Twitch-Rechte nicht aus GPU-Daten ableiten. Die Akzeptanz der gesamten Hybridkette bleibt ein echter Twitch-Livenachweis.

## Gesicherter Entwicklungsstand dieser Übergabe

Die folgenden Stände wurden im Repository nachgesehen; Produktionsdienste wurden für diese Dokumentationsrunde nicht geprüft oder verändert.

| Bestandteil | Nachweis / Stand |
| --- | --- |
| Uplink-Basis auf `main` vor dieser Dokumentationsänderung | `f2acc17`: AV1-1080p-Enhanced; davor `56bf084`: experimentelles AV1-Native-2K; `9d91a18`: HEVC-Native-2K |
| Kontogebundener AV1-2K-Test | Branch `feat/av1-2k-scoped-test`, Implementierung `9e06ba4`, letzter Übergabestand `a754a09`; auf `origin` vorhanden, **noch nicht nach main gemergt** |
| Hardware-Autospeicherung im Dashboard | Im vorherigen Arbeitsstand `92f19d74bf8d9bf59ebf9c7d75986c4844ad1ecb`; weiterer UI-Stand `34109420` im Worktree `tb-uplink-native-2k-av1`. Den aktuellen Main- und Live-Stand im Twitch-Repository neu prüfen. |
| Tests des Featurestands | Laut vorhandener Aufgabenakte 158 bestanden, 0 fehlgeschlagen, 6 Medientests ignoriert; gezielte Native-2K-Auswahl 7 bestanden, nicht nochmals zu den 158 addieren. Clippy ohne Warnungen. In dieser Dokumentationsrunde nicht erneut ausgeführt. |
| Review, Release, Aktivierung | Kein belastbar erfolgreicher unabhängiger Merge-Gate-Nachweis dokumentiert. Feature-Release, Deploy, Konto-ID-Aktivierung und neuer OBS→Twitch-Test sind nicht als erledigt belegt. |

Die bisherige Aufgabenakte liegt auf dem Feature-Branch unter `.tasks/2026-09-18-av1-2k-scoped-test/EVIDENCE.md`. Zum Nachlesen ohne Branchwechsel:

```sh
git show origin/feat/av1-2k-scoped-test:.tasks/2026-09-18-av1-2k-scoped-test/EVIDENCE.md
git show origin/feat/av1-2k-scoped-test:docs/twitch-native-2k.md
```

**Bekannter Budgetfehler auf dem bisherigen main:** Die dortige Deployment-Vorlage fordert 100 zusätzliche AV1-1080p-Einheiten bei insgesamt 100 und bereits reservierter Basiseinheit. Das kann nicht zugelassen werden. Der Feature-Branch korrigiert dies auf **99 zusätzliche + 1 Basis** und ergänzt die Prüfung. Beim Release auch die tatsächlich installierte Konfiguration prüfen. Die Einheiten sind eine interne Zulassungsskala, keine CPU-Prozentwerte.

## Was die neue Testfreigabe tut — und was nicht

Erst im geprüften Feature-Release existiert unter `[media.enhanced]` die optionale Einstellung `native_2k_av1_test_streamer_id`. Sie erhält genau die numerische Uplink-`streamer_id` des autorisierten Kontos aus der bestehenden Kontozuordnung; nicht blind einen Kanalnamen oder eine andere Plattform-ID einsetzen.

- `native_2k_av1_units = 0` bleibt gesetzt: keine allgemeine Produktionsfreigabe. Die Test-ID ist in den ausgelieferten Vorlagen absichtlich ungesetzt.
- Nur dieses Konto **und** dessen ausdrücklich gewählter Modus `native_2k_av1` dürfen die Ausnahme nutzen. Hardware-, Quellen-, Audio-, Publish- und GoLive-Prüfungen bleiben erforderlich. Keine automatische Umstellung oder stille 1080p-Ersatzwahl.
- Der Test startet nur ohne andere aktive Uplink-Sessions und reserviert atomar das gesamte begrenzte Uplink-Budget. Er verdrängt keine laufenden Streams; weitere Sessions werden während des Tests abgewiesen. Nach Sessionende wird das Budget freigegeben.
- Das ist nur Exklusivität innerhalb des Uplink-Prozesses: **keine CPU-Isolation gegenüber anderen Serverdiensten, kein automatischer Zeit- oder Lastabbruch und kein dauerhaftes One-Shot-Limit**. Ein späterer Start des freigegebenen Kontos ist weiter möglich, solange die Einstellung besteht. Nach dem Versuch die Test-ID wieder entfernen.

Die Option niemals in einen alten Dienst eintragen: Unbekannte Konfigurationsfelder werden abgelehnt. Ein positiver globaler AV1-2K-Wert ist kein Ersatz für den kontogebundenen Test.

## Fortsetzung für den nächsten Agenten

1. Diese Übergabe lesen. Aktuelle Branches, Änderungen anderer Sessions, Release-SHAs, installierte Konfiguration und aktive Uplink-Sessions prüfen. Vorhandene Implementierung fortsetzen, nicht erneut einen Relay-Kern oder eine Tokenverwaltung bauen. Dokumentierte Testergebnisse nicht als eigene aktuelle Messung ausgeben.
2. `feat/av1-2k-scoped-test` prüfen, vorhandenen unabhängigen Rust-/Sicherheitsreview und Workspace-Gate korrekt ausführen, Fehler anhand der echten Ausgabe klären. Erst mit nötigen Nachweisen koordinierter Merge und nachvollziehbarer Release. Den laufenden Haupt-Checkout mit fremden Änderungen nicht zurücksetzen und keine alten Dashboard-Binaries als neuen Release ausgeben.
3. Dashboard-Stand mit lokaler OBS-Analyse, automatischer Hardwareübernahme und getrennten Moduswahlen prüfen. Alte Release-Aufträge oder vorhandene Build-Verzeichnisse nicht ungeprüft erneut deployen. Zuständigen Dienst ermitteln; die Uplink-Deployment-Vorlage verweist auf `rs-relay.service`, obwohl der neue Code im Repository `uplink` liegt. Nicht deshalb im alten `rs-relay`-Medienkern weiterbauen.
4. Eigenes autorisiertes Konto aus der bestehenden Anmeldung/Kontozuordnung auflösen, normales OBS-Profil und normale Zielwahl erhalten. Mit geprüftem Release ausschließlich dieses Testkonto freigeben. Aktive Streams bei Konfigurationswechsel/Neustart schützen. Kein bloßes Dokumentationsupdate rechtfertigt einen Dienstneustart.
5. Teststart, Messung und Rückkehr gemäß den folgenden Abschnitten begleiten. Erst tatsächliche Gesundheits-, Stream- und Plattformprüfungen belegen eine Aktivierung. Wenn ein Werkzeug fehlt oder blockiert, genau die offene Aktion dokumentieren; kein erfolgtes Deploy behaupten.

## Konkreter Testablauf für den Nutzer

**Erst beginnen, wenn der Agent die Aktivierung ausdrücklich überprüft hat.** Ein sichtbarer Modusschalter allein bedeutet nicht, dass der Server den Test schon zulässt.

1. Eigenes bisheriges OBS-Profil behalten. Separates Profil anlegen, beispielsweise `Uplink – AV1 1440p Test`. Das bestehende 1080p-Setup nicht überschreiben. Im Uplink-Bereich des Analyse-Dashboards ein frisches OBS-Log lokal auswerten und das gespeicherte Quellhardwareprofil kontrollieren.
2. Nur das eigene Twitch-Ziel für diesen Versuch aktivieren, keine weiteren Plattformziele und kein Hochkant. `native_2k_av1` wählen. Der Agent bestätigt die korrekte Konto-ID-Freigabe und dass kein anderer Uplink-Stream läuft. Der Versuch kann auf dem verbundenen Twitch-Kanal öffentlich live werden; er ist kein automatisch unsichtbarer Bandbreitentest.
3. Mit den im Dashboard angezeigten eigenen Uplink-Zugangsdaten streamen, nicht direkt an Twitch. **Eine AV1-Videospur, 2560×1440, exakt 60 fps, 8-Bit YUV420/NV12, BT.709, begrenzter Farbbereich.** Vorhandene getrennte AAC-/48-kHz-Mischungen für Live und VOD beibehalten: OBS-Spur 1 → Wire 0 → Live; OBS-Spur 2 → Wire 1 → VOD. Bei fehlender VOD-Spur nicht still Live-Ton als Ersatz verwenden.
4. AMD-Hardware-AV1 wählen, nicht den Software-AV1-Encoder. HQCBR nur verwenden, wenn die installierte OBS-/AMF-Kombination ihn tatsächlich anbietet und verträgt; andernfalls CBR und den Unterschied dokumentieren. Kein angeblich identisches Nvidia-Preset erfinden. Für 1440p ist hier **kein qualitätsgeprüftes Bitratenpreset freigegeben**. Eingangsbitrate passend zur gemessenen Leitung bewusst festlegen, mit Reserve für Audio/Transport, und protokollieren.
5. Mit reproduzierbarem bewegtem Spielmaterial beginnen. Agent und Nutzer prüfen tatsächliche Twitch-Qualitätsstufen, CPU/RAM, Rückstau, Verzögerung, Bildqualität und getrennten Ton. Bei wachsenden Puffern oder Störungen anderer Dienste den Versuch stoppen statt immer weitere Encoderthreads zu vergeben.

Der separate **1080p-Vergleichstest** nutzt `enhanced`, AV1 1920×1080@60, dokumentierten Startwert 5 Mbit/s Video und 2 Sekunden Keyframe-Intervall. Das ist ein Teststartwert, keine garantierte Qualitätsersparnis. Details: [Twitch Enhanced über AV1](twitch-enhanced-av1-uplink.md); dessen historische 100-Einheiten-Angabe ist durch die oben beschriebene Korrektur überholt.

## Was gemessen und als Ergebnis aufgeschrieben werden muss

Vor Beginn Ausgangslast des Servers und andere laufende Dienste festhalten. Dann Zeit, Dauer, Uplink-/Dashboard-SHA, OBS-/Treiber-Version, Quellhardware, gewähltes Profil, Bitrate/Rate-Control und Konto-Zuordnung ohne Secrets dokumentieren.

Während des Versuchs: tatsächliche Eingangsbitrate, Auflösung/Codec/Bildrate; GoLive-Leiter mit Spuren, Bitraten und tatsächlichen Serverencodern; CPU-/GPU-Last soweit vorhanden, RAM und ausgehende Bitrate; Verarbeitungsgeschwindigkeit, Pufferwachstum/Rückstau, verworfene Frames bzw. OBS-Netzwerkdrops, A/V-Synchronität und Ende-zu-Ende-Verzögerung. Die weitergereichte Quell-GPU nicht als gemessenen Serverencoder darstellen. Twitch-Livebild, verfügbare Qualitätsstufen und tatsächlichen VOD-Ton prüfen.

**Historische Datei-Benchmarks, keine neue Messung dieser Übergabe:** AV1-1440p-Decoding etwa 8–10× Echtzeit; vollständige Leiter mit x265 je nach damaliger Einstellung etwa 0,27–0,42×, selbst `ultrafast` etwa 0,81–0,83×; nur HEVC-Topspur mit 8 Threads und `ultrafast` knapp 1,01×. Der dokumentierte Engpass war der HEVC-Softwareencode, nicht AV1-Decoding. Quelle: [Native-2K-Dokumentation](twitch-native-2k.md). Ein Wert knapp über 1× ist keine belastbare Dauerlastreserve.

Noch zu beantworten: Akzeptiert Twitch die komplette Hybridkette auf diesem Konto? Liefert es eine volle Leiter oder nur die Topspur? Schafft die reale Serverkonfiguration die tatsächlich verlangte Leiter dauerhaft ohne wachsenden Rückstau? Wie viel Heimupload spart ein qualitätsvergleichbarer Eingang nach dem zusätzlichen Serverencode? Keine feste Prozentersparnis und keine Produktionsfreigabe aus einem erfolgreichen Verbindungsaufbau ableiten.

## Stoppen und zurück zum Normalbetrieb

OBS normal stoppen, Sessionende und Kapazitätsfreigabe kontrollieren. Die kontogebundene Test-ID aus der Dienstkonfiguration entfernen und den geprüften Konfigurationswechsel sicher aktivieren. Im eigenen Konto wieder den vorherigen Modus und das normale OBS-Profil wählen. `native_2k_av1_units` bleibt 0. Dienstgesundheit, normale Zulassung und gegebenenfalls einen normalen Teststart prüfen. Bei Release-Problemen auf den zuvor dokumentierten geprüften Release samt passender Konfiguration zurückkehren; keine Datenbank oder Schlüssel löschen.

## Wo ein Agent im Code suchen soll

| Bereich | Repository / Datei |
| --- | --- |
| Test-ID, Grenzen und additive Budgetprüfung | `uplink/crates/uplink-service/src/config.rs` |
| Moduswahl, Quelle, Hardwareprofil, GoLive und Testzulassung | `uplink/crates/uplink-service/src/media.rs` |
| Atomare Exklusivreservierung, Freigabe und Sessionstatus | `uplink/crates/uplink-service/src/registry.rs` |
| Twitch-Leiter und Audiozuordnung | `uplink/crates/uplink-service/src/media_output.rs` |
| Hardwareprofil-API | `uplink/crates/uplink-service/src/api.rs` |
| GoLive-Anfrage und Vertragsprüfung, Hardware-Regressionen | `uplink/crates/uplink-media/src/platform/twitch.rs`, `platform/twitch/tests.rs` |
| Mediengraph / Lastproben | `uplink/crates/uplink-media/src/graph.rs`, `examples/native_2k_av1_load_probe.rs`, `examples/enhanced_av1_1080_load_probe.rs` |
| Einstellungen / Tests | `uplink/deployment/uplink.toml`, `config/uplink-beispiel.toml`, `crates/uplink-service/tests/contracts.rs` |
| Dashboard und OBS-Analyse | `Deadlock-Twitch-Bot/bot/dashboard_v2/src/pages/UplinkEingang.tsx`, `UplinkNative2kHardware.tsx`, `UplinkOutputMode.tsx`, `src/uplinkEncoderAnalyse.ts`, `src/api/uplink.ts` |

Lokale Basis: `/home/nathanael/repos/uplink`. Zum Übergabezeitpunkt existiert der Feature-Worktree `/home/nathanael/repos/uplink-av1-2k-test-20260918`; für Dashboard-Arbeit der bisherige Worktree `/home/nathanael/repos/tb-uplink-native-2k-av1`. Worktrees können später verschwinden; Repository, Commit und Dokumentation sind die dauerhafte Referenz.

## Fortschritt bei jeder Fortsetzung aktualisieren

Nach Review, Merge, Deploy oder einem Testlauf hier Datum, genaue SHAs, Aktivierungsstatus, Ergebnis und nächsten offenen Schritt ergänzen. Laufprotokolle ohne Schlüssel oder rohe private OBS-Logs unter `docs/testberichte/` ablegen und von hier verlinken. Bis echte Ergebnisse vorliegen, bleibt der persönliche OBS→Uplink→Twitch-AV1-1440p-Test **offen**.

Der Nutzer soll später mit „Was wollte ich bei AV1/Uplink testen?“ diese Erklärung und den dann aktuellen nächsten Schritt bekommen, ohne den bisherigen Auftrag erneut rekonstruieren zu müssen.
