# Entscheidungen für den weiteren Neubau

Stand: 8. September 2026. Ergänzung zum unveränderten Produktvertrag v0.3 aus den aktuellen Nutzerantworten und den ausdrücklich benannten Arbeitsannahmen der Hauptsession.

Bestätigt sind zwei wählbare VOD-Quellen: Uplink-Eingangsmitschnitt oder vorhandenes Twitch-VOD. Der YouTube-Upload erfolgt automatisch nach Streamende. Lokale Quellen dürfen erst nach sicher bestätigtem Uploadabschluss, Abgleich und erfolgreicher YouTube-Verarbeitung gelöscht werden. Ein Fehler darf die Quelle nicht automatisch löschen. Daraus folgt weder eine erfundene allgemeine Aufbewahrungsfrist noch eine vorweggenommene Entscheidung über Speicherprodukt oder VOD-Repository.

Die Unterstützung wird nicht auf das OBS, Betriebssystem oder die GPU des Betreibers begrenzt. Kompatible Standardquellen sollen funktionieren; die konkrete Codec-/Trackkombination wird am Eingang geprüft. Eine universelle Codec-, Versions- oder HDR-Freigabe ist damit nicht behauptet.

Als sichtbare Arbeitsannahmen für die noch offene Bediensemantik gelten: Fehlt der gewünschte VOD-Mix, werden ausschließlich davon abhängige Ausgaben und Exporte angehalten; andere Ausgaben laufen weiter. Delayänderungen gelten zunächst ab dem nächsten Stream. Bereits gespeicherte individuelle `reconnect_wait_s` bleiben erhalten. Diese Regeln sind keine Abnahme des derzeitigen Failover- oder Exportcodes.

Ohne ausdrückliche Veröffentlichung oder autorisierte Veröffentlichungsregel bleiben VOD-Uploads privat. Die bloße YouTube-Verbindung erlaubt keine automatische öffentliche Veröffentlichung. Bestehende OAuth- und Tokenquellen bleiben autoritativ; tatsächlich gewährte Scopes werden geprüft, einschließlich eines gegebenenfalls bereits ausreichenden `youtube.force-ssl`.

Die drei früher gemergten Grundlagen und der jetzt gebaute Dienst-/Medienbaustein ersetzen noch keinen Nachweis des vollständigen Produktivwechsels. Wartebild, Weiterführung derselben logischen Session beim Reconnect, vollständige Plattformfunktionen, Dashboard/Docks sowie VODs werden im weiteren Auftrag umgesetzt und geprüft.
