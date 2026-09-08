# Lokaler AMF-Patch

Basis ist die unveröffentlichte lokale Ableitung des veröffentlichten Crates
`scuffle-amf0 0.2.4`, keine eigene AMF-Protokollimplementierung.

- Quelle: <https://crates.io/crates/scuffle-amf0/0.2.4>
- Archiv-SHA256: `2db0c6c36b3c0ace0a46d73193d800374b6152de0e3209d0953078bd8eaa4e27`
- Upstream-Git: `1b7b6ec46bcfeb5b9569753ab94e467eb2ab0c04`, Verzeichnis `crates/amf0`
- Lizenz: `MIT OR Apache-2.0`; beide Lizenzdateien unverändert übernommen.
- `Cargo.toml.orig` bewahrt das ursprüngliche Manifest. Das normalisierte
  Buildmanifest entfernt den direkten Workspace-Hack, enthält einen eigenen
  Workspace und registriert die Regressionstests. Über andere Scuffle-Crates
  kann der Workspace-Hack weiterhin transitiv im Lockfile vorkommen.

Die lokale Änderung vom 8. September 2026 begrenzt AMF-Daten vor Stringabrufen,
Containerreservierungen und Serde-Größenhinweisen. `DecodeLimits` gilt für
native Decoderaufrufe und Serde gemeinsam. Standardwerte sind 64 KiB Eingabe,
16 KiB je String/Schlüssel/Klassenname, 128 tatsächliche Containereinträge,
2048 Werte einschließlich Container und 16 gleichzeitig offene Container.
Schlüssel und Klassennamen sind keine zusätzlichen AMF-Werte; ihre Größe und
die Anzahl der zugehörigen Einträge sind gesondert begrenzt. Doppelte Schlüssel
zählen als tatsächliche Einträge und umgehen die Grenze nicht.

`Amf0Decoder::with_limits` akzeptiert einen `ZeroCopyReader`. Die bequemen
Konstruktoren heißen `from_buf_with_limits`, `from_slice_with_limits` und
`from_reader_with_limits`. Bestehende Konstruktoren verwenden die Standardwerte.
Die Limits gelten über die gesamte Lebensdauer eines Decoders, auch bei
abwechselnden nativen und Serde-Aufrufen. Ein Decoder gehört zu einer begrenzten
Nachricht, nicht zu einer unbegrenzt langen RTMP-Session.

ECMA-Längen sind Hinweise. Objekte werden bis zum vollständigen Terminator
`00 00 09` gelesen; die angegebene ECMA-Anzahl steuert keine Vorreservierung
und keinen Serde-Größenhinweis. Strikte Arrays prüfen dagegen ihre verbindliche
Anzahl gegen Limits und bei bekanntem Eingabeumfang gegen die verfügbaren Bytes.
Serde prüft zusätzlich vollständigen Containerverbrauch und Fortschritt der
Element-/Wert-Seeds. Auch `IgnoredAny`, optionale Nullwerte und `MultiValue`
durchlaufen dieselben Budgets.

Die freien Einzelwertfunktionen `from_slice`, `from_buf`, `from_reader`
weisen Restdaten zurück. Inkrementelle Decoderaufrufe bleiben möglich;
`finish()` prüft anschließend das Nachrichtenende. IO-Reader können für diese
Prüfung ein zusätzliches Byte über dem eigentlichen Budget lesen, niemals
dekodieren oder davon Speicher reservieren. Nach fehlgeschlagenen Teilreads
verweigert der Decoder weitere Verarbeitung. Aufrufer sollen einen Decoder
nach jedem Fehler verwerfen. Das ist kein Netzwerkframing oder Transporttimeout.

Geänderte Produktionsdateien: `src/decoder.rs`, `src/de/mod.rs`,
`src/de/stream.rs`, `src/error.rs`, `src/lib.rs`. Die übrigen Quelldateien
wurden ausschließlich mit dem lokalen Rust-Formatter formatiert. Zwei
Upstream-Testobjekte besaßen keinen korrekten ECMA-Terminator; sie enthalten
nun gültige Endmarkierungen. Der Serde-Doctest ist an das Serde-Feature gebunden.

Nachweis mit Rust 1.97.1: zunächst neun neue Regressionen gegen unveränderten
Upstream-Code ausgeführt, alle neun rot. Nach dem Patch 21 Grenztests,
40 Bibliothekstests und ein Doctest in Debug und Release grün. Ohne Serde
elf Bibliothekstests und ein Doctest grün. `cargo fmt --check` und
`cargo clippy --all-targets --features serde -- -D warnings` sowie Clippy ohne
Features grün. Die ursprünglichen neun Regressionen prüfen Container-/Tiefen-
und Stringgrenzen, ECMA-Semantik, Serde-Größenhinweise und vollständigen Verbrauch.
Weitere Fälle prüfen unter anderem `u32::MAX`, verkürzte Eingaben, gemischte
Decoderaufrufe, individuelle Limits und ausbleibenden Serde-Fortschritt.

Reproduktion im Verzeichnis dieses Manifests:

```text
cargo +1.97.1 test --features serde
cargo +1.97.1 test --features serde --release
cargo +1.97.1 test --no-default-features
cargo +1.97.1 fmt -- --check
cargo +1.97.1 clippy --all-targets --features serde -- -D warnings
cargo +1.97.1 clippy --all-targets --no-default-features -- -D warnings
```

Die Tests belegen begrenzte synthetische Parserfälle. Sie ersetzen weder
Fuzzing noch TLS-, OBS- oder Plattformabnahmen. Unabhängiger Rust-/Sicherheitsreview
und die Integration in den begrenzten RTMP-Eingang sind gesonderte Schritte.
