# Lokaler Rust-Eingang über RTMPS

Stand: 8. September 2026. `uplink-ingest` nimmt tatsächlich TCP-/TLS-Verbindungen
an und verarbeitet RTMP mit einer lokal begrenzten Scuffle-Serverimplementierung.
Der Eingang ist über seine API ausschließlich an IPv4-Loopback bindbar. Es gibt
keine neue öffentliche Adresse, keinen Dienstwechsel und keinen Plattformstream.

## Schnittstelle und genaue Aussage

`IngestServer::bind_loopback` bekommt Port, `rustls::ServerConfig`, einen
asynchronen `Authorizer` und explizite `IngestLimits`. Der Autorisierer entscheidet
beim Publish anhand der nur im Prozess übergebenen App-/Streamkennung und liefert
eine autoritative Nutzer-/Sessionidentität. Der Eingang speichert weder Tokens
noch eine zweite Kontenverwaltung. Brokeranbindung und öffentliche Authentifizierung
sind noch nicht abgenommen.

Jede angenommene Verbindung erhält eine neue, serverseitige Generation: eine
zufällige 128-Bit-Instanzkennung plus monotonen, gegen Überlauf abgesicherten
Verbindungszähler. Der Autorisierer kann diese Generation nicht wiederverwenden.
Trackschlüssel bestehen aus Nutzer-/Sessionidentität, Generation, Medienart und
Wire-Tracknummer. Video 0 und Audio 0 sind verschiedene Spuren. Live-/VOD-Rollen
werden nicht aus der Tracknummer erfunden.

`MediaEvent` erhält den vollständigen kompakten Medienbody, die separaten
Nutzbytes, DTS, PTS, Transport-Codec, Ereignisart und Headerrevision. Erkannte
Formen sind altes AAC auf Track 0, erweitertes AAC OneTrack mit `mp4a`, altes AVC
und erweitertes AV1 auf Video-Track 0. AVC-Kompositionszeiten werden vorzeichenrichtig
aus 24 Bit gelesen. Andere Mehrspurformen, Codecs und ModEx sind nicht freigegeben.
AAC-MultichannelConfig wird als eigenes Metadatenereignis erhalten; geprüft ist
die native Kanalreihenfolge mit genau sechs Nutzbytes, gültiger Sprecherbitmaske
und übereinstimmender Kanalzahl, gemäß
[Enhanced RTMP V2](https://veovera.org/docs/enhanced/enhanced-rtmp-v2).
Andere Reihenfolgeformen werden abgelehnt.
Zeitstempel-Wrap wird noch nicht rekonstruiert; sinkende DTS innerhalb einer
laufenden Trackkonfiguration werden abgelehnt.

Die Medienköpfe werden direkt und mit Längenprüfung gelesen. Es gibt keinen
Aufruf des zuvor auffällig gewordenen `scuffle-flv`-/SPS-/AVCC-Codecparsers.
SequenceHeader und komprimierte Bilder bleiben opaque: Empfang ist kein Nachweis
gültiger Codec-Konfiguration, Auflösung, Bildrate, HDR oder Decodierbarkeit.
Ein künstlicher AVC-Header mit Profil 100 wird deshalb nur als Bytes transportiert;
er wird nicht durch den verwundbaren Extended-SPS-Pfad geschickt. Diese Daten
sind absichtlich kein vollständiges `uplink-core::VideoProfile`.

## Grenzen und Ende einer Verbindung

Die lokalen Prüflimits erlauben vier belegte Verbindungen, 64 KiB je
RTMP-Mediennachricht, 16 KiB je AMF-Nachricht, vier unvollständige Nachrichten
mit zusammen höchstens 256 KiB reservierter Länge, acht Tracks und 4 KiB
SequenceHeader-Nutzdaten. Pro Verbindung sind höchstens 512 ausstehende
Medienereignisse mit zusammen 1 MiB Bodybytes erlaubt. Die Werte sind
konfigurierbare technische Prüflimits, keine freigegebenen Produktprofile.

Byte- und Ereignisbudgets zählen auch bereits an einen Consumer übergebene,
noch nicht freigegebene Events. Ihre Bodies sind kompakte `Box<[u8]>` und halten
keine möglicherweise größere Scuffle-Readbuffer-Allocation fest. Auch der
Verbindungsslot bleibt bis zum letzten Event belegt. Damit kann ein Consumer
die Grenze nicht durch viele bereits beendete, aber noch gehaltene Sessions
umgehen. Parserpuffer, TLS und Objektverwaltung haben zusätzlich begrenzte bzw.
bibliothekseigene Allokationen; die Eventbytezahl ist kein gemessener Gesamt-Heap.
`max_queued_bytes` ist die beobachtete Belegung des Eventbudgets.

Die Abgabe verwendet ausschließlich `try_send` und sofortige Budgetreservierung.
Volle Budgets oder ein voller Kanal beenden die Session mit `Backpressure`, ein
geschlossener Consumer mit `ConsumerClosed`; kein unbeschränktes Warten auf einen
langsamen Verbraucher. Fehlende Header, zu viele Tracks und sinkende Zeitstempel
werden separat abgewiesen. Ein expliziter Stop schließt die Verbindung; ein
weiterer Publish braucht eine neue Generation.

Auch Metadaten vor dem ersten SequenceHeader belegen einen Track; Revision 0
kennzeichnet dabei den fehlenden Header. Sie erlauben keine Frames und erben
keine Headerrevision eines anderen Codecs. Nach SequenceEnd braucht der Track
erneut einen SequenceHeader. Vier Transportregressionen sichern diese Grenzen;
drei davon schlugen vor dem Reviewfix tatsächlich fehl.

Eine absolute Frist läuft ab TCP-Annahme über TLS, RTMP und die vollständige
Autorisierung. Danach gilt ein eigener Medien-Inaktivitätstimer. Die zusätzlich
begrenzten Scuffle-Read-/Write-/Handlerfristen sind Protokollfristen und werden
als `ProtocolTimeout` ausgewiesen. `PeerClosed`, `ExplicitStop`, `TlsRejected`,
`StartTimeout`, `MediaTimeout` und Autorisierungs-/Medienabweisung bleiben getrennt.
Nach außen gelangen feste Fehlercodes, keine App-/Publishnamen, AMF-Inhalte oder
ungefilterten Bibliotheksfehler.

## Portable Tests

Der gezielte Lauf bestand mit 22 Tests: sieben reine Wirekopf-Prüfungen
und 15 echte Loopback-TLS-/RTMP-Tests. Zusammen mit den bisherigen 24 Core-/CLI-
Tests und drei Beispieltests bestanden 49 Workspace-Tests mit `--all-targets`.
Ingest und Beispiel wurden auch im Releaseprofil geprüft. Geprüft wurden:

- unbekannter Hostname und fremdes Zertifikat werden abgelehnt;
- Publishabweisung liefert keine Medien;
- zwei Verbindungen derselben autorisierten Session bekommen verschiedene
  Generationen und erhalten Header, Framebytes, PTS und DTS;
- EOF, expliziter Stop, fehlende TLS-Daten, Medien-Inaktivität und hängende
  Autorisierung haben unterscheidbare, begrenzte Enden;
- Byte-/Ereignisgrenze, Trackgrenze und fehlender Header werden eingehalten;
- AAC-MultichannelConfig bleibt von Medienframes getrennt; unvollständige,
  widersprüchliche und nicht freigegebene Kanalbeschreibungen werden abgelehnt;
- nach beendeter Session bleiben Slot und Budgets bis zum letzten gehaltenen
  Event belegt und werden danach freigegeben.

Die Zertifikate werden für die Tests frisch im Prozess erzeugt. Private Schlüssel
werden weder aus einer Datei gelesen noch in eine Datei geschrieben oder ausgegeben.
Die Rust-TLS-Clients verwenden eine explizite Root-CA und Hostnamenprüfung.

```sh
cargo fmt -p uplink-ingest -- --check
cargo clippy --workspace --all-targets --locked -j2 -- -D warnings
cargo test --workspace --all-targets --locked -j2
cargo test -p uplink-ingest --release --locked -j2
cargo test -p uplink-ingest --example rtmps_probe --locked -j2
cargo test -p uplink-ingest --example rtmps_probe --release --locked -j2
cargo build --workspace --release --locked -j2
```

Die gezielten RTMP-/AMF-Vendortests werden zusätzlich in CI ausgeführt. Zwei
geerbte RTMP-Upstreamtests benötigen nicht mitgelieferte Monorepo-Testressourcen
und bleiben ausdrücklich ignoriert; sie zählen nicht als bestanden. Sie sind
kein Ersatz für den separaten echten FFmpeg-RTMPS-Nachweis.

## Externer FFmpeg-Nachweis

Das separat ausführbare Beispiel `rtmps_probe` wird gegen das ausdrücklich
angegebene vorhandene FFmpeg 8 und die kleinen versionierten künstlichen
AV1-/H.264-plus-zwei-AAC-Fixtures gemessen. Es ist keine still übersprungene
Pflichtprüfung in `cargo test`. Ein fehlender Binaryparameter oder FFmpeg 6
beendet den Aufruf mit Fehler; diese Fälle werden nicht als bestandene
Medienprüfung ausgegeben.

Der abschließende Lauf nach dem Metadata-Reviewfix mit
`n8.1.2-50-g1a748fe2cd-20260831` bestätigte für
beide Codecs jeweils 240 komprimierte Medienpakete und drei SequenceHeader
auf drei Spuren. Sämtliche Nutzbytes und Header stimmen nach Größe und SHA-256
mit den versionierten FFprobe-Referenzen überein. DTS und PTS stimmen für
jedes Paket überein; die erlaubte gemeinsame Zeitverschiebung beträgt im
gemessenen Lauf **0 ms**. Beide Verbindungen endeten mit `ExplicitStop` und
bekamen trotz derselben autorisierten Session verschiedene Generationen.

Container-Metadaten werden separat geprüft. Auf Audio-Track 1 sendet FFmpeg
die native Mono-Kanalbeschreibung `01 01 00 00 00 04`. Die AV1-ColorInfo wächst
beim RTMP-Remux gegenüber dem FLV-Quellobjekt von 38 auf 62 Nutzbytes:
FFmpeg ergänzt `matrixCoefficients=0`. Die Probe erwartet exakt dieses gemessene
Objekt; daraus folgt kein geprüfter Farbraum oder HDR-Nachweis.

| Eingang | Video / Audio 0 / Audio 1 | Header / Metadaten / SequenceEnd | Events / Bodybytes | Beobachtetes Eventbudget-Maximum |
| --- | --- | --- | --- | --- |
| AV1 + zwei AAC | 50 / 95 / 95 Pakete | 3 / 2 / 0 | 245 / 59.665 Bytes | 9.640 Bytes |
| H.264 + zwei AAC | 50 / 95 / 95 Pakete | 3 / 1 / 1 | 245 / 141.520 Bytes | 23.527 Bytes |

Die Tabelle zeigt den Debug-Nachlauf. Der abschließende Release-Aufruf bestand
ebenfalls vollständig; seine Budgetspitzen lagen bei 8.004 beziehungsweise
8.025 Bytes. Die Spitzenwerte hängen vom Scheduling ab; sie sind kein
Kapazitätsbenchmark. Der [native Reviewer](review-rtmps.md) wiederholte den
echten Nachweis auf dem korrigierten Librarypfad unabhängig erfolgreich.
Der [separate Sicherheitsreview](review-security-rtmps.md) bestätigt den finalen
lokalen Baustein mit 59 gezielten Tests, drei Audits und Gitleaks; Gate und
Merge werden separat nach tatsächlicher Ausführung festgehalten.

Der Aufruf verwendet `rtmps://localhost:<port>/live/probe`, `tls_verify=1` und
eine explizite öffentliche Test-CA. Falsche CA sowie eine vertraute CA mit einem
Zertifikat nur für `wrong.invalid` wurden jeweils abgewiesen: FFmpeg Exit 251,
`TlsRejected`, null Publish-Autorisierungen und null Medienereignisse. Der
gesamte Probeaufruf endet nach positiven und negativen Prüfungen mit Exit 0.

**Grenze des geprüften FFmpeg-Builds:** Bei einer numerischen URL mit
`127.0.0.1` wurde eine echte SAN-Abweichung trotz `tls_verify=1` angenommen;
die strenge Probe schlug dabei korrekt fehl. Im
[genau verwendeten OpenSSL-Pfad](https://raw.githubusercontent.com/FFmpeg/FFmpeg/1a748fe2cd/libavformat/tls_openssl.c)
wird die Hostnamenprüfung für numerische Hosts ausgelassen. Deshalb ist nur
der gemessene DNS-Hostnameweg belegt; keine allgemeine FFmpeg-TLS-Freigabe und
keine Abschwächung der Zertifikatsprüfung.

```sh
cargo run -j2 -p uplink-ingest --example rtmps_probe --locked -- \
  --ffmpeg /absoluter/pfad/zu/ffmpeg8
```

Verwendetes Binary: `/opt/rs-relay/ffmpeg8/releases/c733b4b2951e5957e15505f788b2c65a7a41b6da4b289e295852cc38079b4d2b/ffmpeg`.
SHA-256: `ad7a8c8e8fe4f50972f32f63705cfcc57f44cd3531f57aa8defe388372242f5e`.
Der Pfad ist ein expliziter lokaler Testparameter, keine Repository-Laufzeitabhängigkeit.

## Weiter offen

OBS-Versionen/-Betriebssysteme, echte Live-/VOD-Audiozuordnung, öffentliche
Broker-/Ingestauthentifizierung, Keyrotation, lange Sessions, Zeitstempel-Wrap,
vollständige Codecvalidierung, HEVC und andere E-RTMP-Formen, Decoder/Encoder,
vier Plattformausgänge, Wartebild, Delay und Betrieb unter gemessener Last.
Dieser lokale Eingang ist ein weiterer ausführbarer Baustein des Neubaus und
keine Abnahme der vollständigen Produktstrecke.
