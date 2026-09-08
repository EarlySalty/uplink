# Kick: eigene Abos nach normalem Trennen bereinigen

Basis: `02db760ee5bca1ddc03dc97a72b7ed520e1ccbd3`. Dieser Nachweis betrifft nur
den Kick-Cleanup und dessen Kopplung an die vorhandene TokenQuelle. Er ist kein
Nachweis des vollständigen Plattformbetriebs oder des Produktivwechsels.

## Ursache und Änderung

Der normale Disconnect sperrt den zentralen Uplink-Broker, bevor der ChatHub den
Adapter trennt. Die bisherige Bereinigung forderte ihre Zugangsdaten ebenfalls
bei diesem bereits gesperrten Broker an. Eigene vorhandene Kick-Abos blieben
dadurch ohne externen Tokenwiderruf oder Plattformfehler liegen.

Der vorhandene Abo-Besitzer erhält jetzt beim regulär autorisierten Anlegen einen
flüchtigen Cleanup-Zugang. Dessen typisierte Operationen sind ausschließlich
Tokenprüfung, Abgleich des eigenen Konto-/App-/Event-Bestands und DELETE bereits
verantworteter IDs. Der normale Chat- und Anlageweg verwendet weiterhin nur den
regulären Broker. Es gibt keinen zusätzlichen OAuth-Flow, Refresh-Token-Speicher
oder Cleanup-Refresh-Aufruf. Geheimnisse werden weder protokolliert noch
persistiert; verworfene Grant-/Cleanup-Token werden im RAM überschrieben.

Die bestehende TokenQuelle liefert erfolgreich validierte normale Grant-Wechsel
an schwache, auf 1024 begrenzte Empfänger. Registrierung und bereits bestätigter
Anfangsstand sind gegenüber einer gleichzeitig eintreffenden Antwort atomar.
Danach erfolgt eine Übernahme nur beim tatsächlichen erfolgreichen Brokeraufruf
nach dessen bestehender Generation-Prüfung, nicht durch Cache-Lesen oder einen
zusätzlichen Poll. Konto und Scope müssen passen. Der Ressourcenbesitzer prüft
seine unveränderte App erneut, bevor er einen erneuerten Cleanup-Zugang übernimmt.
Das verhindert, dass eine lange Verbindung unnötig ihren ersten Token behält,
obwohl der normale Broker ihn inzwischen erneuert hat.

Vor Trennen, Adapterfreigabe oder einem weiteren Anlegeversuch wird der bisherige
Empfänger gestoppt. Ein neuer Besitzerdurchlauf ersetzt ihn. Ein gestoppter
Empfänger wird nicht mehr aktualisiert; tote Empfänger werden entfernt. Die
letzte bestätigte Erneuerung bleibt während einer abgebrochenen Bereinigung
erhalten. Alte Bereinigung und neue Anlage bleiben durch den vorhandenen
Owner-Lock serialisiert. Bestehende Grenzen für Besitzer, IDs, Antwortgröße,
gleichzeitige Bereinigungen und Fristen bleiben erhalten.

## Ausgeführte Prüfungen

- Ursprüngliche normale Disconnect-Probe auf der Basis ohne Fix: rot, kein
  DELETE für das eigene vorhandene Abo (`/tmp/uplink-kick-trennen-red.log`).
- Dieselbe Probe mit Fix: grün. Der Broker ist gesperrt; eigenes DELETE findet
  statt; Chat-Schreiben und neue Anlage bleiben gesperrt.
- Lokaler HTTP-Broker liefert einen erneuerten Grant über den wirklichen
  TokenQuelle-Pfad. Danach lehnt die Plattform den alten Token ab und der Broker
  ist getrennt. Der bestehende Owner löscht mit dem zuvor regulär erneuerten
  Zugang. Keine manuelle Test-Benachrichtigung und kein weiterer Refresh.
- Alter verzögerter DELETE gegen eine neue Verbindung: neue Abos entstehen erst
  nach der alten Bereinigung und werden von ihr nicht gelöscht.
- Falsches Konto, fehlender Scope, gestoppter Empfänger, geänderte App sowie eine
  nach Identitätsentzug eintreffende Brokerantwort aktualisieren keinen
  unpassenden Cleanup-Besitzer. Empfängergrenze und Freigabe werden geprüft.
- Externer Widerruf liefert einen sichtbaren typisierten Cleanup-Fehler. Ein
  abgelaufener Zugang wird verworfen; die offenen IDs bleiben erhalten. Kein
  Chat-Schreiben, keine neue Anlage und kein behauptetes erfolgreiches DELETE.
- Bestehende Fälle für Teilantworten, unklaren POST, Cancellation, fehlgeschlagene
  DELETEs, Konto-/App-/Event-Zuordnung und begrenzte Schulden bleiben grün.

Selbstprüfung: `cargo check` und `cargo clippy -- -D warnings` für `uplink-chat`
und `uplink-service`, jeweils alle Targets; `cargo fmt --all --check` und
`git diff --check`; 150 Chat-Bibliothekstests, 0 fehlgeschlagen, 0 ignoriert
(1,03 Sekunden Laufzeit nach dem Build). Rust-Werkzeugkette 1.97, `--locked -j2`,
eigener Target-Pfad. Logs: `/tmp/uplink-kick-trennen-{check,clippy,final-tests}.log`.
Die zentrale Autorenprüfung auf dem anschließend erzeugten Commit und der
unabhängige Nachreview werden getrennt berichtet.

## Grenzen

Ohne einen noch gültigen, zuvor normal autorisierten Zugang sind abgelaufene oder
extern widerrufene Berechtigungen nicht umgehbar. Der begrenzte Owner behält dann
die Cleanup-Schuld und meldet den Fehler einmal pro ungelöstem Fehlerzustand;
eine spätere passende reguläre Verbindung kann sie vor einer neuen Anlage
bereinigen. Es gibt keine Zusage erfolgreicher Löschung trotz Plattformwiderruf.
Owner und Cleanup-Zugang bleiben prozesslokal; dieser Fix führt weder einen
dauerhaften Cleanup-Job noch eine Koordination mehrerer Prozesse ein.

Die Prüfungen verwenden lokale HTTP-Gegenstellen und synthetische Grants. Es
wurden keine echten Plattformkonten, Produktivdienste oder externen Abos geändert.
