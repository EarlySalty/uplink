# Begrenzte RTMP-/AMF-Bibliotheken für den neuen Eingang

Stand: 8. September 2026. Lokale Quellableitungen veröffentlichter Scuffle-Crates.
Die Änderungen gehören zur echten Rust-Eingangsstrecke in `uplink-ingest`.
Es wird kein alter rs-relay-Medienkern übernommen. Dies ist keine Freigabe für
öffentlichen Betrieb, OBS-Versionen, Ausgabeprofile oder eine Plattform.

## Herkunft und Grenzen der Auswahl

| Verzeichnis | Crate | Originalarchiv SHA256 | Upstream-Revision |
|---|---|---|---|
| `third_party/scuffle-rtmp` | `scuffle-rtmp 0.2.3` | `8c2da6c55454996d5654c7f56fcf5db9c9b48a82ece219b22badebdb9782de0a` | `2445114cc326ca8862a72355db555fd0a7fa16f9` |
| `third_party/scuffle-amf` | `scuffle-amf0 0.2.4` | `2db0c6c36b3c0ace0a46d73193d800374b6152de0e3209d0953078bd8eaa4e27` | `1b7b6ec46bcfeb5b9569753ab94e467eb2ab0c04` |

Die Archive stammen von static.crates.io. `Cargo.toml.orig`, beide unveränderten
Lizenzen MIT/Apache-2.0 und lokale `PATCHES.md` erhalten Herkunft und Änderungen.
Jedes Vendor-Verzeichnis ist eigenständig baubar und besitzt ein Lockfile;
die Ingest-Anwendung verwendet dieselben lokalen Quellen. Der Scuffle-RTMP-
Adapter pinnt ausdrücklich `scuffle-amf0 =0.2.4`, statt still die im ursprünglichen
RTMP-Lockfile genannte 0.2.3 oder beliebige transitive Version zu nehmen.

Primärquellen: [RTMP 0.2.3](https://docs.rs/scuffle-rtmp/0.2.3/scuffle_rtmp/),
[AMF0 0.2.4](https://docs.rs/scuffle-amf0/0.2.4/scuffle_amf0/),
[Enhanced RTMP V2](https://veovera.org/docs/enhanced/enhanced-rtmp-v2).
Scuffles dokumentierte E-RTMP-Unterstützung ersetzt keinen Nachweis unserer
Medientags. Im Eingang bleiben Audio und Video rohe begrenzte Payloads;
`scuffle-flv` wird dort nicht als Codecparser eingebunden. Die separat gemessene
AVCC-/Exp-Golomb-Falle aus `scuffle-nachweis.md` wird dadurch nicht übernommen.

## Tatsächlicher Patch

`ChunkReader::with_limits(ChunkReaderLimits)` prüft Nachrichtenlänge,
AMF-Nachrichtenlänge und CSID-Anzahl nach dem Header und vor Payloadübernahme
oder Map-Eintrag. Für Teilnachrichten werden Anzahl und Summe der deklarierten
Längen vor der Reservierung geprüft. Das ist ein Budget angeforderter Payload-
Kapazitäten; Allocator-/Map-/TLS-Overhead wird damit nicht als gemessener RSS
verkauft. Abgeschlossene Teilnachrichten geben ihre Reservierung frei.
Über der erlaubten Framinggröße wird eine ChunkSize-Verhandlung abgewiesen,
nicht verändert: heimliches Kappen würde den Bytestrom falsch interpretieren.

Eine Fortsetzung darf keinen abweichenden logischen Header in derselben CSID
ersetzen. Ein identischer erneut übertragener vollständiger Header bleibt
zulässig. Längensubtraktion ist geprüft. RTMP-Zeitstempel werden ausdrücklich
modulo 2^32 addiert; Type 3 unterscheidet Fortsetzung und neue Nachricht mit
wiederverwendetem Delta. Gültig verschachtelte CSIDs behalten eigene Payloads.

`ServerSession::new(io, handler).with_limits(limits)?.run().await` nimmt einen
bereits aufgebauten generischen AsyncRead-/AsyncWrite-Transport entgegen.
`ServerSessionLimits` enthält `chunk`, `amf`, `max_read_buffer_bytes`,
`max_write_buffer_bytes`, `max_publishing_streams`, `max_messages_per_turn`,
`handshake_timeout`, `connect_timeout`, `publish_timeout`, `read_timeout`,
`write_timeout` und `handler_timeout`.

Die RTMP-Handshakefrist gilt absolut einschließlich Flush; C0/C1 benötigen
1537 Byte, C2 genau 1536. EOF beendet den Handshake nach dem ersten leeren Read.
Connect-/Publish-Fristen beginnen nach dem Handshake. TLS und die gemeinsame
Startfrist liegen beim Ingest-Aufrufer. Idle-, Callback- und Schreibfristen
bleiben eigene Grenzen. Schreiben umfasst ausdrücklich `write_all` und
`flush`, damit TLS-Puffer wirklich an den Transport gelangen. Ein Timeout wird
nicht länger als gewöhnliches Peer-Ende verschluckt. Nach einer begrenzten Zahl
vollständiger Nachrichten gibt die Verarbeitung an Tokio zurück.

Die Parser für SetChunkSize und WindowAcknowledgementSize erhalten vollständige
RTMP-Nachrichtenkörper und verlangen genau vier Byte. Fehlende oder zusätzliche
Bytes sind `InvalidData`. Sie ergeben kein Parser-`UnexpectedEof`, das sonst
als Transportende behandelt würde. `ServerSession::run()` gibt diesen
Protokollfehler weiter; die bestehende Ingest-Zuordnung ergibt `ProtocolRejected`.
Tatsächliches Transport-EOF und Leerlauf-Zeitüberschreitung bleiben unverändert.

Read- und Write-Puffer haben konfigurierbare Bytebudgets vor Wachstum.
ACK-Fenster null wird abgewiesen; Empfangsbestätigungen melden die tatsächlich
empfangene Bytezahl mit separatem Zähler seit dem letzten ACK und definiertem
Überlauf des 32-Bit-Protokollzählers. Audio/Video/AMF0-Metadaten werden erst für
autorisierte Publish-Streams an den Handler gegeben. Doppelte/ungültige IDs,
zu viele parallele Publish-Streams und ungültige Delete-Stream-IDs werden
abgewiesen. `HandlerRejected` enthält keine fremden Daten.

Nach einer gültigen ACK-Fensteränderung wird dieselbe Fälligkeitsprüfung wie
nach einer Änderung der empfangenen Bytezahl ausgeführt. Wird das Fenster unter
die bereits unbestätigt empfangene Menge abgesenkt, legt sie sofort genau ein
kumulatives ACK in den begrenzten Ausgabepuffer. Der vorhandene Flush nach der
Kontrollnachricht sendet es vor dem nächsten blockierenden Read. Größere Fenster
behalten unbestätigte Bytes; bereits bestätigte Bytes lösen kein weiteres ACK
aus. Eine Nullgröße wird vor jeder Zustandsänderung abgewiesen.

`MessageData::read_with_limits` und `Command::read_with_limits` verwenden
`DecodeLimits`. Auch optionale Argumente bekannter Commands werden budgetiert
validiert. Der AMF-Patch begrenzt Bytes, Strings/Schlüssel, tatsächliche
Containereinträge, Wertezahl und Verschachtelung in nativen und Serde-Pfaden.
ECMA-Anzahlen reservieren keinen Speicher; StrictArray-Längen werden vor
Reservierungen/Serde-Größenhinweisen geprüft. Vollständige Terminatoren,
Containerverbrauch und Fortschritt werden erzwungen. Details einschließlich
Einzelwert- versus inkrementeller API stehen in `third_party/scuffle-amf/PATCHES.md`.

Unbekannte Commands/Medien werden in Standardhooks nur mit numerischer
Stream-/Typinformation protokolliert. App-/Publishname, rohe AMF-Werte und
Payloads erscheinen dort nicht. `CommandError` redigiert AMF-Inhalte auch in
Display/Debug. Interne Fehlerquellen weiterhin nicht direkt protokollieren.

Die Bibliotheksstandardwerte sind keine Uplink-Produktpresets.
Die konkrete Eingangsanwendung setzt engere lokale Grenzen. Ihre Sessionzahl,
TLS-Konfiguration, Autorisierung und Eventwarteschlange sind zusätzlich nötig.
Ein Payload-Slice kann eine größere gemeinsame Read-Allokation festhalten;
`uplink-ingest` übernimmt deshalb kompakte Medienkopien innerhalb seiner
reservierten Eventbudgets. RTMP-Message-Stream-ID ist keine Audio-/Video-Track-ID.
Medienidentität besteht aus Sessiongeneration, Medienart und Wire-Tracknummer.

## Gemessene Regressionen

Vor den RTMP-Korrekturen liefen acht neue Tests gegen die unveränderte Logik:
Debug: 1 bestanden, 7 fehlgeschlagen, Exit 101; Release: 2 bestanden,
6 fehlgeschlagen, Exit 101. Geprüft wurden Headerersetzung während Partial,
Type-1-/Type-2-Überlauf, gültiges Interleaving, Flush, Handshake-EOF, ACK null
und Medien vor Publish. Type 2 panikte nur im Debugprofil; Release-Wrapping
war kein hinreichender Protokollnachweis. Ein zusätzlich zuerst roter ACK-Test
zeigte die veraltete Bytezahl vor dem Read statt der aktuellen Empfangssumme.

Nach dem Patch: RTMP **84 Bibliothekstests plus ein Doctest** sowohl Debug als
auch Release bestanden, Exit 0. Darunter neue Tests für präallokative Header-,
CSID-, Partialanzahl-/Summengrenzen, Freigabe von Reservierungen, Type-3-Delta,
ChunkSize-Ablehnung, absolute Ein-Byte-Slowloris-Frist, Idle/EOF-Unterschied,
C2-Länge, Flush-Frist, Ausgabebudget, AMF-Bombe vor Handler und Fehlerredaktion.
Clippy über alle Targets mit `-D warnings` sowie fmt bestanden.

Nachkorrektur gegen Stand `b6c18f55fd30b6fc0669e444de5eceeae2f99751`:
`uplink_short_set_chunk_size_is_protocol_error_while_transport_open` und
`uplink_short_ack_window_is_protocol_error_while_transport_open` senden nach
vollständigem RTMP-Handshake einen vollständig gerahmten, drei Byte langen
Body und halten den Duplex-Transport offen, bis `ServerSession::run()` endet.
Vor dem Fix lieferten beide `Ok(true)` statt eines Fehlers: Debug und Release
je zwei Fehler, Exit 101. Nach dem Fix ergeben beide `InvalidData`,
`is_client_closed()` ist falsch. Ein weiterer Test prüft für beide Parser
die ungültigen Längen 0–3 und 5–8; ein vollständiger Sitzungstest bestätigt
gültige vier Byte mit anschließendem echtem Transport-EOF als normales Ende.
Es werden weder Fehlertexte verglichen noch künstliche Timeouts als
Erkennung verwendet. Alle vier zusätzlichen Tests sowie die vorhandenen
EOF-/Timeout-Regressionen sind in der oben genannten grünen Anzahl enthalten.

ACK-Nachkorrektur gegen `e9147232b482a47a10673b16f500f043fb50e7b2`:
`uplink_ack_window_shrink_unblocks_waiting_peer_without_more_bytes` führt den
vollständigen RTMP-Handshake durch, senkt das ACK-Fenster auf 64 Byte und wartet
danach ohne weitere gesendete Bytes auf das ACK. Vor dem Fix endete der Read
mit `UnexpectedEof`, weil kein ACK vor dem nächsten serverseitigen Read kam;
Debug und Release je ein fehlgeschlagener Test, Exit 101. Tokio verwendet dabei
eine pausierte Testuhr; die bestehende Sessionfrist beendet den fehlerhaften
Ablauf, keine zusätzliche ACK- oder Timeout-Schleife wurde eingeführt.
Nach dem Fix kommt das ACK mit genau der empfangenen kumulativen Bytezahl an,
und der Peer beendet die Sitzung kontrolliert. Ein weiterer Zustandstest prüft
Fenstervergrößerung, Gleichheit am Grenzwert, Null-Abweisung ohne Mutation,
mehrfache Wechsel nach einem ACK sowie neue Bytes über den Wire-Zähler-Wrap.
Die frühere Read-/Wrap-Regression bleibt ebenfalls grün. Beide neuen Tests
sind in den 84 bestandenen Bibliothekstests enthalten.

AMF: zunächst neun neue Regressionen rot; danach **21 Grenztests plus
40 Bibliothekstests und ein Doctest** mit Serde in Debug/Release bestanden.
Ohne Serde elf Bibliothekstests und ein Doctest bestanden. fmt und Clippy mit
und ohne Serde bestanden. Keine unserer neuen Regressionen ist ignoriert.

Zwei ursprüngliche RTMP-Tests bleiben ausdrücklich **nicht ausgeführt**:
`test_basic_rtmp_clean` und `test_basic_rtmp_unclean`. Sie verlangen das nicht
im veröffentlichten Crate enthaltene Monorepo-Asset `../../assets/avc_aac.mp4`
sowie ein beliebiges `ffmpeg` im PATH. Dieses Asset wurde weder verifiziert noch
kopiert. Es ist nicht durch die vorhandenen E-FLV-Mehrspur-Fixtures austauschbar,
ohne die zugehörigen Legacy-Paketannahmen des Tests neu zu definieren. Die
neue echte FFmpeg-/TLS-Strecke wird separat in `uplink-ingest` geprüft; diese
beiden ignorierten Tests sind in keiner grünen Testanzahl enthalten.

Ein alter Upstream-Test wechselte während derselben unvollständigen Nachricht
den Zeitstempel und erwartete dennoch Zusammenfügen. Er prüft jetzt ausdrücklich
die Ablehnung. Gültige Extended-Timestamp- und identische Type-0-Fortsetzungen
werden weiterhin getestet. Die AMF-Tests korrigieren zwei zuvor ungültige
ECMA-Testterminatoren. Keine falsche Altannahme wird als Kompatibilität bewahrt.

RTMP-Lockfile: `cargo audit` am 8. September 2026 mit 97 Abhängigkeiten,
1242 geladenen Advisories, Exit 0, keine gemeldete Sicherheitslücke.
Ein Advisory-Audit ersetzt keine Parserprüfung oder Fuzzkampagne.

## Reproduktion

Im Repository mit Rust 1.97.1; die Manifest-Lockfiles sind enthalten:

```text
cargo +1.97.1 test -j2 --locked --manifest-path third_party/scuffle-rtmp/Cargo.toml
cargo +1.97.1 test -j2 --locked --release --manifest-path third_party/scuffle-rtmp/Cargo.toml
cargo +1.97.1 clippy -j2 --locked --all-targets --manifest-path third_party/scuffle-rtmp/Cargo.toml -- -D warnings
cargo +1.97.1 fmt --manifest-path third_party/scuffle-rtmp/Cargo.toml -- --check
cargo audit --file third_party/scuffle-rtmp/Cargo.lock
cargo +1.97.1 test -j2 --locked --features serde --manifest-path third_party/scuffle-amf/Cargo.toml
cargo +1.97.1 test -j2 --locked --features serde --release --manifest-path third_party/scuffle-amf/Cargo.toml
```

Keine Secrets, echten Streamkonten oder Produktionsservices wurden für diese
Bibliotheksprüfungen verwendet. Codecqualität, OBS-Mehrspurrollen, Twitch-Push,
nahtloser Failover und Kapazität bleiben gesonderte Abnahmen. Ein gemeinsamer
unabhängiger Review der Bibliotheken und ihrer neuen Ingest-Verwendung folgt
vor Übernahme dieses Codebausteins.
