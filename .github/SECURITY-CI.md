# Deterministische PR- und Security-CI

Stand: 24. September 2026. Ausschließlich PR-Testbetrieb. Diese Änderung erlaubt weder Merge noch Release, Deployment oder Neustart eines Streaming-Dienstes.

## Verbindlicher Abschlusscheck

Der Workflow `Rust` läuft ohne Workflow-Pfadfilter auf jedem `pull_request`, auf `merge_group`, auf Push nach `main`, manuell und montags um 04:23 UTC. Zeitgesteuerte Läufe werden von GitHub erst aktiviert, wenn der Workflow im Default-Branch vorhanden ist. Der PR-Testbetrieb lässt den PR ausdrücklich offen.

`Required PR Gate` verwendet `always()` und die vollständigen `needs` der sieben Pflichtjobs: `check`, `docks`, `gitleaks`, `cargo-security`, `semgrep`, `trivy`, `workflow-security`. `.github/ci/required-gate.jq` akzeptiert ausschließlich genau diese Jobs mit `result == success`. Fehlende oder zusätzliche Jobs, Abbruch, Fehler, neutrale/übersprungene oder unbekannte Ergebnisse blockieren. `.github/ci/test-gate.sh` verwendet dieselbe echte Policy: ein positiver Test und 83 Gegenproben. Ein komplett abgebrochener Workflow wird nicht als erfolgreicher Gate-Lauf gewertet.

Alle Jobs laufen auf kurzlebigen GitHub-Runnern mit `contents: read`, ohne produktive Secrets und ohne `pull_request_target`. Checkout speichert keine Git-Anmeldedaten. Es gibt weder Fehlerneutralisierung über `continue-on-error` noch optionale Pflichtjobs. Ein nachgelagerter Report-Upload mit `always()` ändert das Ergebnis des davor fehlgeschlagenen Scans nicht. Reports bleiben 14 Tage verfügbar; Secret-Reports werden redigiert.

## Funktionaler Bestand und Erweiterungen

Der bestehende Job `check` und sämtliche vorhandenen Prüfkommandos bleiben erhalten:

- Workspace-Formatierung, Clippy mit `-D warnings`, alle Workspace-Tests und vollständiger Release-Build, jeweils mit dem gepinnten Toolchain-Stand aus `rust-toolchain.toml` und mit Lockfile-Bindung, wo Cargo sie unterstützt.
- PostgreSQL-Integration für Datenbank, HTTP-Shutdown und Lebenszyklus; die Installation von PostgreSQL 16 und FFmpeg erfolgt jetzt ausdrücklich vor den Workspace-Tests, weil auch die VOD-Tests PostgreSQL benötigen. Zusätzlich läuft der bisher ignorierte `uplink-youtube-live --test store_postgres`-Test.
- Unverändertes, dreifach SHA256-geprüftes FFmpeg-8.1.2-Archiv samt `ffmpeg` und `ffprobe`; `normalbetrieb`, `idee1_single` und die vier echten Audio-/AAC-Regressionen.
- Releasepaket-Negativtests, Paketbau und `--check-package` einschließlich Bridge und TLS-Provider. Das ist kein Deployment und veröffentlicht kein Paket.
- Ingest-Release-Tests, RTMPS-Probe in Debug und Release, RTMP- und AMF-Parser in beiden Profilen, AMF mit `serde`, lokales Planszenario und eigenständige E-FLV-Probe mit Formatierung, Clippy und Debug-/Release-Tests.
- Neuer separater Job für die bestehenden DOM-Regressionen der vier Docks. `npm ci --ignore-scripts` verwendet ausschließlich `web/docks/tests/package-lock.json`; Fetch, WebSocket und Zeit sind im vorhandenen Testharness simuliert.

PostgreSQL-Tests starten eigene temporäre Cluster mit privaten Unix-Sockets und deaktiviertem TCP-Listener. Medien-/RTMPS-Tests verwenden synthetische Daten und lokale Endpunkte. Keine produktive Datenbank, keine echten Plattform-Streams und keine Dienststeuerung sind Teil dieser CI.

Bewusst nicht ausgeführt: Die beiden vorhandenen Upstream-RTMP-Tests `test_basic_rtmp_clean` und `test_basic_rtmp_unclean` benötigen das nicht mitgelieferte Monorepo-Asset `../../assets/avc_aac.mp4` und beliebiges externes FFmpeg. Ihr bestehendes `ignore` bleibt dokumentiert in `docs/third-party-patches.md`; sie gelten nicht als erfolgreiche Tests. Die lokale Browser-Smoke-Datei verlangt einen spezifischen extern vorinstallierten Chromium-Build und ist kein portabler PR-Test; die tatsächlichen Dock-Skripte werden stattdessen im vorhandenen DOM-Harness geprüft. Reale OBS-/Plattform-/Produktivabnahmen bleiben getrennte Aufgaben.

## Scanner und tatsächlicher Umfang

| Pflichtjob | Blockierende Prüfung |
| --- | --- |
| Gitleaks 8.30.1 | Vollständige erreichbare Git-Historie mit `fetch-depth: 0` und `--all`, zusätzlich Arbeitsbaum. Eingebaute Regeln, keine projektweite Baseline oder Quellcode-Ausnahmen. Ein künstlicher PAT muss gefunden werden, ein leeres Kontrollverzeichnis muss sauber sein. |
| Cargo-Audit 0.22.2 | Alle vier Cargo-Lockfiles, einschließlich Warnungen (`--deny warnings`). Ein absichtlich verwundbares, nur geparstes Smallvec-Lockfile muss sowohl einen Fehlercode als auch echte Advisory-Daten liefern. |
| Cargo-Deny 0.20.2 | Alle vier Manifeste mit `--locked --all-features`: Advisories, Lizenzen, Bezugsquellen und Dependency-Regeln. Ein isoliertes, ausdrücklich gesperrtes Test-Crate muss als `banned` abgewiesen werden; es wird nicht gebaut oder ausgeführt. |
| Semgrep 1.173.0 | Eingecheckte Regeln statt einer zur Laufzeit veränderlichen oder leeren Registry-Auswahl. Sechs Rust-Regeln: deaktivierte Zertifikats-/Hostnamenprüfung, fehlende OpenSSL-Verifikation, formatierte Kommando- und SQL-Strings, weltbeschreibbare Dateimodi. Zusätzlich JavaScript/TypeScript-Codeausführung, ein generisches Muster für Inline-HTML-Skripte und GitHub-Actions-Script-Injection. Alle neun Regeln müssen in synthetischen Dateien wirklich auslösen. Der echte Report muss Rust-, HTML- und Workflow-Dateien enthalten und darf keine Scannerfehler enthalten. |
| Trivy 0.73.0 | Dateisystem-SCA und Fehlkonfigurationen, einschließlich Entwicklungsabhängigkeiten, HIGH/CRITICAL mit Exit-Code 1. Kein `ignore-unfixed`. Alle vier Cargo-Lockfiles und das npm-Lockfile müssen im Report erscheinen. Ein nicht installiertes synthetisches Lodash-Lockfile muss die bekannte HIGH-Advisory CVE-2021-23337 auslösen. Secret-Scanning übernimmt separat Gitleaks. |
| Actionlint 1.7.12 / Zizmor 1.30.1 | Syntax/Shell-Prüfung sowie lokale Workflow-Sicherheitsanalyse im pedantischen Modus; Befunde ab LOW blockieren. Gate-Gegenproben laufen im selben Pflichtjob. |

Semgrep läuft mit `--strict --error --disable-nosem`. Eine fehlende Konfiguration wird in einer separaten Gegenprobe ausdrücklich abgewiesen. Scannerzeitüberschreitungen führen nicht zu einem grünen Ergebnis: Das Budget ist 60 Sekunden pro Regel/Datei, zwei Worker; `errors` im JSON blockieren zusätzlich. Die Regeln sind gezielte musterbasierte SAST, kein Nachweis vollständiger Datenfluss- oder Anwendungssicherheit. Gepatchter Third-Party-Code und Tests sind im Scan enthalten. Ausgeschlossen sind ausschließlich Git-Metadaten, Build-Ausgaben und installierte Node-Pakete; deren Lockfile bleibt in der Abhängigkeitsprüfung.

Scanner-Downloads verwenden feste Versionen und vor dem Entpacken geprüfte SHA256-Werte aus den offiziellen GitHub-Release-Assets. Keine Remote-Installerskripte werden ausgeführt. Semgrep und seine Python-Abhängigkeiten sind vollständig versioniert und mit Paket-Hashes in `.github/ci/semgrep-requirements.txt` gebunden; die Installation erfolgt in einer temporären Python-Umgebung mit `--require-hashes`. Sicherheitsdatenbanken werden absichtlich aktualisiert: deterministisch bedeutet hier unveränderliche Kontrollstruktur und nachvollziehbare Tool-/Regelversionen, nicht das Einfrieren neu bekannt gewordener Schwachstellen.

Verifizierte Action-Refs vom 24. September 2026: `actions/checkout@v4` auf `11d5960a326750d5838078e36cf38b85af677262`, `actions/setup-node@v4` auf `49933ea5288caeca8642d1e84afbd3f7d6820020`, `actions/upload-artifact@v4` auf `ea165f8d65b6e75b540449e92b4886f43607fa02`. Workflows verwenden nur die vollständigen SHAs. Dependabot pflegt GitHub Actions, Cargo am tatsächlichen Root-Workspace und an den drei eigenständigen Manifestpfaden sowie das npm-Testprojekt.

## Ausnahmen und bewusst nicht blockierende Hinweise

Keine Advisory-Ignore-Liste und kein pauschaler SAST- oder Trivy-Befund-Ausschluss. Die einzige Gitleaks-Ausnahme steht in `.gitleaks.toml` und verbindet per AND die Regel `generic-api-key`, den exakten Pfad `crates/uplink-service/src/api.rs` und den vollständig verankerten SQL-Spaltenausdruck. Die Stelle wurde sowohl im Ausgangsbaum als auch in Commit `df170b8e266e3a0d86bbf41f5f8e3d1e09ea82ae`, dort Zeile 766, geprüft: Die Fundstelle ist eine parametrisierte SQL-Abfrage; der vermeintliche Schlüssel besteht ausschließlich aus SQL-Spaltennamen und der `EXCLUDED`-Referenz. Es gibt keine Zeilennummern-Baseline und keine dateiweite oder regelweite Freistellung. Eine zusätzliche künstliche generische API-Key-Probe im identischen Dateipfad muss weiterhin blockieren. Keine echten Zugangsdaten oder deren Rotation sind Gegenstand dieser Ausnahme. Cargo-Deny erlaubt nur die expliziten Lizenzen in `deny.toml`; private, unveröffentlichte eigene Workspace-Crates sind von der Lizenzmetadatenpflicht ausgenommen, nicht deren Fremdabhängigkeiten. Wilde Versionsangaben bleiben verboten; interne private Pfadabhängigkeiten dürfen ohne Registry-Versionsgrenze existieren.

Cargo-Deny-Meldungen über gleichzeitig benötigte Versionen oder über für ein einzelnes Teilmanifest unbenutzte Lizenzfreigaben sind Qualitäts-/Konfigurationshinweise, keine ignorierten Sicherheitsadvisories, und bewusst nicht blockierend. Trivy LOW/MEDIUM ist außerhalb der dort geforderten HIGH/CRITICAL-Schwelle; Cargo-Audit prüft Rust-Advisories unabhängig von dieser Schwelle. RustSec-Warnungen werden nicht freigestellt.

Beim Ausgangsstand fand Cargo-Audit `RUSTSEC-2026-0285` in `rustls 0.23.44`. Der PR hebt die Mindestversion und das Root-Lockfile gezielt auf die korrigierte Version `0.23.45` an, statt die Advisory auszunehmen. Referenz: https://rustsec.org/advisories/RUSTSEC-2026-0285.html

## GitHub-Schutzstatus und Abnahme

Die authentifizierten Abfragen für Repository-Rulesets und `main`-Branch-Protection lieferten am 24. September 2026 HTTP 403 mit `Upgrade to GitHub Pro or make this repository public to enable this feature.` Damit kann der stabile CI-Abschlusscheck bereitgestellt werden, aber in diesem privaten Repository ist die serverseitige Erzwingung als erforderlicher Mergecheck derzeit durch den Tarif blockiert. Es wurden keine Sichtbarkeit, Tarife, Schutzregeln oder bestehenden zentralen Review-Hooks geändert. Ein Checkname allein ist keine aktivierte Branch-Protection.

Nach Verfügbarkeit geeigneter GitHub-Schutzfunktionen soll exakt `Required PR Gate` als erforderlicher Check für `main` eingerichtet werden. Kein LLM-/Copilot-Review wird durch diesen PR als notwendiger GitHub-Mergecheck eingeführt. Maßgebliche GitHub-Läufe, der tatsächlich getestete PR-Head-SHA und Gegenproben werden im PR-Bericht verlinkt. Nur abgeschlossene erfolgreiche Läufe am aktuellen Head gelten als CI-Abnahme; lokale oder ältere grüne Läufe ersetzen sie nicht.

### GitHub-Abnahmeversuch im PR-Testbetrieb

PR: https://github.com/EarlySalty/uplink/pull/26

Der erste echte PR-Lauf https://github.com/EarlySalty/uplink/actions/runs/35941892958 für `180e50433354c7b2604482089d53aceeddf88751` endete rot, bevor irgendein Runner oder Testschritt gestartet wurde. Alle sieben Pflichtjobs und auch der sichtbare `Required PR Gate` wurden als fehlgeschlagen ausgewiesen. Die GitHub-Annotation lautet: `The job was not started because recent account payments have failed or your spending limit needs to be increased.` Das ist ein externer Billing-/Limit-Blocker, kein ausgeführter Testfehler und kein grüner CI-Nachweis. Ein höheres Budget, geänderte Zahlungsdaten, andere Repository-Sichtbarkeit oder selbstgehostete Runner werden durch diesen Auftrag nicht autorisiert oder als Umgehung eingerichtet.

Lokale Security-Prüfungen und Gegenproben sind erfolgreich; die vollständige funktionale GitHub-Abnahme bleibt ausdrücklich offen, bis GitHub die Runner starten kann. Maßgeblich für spätere Versuche sind der aktuelle PR-Head und dessen im PR-Bericht verlinkter Run, nicht allein der hier festgehaltene erste Versuch.

### Tatsächlich ausgeführte lokale Nachweise

- Alle fünf Security-Harness-Modi einschließlich ihrer Gegenproben erfolgreich: Gitleaks, Cargo-Audit/Deny, Semgrep, Trivy und Workflow-Prüfungen. Cargo-Audit prüfte 308 Abhängigkeiten im Root-Lockfile sowie 97, 31 und 43 in RTMP, AMF und der eigenständigen Probe, jeweils ohne verbleibende Advisory. Semgrep prüfte 179 Rust-Dateien und insgesamt 186 Ziele mit neun Regeln; keine Findings und keine Scannerfehler. Trivy erfasste alle fünf geforderten Lockfiles ohne HIGH/CRITICAL-Fund; es fand im Bestand keine unterstützten IaC-Konfigurationsdateien. Daraus wird keine tatsächlich vorhandene IaC-Abdeckung abgeleitet.
- Die vorhandene Dock-Suite lief mit `npm ci --ignore-scripts --no-audit --no-fund --prefix web/docks/tests` und `npm test --prefix web/docks/tests`: 35 bestanden, null Fehler, null Skips. Lokale Node-Version: 22.23.2; der PR-Job ist auf 22.22.2 gepinnt. Keine realen Plattformverbindungen.
- `sh deployment/tests/prepare-release.sh` bestand die Paketregressionen für Pflichtartefakte, Ausführbarkeit, Symlinks, Manifestumfang und Integrität. Der Harness verwendet Test-Artefakte; dies ersetzt weder einen echten Release-Build noch dessen vollständige Paketabnahme.
- `cargo fmt --all -- --check` fand vier bereits im Ausgangsstand unformatierte Dateien: die Beispiele `enhanced_av1_1080_load_probe.rs` und `native_2k_av1_load_probe.rs` sowie `uplink-service/src/media.rs` und `media_output.rs`. Ausschließlich rustfmt wurde auf diese Dateien im eigenen Worktree angewendet. Der erneute vollständige Formatcheck mit Rust 1.97.1 ist erfolgreich; keine Lastprobe wurde gestartet.

Workspace-Clippy, kompilierte Rust-/PostgreSQL-/FFmpeg-/RTMPS-/Parser- und Release-Profiltests wurden im Rahmen dieses Abnahmeversuchs nicht lokal als Ersatz für die gesperrten GitHub-Runner ausgeführt. Die schwere funktionale Prüfkette bleibt unverändert auf isolierten GitHub-Runnern vorgesehen; laufende Streaming-Dienste sind kein Ausweich-Testsystem. Diese Prüfungen sind damit konfiguriert, aber für diesen PR noch nicht vollständig nachgewiesen.
