# Auftrag: deterministische Security-Gates für EarlySalty/uplink

Nutzerauftrag vom 24. September 2026. Den vorhandenen funktionalen PR-Workflow vollständig erhalten und um Gitleaks, Cargo-Audit, Cargo-Deny, tatsächliche Rust-/Frontend-/Actions-SAST, Trivy HIGH/CRITICAL, Actionlint, Zizmor, Dependabot und einen fehlersicheren stabilen `Required PR Gate` erweitern. Gegenproben sowie aktuelle GitHub-PR-Läufe prüfen und Ausnahmen in `.github/SECURITY-CI.md` dokumentieren.

Remote verifiziert: `/home/nathanael/repos/uplink` verwendet `git@github.com:EarlySalty/uplink.git`. Ausgangsstand nach Fetch: `ca9abd04102b372bd877bd800243b751e47ebef9`, sauberer `main`. Ausschließlicher Schreibbereich ist der eigene Worktree `/home/nathanael/.worktrees/uplink-security-gate-20260924`, Branch `ci/deterministic-security-gate-20260924`. Der bestehende AV1-Worktree, `rs-relay`, andere Repositories und fremde Änderungen bleiben unberührt.

PR-Testbetrieb: verifizierte Commits pushen, PR erstellen und offen lassen. Kein automatischer oder manueller Merge, kein Direktpush nach `main`, kein Release/Deployment, keine Änderungen laufender Dienste oder Streams. Keine produktiven Secrets oder Produktionsdaten in PR-Tests. Bestehende Hooks und Schutzmechanismen bleiben erhalten. Das private Repository meldet eine GitHub-Tarifsperre für Rulesets/Branch-Protection; diese Einschränkung wird nicht umgangen.

Arbeitsregeln aus `/home/nathanael/AGENTS.md`, `/home/nathanael/CLAUDE.md` sowie den Repository-Regeln gelesen. Graphify vor der Detailanalyse konsultiert; der globale Graph lieferte keine brauchbare Uplink-CI-Auflösung, anschließend vorhandene Workflows, Architektur-/Abnahmeunterlagen, Testharness und Manifeste direkt geprüft. Schwere funktionale Tests gehören auf kurzlebige GitHub-Runner, nicht auf einen laufenden Streaming-Dienst.

Produktionsfreigabe ist ausdrücklich nicht Teil dieses Auftrags. Aktuelle Testnachweise und der endgültig geprüfte SHA werden im PR-Abschlussbericht festgehalten.
