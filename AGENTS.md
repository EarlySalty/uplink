# Uplink

- Verbindliche Grundlage: [Produktvertrag v0.3](docs/produktvertrag-v0.3.md), [Neubauentscheidung](docs/adr/0001-neues-repository-und-rust-neubau.md) und [Abnahmestand](docs/abnahme.md).
- Bei Fragen wie „Was wollte ich bei AV1/Uplink testen?“, zu Upload-Sparen, HQCBR, Native 2K oder Quell-GPU-Weitergabe zuerst [AV1-Testgedächtnis und Agentenübergabe](docs/av1-uplink-test-gedaechtnis.md) lesen. Dort stehen Nutzerziel, bekannte RX 7900 XT, die drei getrennten Wege, Codeorte, Feature-Stand, Aktivierungsgrenzen, OBS-Testablauf und Rückkehr. Den dort datierten Entwicklungsstand vor Aktionen mit dem tatsächlichen Release abgleichen; nach Fortschritten aktualisieren. Bekannte Anforderungen nicht erneut beim Nutzer erfragen.
- Backend, Steuerung und Medienintegration neu in Rust implementieren. Alten Medienkern nicht kopieren oder umbenennen. Geprüfte Browser-Oberflächen und Assets dürfen bleiben; keine zusätzliche Laufzeit für neue Backendlogik.
- Einfach und nachweisbar bauen. Bibliotheken für Codec/TLS verwenden, sobald die benötigte Kombination belegt ist. Keine unnötigen Dienste oder Abstraktionsschichten.
- Keine ENV-Dateien und keine Umgebungsvariablen für Konfiguration. Normale Einstellungen gehören in eine Config-Datei; Secrets ausschließlich aus Infisical bzw. dem bestehenden autorisierten Broker. Secrets nie im Klartext lesen, ausgeben oder in Dateien schreiben.
- Bestehende OAuth-Verbindungen und Tokenverwaltung erweitern; keine zweite Tokenablage. Identitäten und Rechte aus authentifizierten Plattform-IDs ableiten.
- Vier Plattformen bleiben im Umfang. Vorschläge und offene Entscheidungen F1–F6 nicht stillschweigend als Freigabe behandeln. Keine Pflicht-Erweiterung für OBS, kein SRT, keine erfundenen Leistungszusagen.
- Keine LLM-Modelle ohne ausdrückliche Nutzerfreigabe einbauen oder wechseln. Der Neubau braucht derzeit keine KI-Abhängigkeit.
- Deutsch mit echten Umlauten in Nutzertexten. Fehler müssen Ursache und Auswirkungen sichtbar machen; kein stiller Ersatz von VOD-Audio, Profilen oder Zielen.
- Änderungen gezielt testen, danach unabhängig auf Rust-Qualität und bei Eingabe-/Auth-Pfaden auf Sicherheit prüfen. Vor Merge den bestehenden Workspace-Gate `gate_hook.py --review` ausführen; keinen parallelen Gate bauen.
- Die Hauptsession plant, verteilt und überwacht; Agenten führen Bau, Datensammlung und zugewiesene Git-Schritte aus. Parallele Änderungen respektieren. Geprüfte Schritte einzeln mit nachvollziehbaren Commits auf GitHub pushen, kein Force-Push. Merge und Branchbereinigung koordiniert die Hauptsession.
- Produktivwechsel erst mit den nötigen Nachweisen und sicherer Rückkehr; zuständigen Dienst danach neu starten und live prüfen. Kein unfertiges Mediengerüst an die Stelle des laufenden Dienstes setzen.

Weitere Details: [CLAUDE.md](CLAUDE.md).
