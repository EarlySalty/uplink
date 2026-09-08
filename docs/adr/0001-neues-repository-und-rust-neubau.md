# ADR 0001: Neues Repository und vollständiger Rust-Neubau

**Datum:** 8. September 2026. **Status:** Nutzerentscheidung bestätigt; Umsetzung begonnen, Produktabnahme offen.

## Anlass

Der Nutzer will den vorhandenen, unübersichtlich gewordenen Medienkern vollständig ersetzen. Er hat nach Übergabe des Produktvertrags v0.3 ausdrücklich ein neues Repository beauftragt und anschließend präzisiert, dass der Code wirklich neu geschrieben werden soll. Ein Refactor oder bloßes Umbenennen des bisherigen Medienkerns erfüllt den Auftrag nicht.

## Entscheidung

1. Uplink erhält ein eigenständiges neues Repository `EarlySalty/uplink`, lokal unter `/home/nathanael/repos/uplink`. Die Anlage und der Versand dieses Repositorys gehören zum autorisierten Auftrag; Remote-Nachweise werden in der Aufgabenakte festgehalten.
2. Neue Backendlogik, Mediensteuerung, Sessionverwaltung und Plattformintegration werden in Rust implementiert. Verwendbare Medien-/TLS-Bibliotheken sind zulässig und werden anhand echter Interoperabilität ausgewählt. Bestehender Rust-Code aus dem alten Kern wird nicht pauschal kopiert, nur weil seine Sprache bereits passt.
3. Alter Medienkern, alte Repository-Historie, Build-Artefakte, Datenbanken, Secrets und Laufzeitkonfigurationen werden nicht als Startpaket importiert. Der Altbestand darf als lesbare Funktionsreferenz dienen.
4. Oberfläche, Overlay, Chat-Bedienung und OBS-Docks dürfen nach Prüfung übernommen werden. Browser-HTML/CSS/JavaScript bleiben als Browserbestand zulässig; diese Ausnahme macht daraus keine zweite Backend-Laufzeit. Herkunft, Dateiumfang und Integrationslücken werden dokumentiert. Ein kopiertes Asset gilt noch nicht als wiederhergestellte Funktion.
5. Bestehende Kontoverbindungen und die autoritative OAuth-/Tokenverwaltung bleiben erhalten. Der Neubau erhält passende Adapter statt einer zweiten unabhängigen Tokenablage. Kein Ablauf setzt zur Laufzeit einen Pfad im später gelöschten `rs-relay`-Checkout voraus.
6. Alle vier Plattformen und der gesamte bestätigte Produktumfang aus v0.3 bleiben Ziel. Die Reihenfolge technischer Nachweise ist keine Reduktion auf ein Twitch-/1080p-Minimum.
7. Es werden keine unnötigen Microservices oder Abstraktionsschichten vorab gebaut. Zuerst die benötigte Kombination messen, danach die kleinstmögliche belastbare Rust-Integration erstellen.

## Abgrenzung der Entscheidung

Die Repositoryentscheidung gilt für **Uplink**. F1–F3 bleiben offen: Aufzeichnungsquelle, VOD-Speicher, Aufbewahrung, VOD-Auslöser und zuständiges Repository werden damit nicht festgelegt. Eine speicherneutrale Übergabeschnittstelle darf entworfen werden. Ein VOD-Worker oder dauerhaftes Archiv darf nicht allein aus dieser Entscheidung als freigegeben gelten.

F4–F6 bleiben ebenfalls offen. Der Neubau legt keine beliebigen Delay-Grenzen, Wartebildfristen, OBS-/HDR-Freigaben oder Audio-Ersatzregeln fest. Nicht abhängige Grundlagen können ohne erneute pauschale Nutzerfreigabe gebaut werden.

## Übergang und Löschung

Der neue Kern wird von Anfang an unabhängig aufgebaut; der alte Kern ist kein Bestandteil dieses neuen Produkts. Der bestehende Dienst darf währenddessen weiterlaufen. Vor seiner Ablösung sind benötigte Nutzerdaten und Bedienfunktionen geprüft zu übernehmen, Migration und Rückkehr zu erproben und die relevanten Live-Abnahmen nachzuweisen.

Der Nutzer beabsichtigt, das bisherige Repository später selbst zu löschen. Dieses ADR ist keine Behauptung, dass dessen Löschung bereits erfolgt oder der neue Dienst einsatzbereit wäre. Vor endgültiger Entfernung müssen verbleibende Abhängigkeiten und Rückkehrmöglichkeiten geklärt sein. Es wird keine zusätzliche Freigaberunde für bereits autorisierte, reversible Entwicklungsarbeit eingeführt.

## Konsequenzen

- Der Altbestand kann nicht als Kapazitäts-, Sicherheits- oder Interoperabilitätsnachweis dienen.
- Eine erfolgreiche lokale Profilplanung beweist weder RTMPS-Eingang noch Encoderbetrieb noch einen öffentlichen Livestream.
- Produktzustände müssen zwischen gewünschter, geplanter und tatsächlich gemessener Ausgabe unterscheiden.
- Erhaltenswerte Oberflächen bekommen einen nachvollziehbaren Integrationsvertrag zum neuen Rust-Backend.
- Ein produktionsfähiger Neubau ist erst nach Erfüllung der [Abnahmematrix](../abnahme.md) als solcher zu melden.
