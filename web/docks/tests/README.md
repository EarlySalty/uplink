# Dock-Regressionsnachweis

Die Tests führen die tatsächlichen Inline-Skripte der vier erhaltenen HTML-Docks aus. Die Testsuite ist eine reine Entwicklungsabhängigkeit; Uplink benötigt dafür weder Node.js noch jsdom im Betrieb.

## Ausführen

Mit Node.js ab 22.22.2:

```sh
npm ci --ignore-scripts --no-audit --no-fund --prefix web/docks/tests
npm test --prefix web/docks/tests
```

Die 24 DOM-Tests steuern Zeit, Fetch-Antworten und WebSocket-Ereignisse. Sie prüfen die sichtbaren Felder, Karten und tatsächlich erzeugten Request-Bodies. Vor der Korrektur waren von den ersten 17 Fällen 15 rot; danach waren alle grün. Die vier später ergänzten Statusfehler-Tests sowie drei Assertions zur revisionsbezogenen Speicherbestätigung wurden ebenfalls zuerst rot nachgewiesen.

Abgedeckt sind:

- Chatentwürfe während eines laufenden Versands, Statusmeldungen während des Requests, vollständige Ablehnung und leere Ergebnisantworten.
- Jüngere lokale Änderungen an Titel, Kategorie und Tags bei Laden/Speichern; Teilfehler und erfolgreiches Speichern ohne anschließenden Lesestand bleiben unterscheidbar.
- Sobald nach dem Speicherklick ein neuer Entwurf entsteht, lautet dessen zugehörige ältere Bestätigung „Vorheriger Stand auf Twitch gespeichert“. Zusätzliche Plattformhinweise bleiben erhalten; dies gilt auch für Änderungen nach Eintreffen der Bestätigung.
- Geleerte oder bereits erneut geänderte Suchanfragen, bevor ein neuer Request startet.
- Extern erfüllte oder abgelehnte Kanalpunkte, verspätete lokale Antworten und verspäteter Nachlauf.
- Aktualisierte Chat-/Geschenkereignisse mit gleicher fachlicher ID sowie Trennung identischer Einlösungs-IDs verschiedener Plattformen.
- Sichtbare technische Brokerfehler und ausdrücklich nicht verfügbare Integrationen, auch wenn `eingerichtet=false` ist.

## Isolierter Browser

Der zusätzliche Browserlauf benötigt eine vorhandene Chromium- oder Chromium-Headless-Shell-Datei als explizites Argument:

```sh
node web/docks/tests/browser-smoke.mjs /absoluter/pfad/zu/chrome-headless-shell
```

Er startet ein eigenes temporäres Profil und einen lokalen HTTP-Server. Transportantworten sind ausschließlich Testdaten; es werden keine Konten verbunden, Nachrichten verschickt oder Plattformmetadaten geändert. Die echten HTML-Docks werden unter einer restriktiven CSP geladen, per DOM bedient und bei 520 × 800 Pixeln aufgenommen. Die Screenshots und der Bericht landen in `artifacts/` und werden nicht eingecheckt. Chromium-Profil und Server werden nach dem Lauf entfernt bzw. beendet.

Am 8. September 2026: HeadlessChrome/151.0.7922.34, alle vier Bedienproben bestanden, keine JavaScript-Ausnahme und kein horizontaler Seitenüberlauf. Alle vier Screenshots wurden visuell geprüft; die vorhandene schwarze/goldene Gestaltung bleibt erhalten. Die lokale Probe hat keine Brand-Font-Dateien und zeigt deshalb den vorgesehenen Systemfont-Fallback.

Das belegt die lokalen UI-Korrekturen. Die Rust-Routen, tatsächliche Kontorechte, echte Plattformnachrichten, OBS-Versionen und Produktivbetrieb benötigen weiterhin den gemeinsamen Integrationsnachweis. Der zentrale Workspace-Review-Gate bleibt unverändert zuständig; diese Tests ersetzen ihn nicht.
