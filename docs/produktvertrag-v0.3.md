# Uplink-Neubau – Produktvertrag und Architekturleitlinien v0.3

Stand: **8. September 2026**. Grundlage: die bis einschließlich dieser Gesprächsrunde bestätigten Nutzerantworten, der bereitgestellte Layout-Screenshot und die dokumentierten Primärquellenprüfungen. v0.3 ersetzt v0.2 als Arbeitsstand; Entscheidungen zum VOD-Speicher und dessen Repository werden ausdrücklich nicht vorweggenommen.

Dies ist der im Auftrag übergebene **Arbeitsentwurf**: bestätigte Produktziele sind von Architekturvorschlägen und noch offenen Entscheidungen getrennt. **Kein Implementierungs-, Live-Test- oder Kapazitätsnachweis.** Die Aussagen zur Quellenprüfung beschreiben den übergebenen Belegstand; bei der Übernahme in dieses Repository wurden die Quellen nicht erneut technisch geprüft.

Der Entwurf entstand vor den Änderungen dieses Neubauauftrags. Die anschließende Nutzerentscheidung für ein neues Repository und einen Rust-Neubau ist separat in [ADR 0001](adr/0001-neues-repository-und-rust-neubau.md) festgehalten. Die Aussage des ursprünglichen Entwurfs „Keine Änderungen am Repository oder am Produktivsystem durchgeführt“ beschreibt dessen Entstehungsstand und ist keine Aussage über den späteren Arbeitsfortschritt. Aktueller Umsetzungsstatus: [Abnahmematrix](abnahme.md).

## 1. Produktziel und Prioritäten

Ein Streamer überträgt möglichst nur ein hochwertiges, bandbreitensparsames Videoprogramm mit den benötigten Audiomischungen aus OBS an Uplink. Bevorzugter Codec ist AV1. Uplink verteilt kompatible Medien ohne erneutes Encoding und erzeugt nur die zusätzlich erforderlichen Ausgabevarianten. Twitch, Kick, YouTube und TikTok sind ausdrücklich Bestandteil des vollständigen Zielumfangs.

Vergleichsbasis für den Uploadnutzen ist insbesondere der vom Nutzer genannte H.264-Stream mit rund 6.000 kbit/s Video. Das Produkt soll auch Menschen mit etwa 3–5 Mbit/s verfügbarer Uploadkapazität helfen. Ungefähr 40 Prozent Einsparung sind ein Ziel, keine nachgewiesene Garantie. Vergleichbare sichtbare Qualität hat Vorrang vor einer starren Prozentzahl. Videobitrate, Gesamtupload und die Ersparnis durch nur einen statt mehrerer Uploads sind getrennt auszuweisen.

Der Umfang wird nicht auf ein 1080p-/Twitch-Minimum reduziert. Abhängigkeiten werden dennoch nacheinander bewiesen: Das ist eine technische Abnahmereihenfolge und keine Kürzung des Produktumfangs.

## 2. Bestätigte Anforderungen

| ID | Anforderung | Einordnung |
| --- | --- | --- |
| P-01 | Vollständiger Neubau des rs-relay-Repositorys statt Reparatur des bisherigen Medienkerns | Bestätigt |
| P-02 | Bestehende Oberfläche, Kontoverbindungen, Chat und OBS-Docks dürfen nach Prüfung weiterverwendet werden | Bestätigt; kein pauschaler Qualitätsnachweis für den Bestand |
| P-03 | Kein SRT im neuen Produkt; TCP-basierter RTMP-Weg | Bestätigt |
| P-04 | Enhanced RTMP mit RTMPS als gewünschter verschlüsselter Eingang | Durch aktuelle Antwort befürwortet |
| P-05 | AV1 bevorzugt; H.264 ebenfalls unterstützen und bei Kompatibilität durchreichen | Bestätigt |
| P-06 | HEVC nicht aus technischen Gründen ausschließen; als weiterer Codec vorsehen | Architekturvorschlag aus „AV1 und anderes Zeug“, konkrete Codec-Matrix noch freizugeben |
| P-07 | Variable Ausgabeprofile anhand von Eingang und Ziel; 1080p und 2560×1440 gehören ausdrücklich dazu, sind aber keine abschließende Liste | Bestätigt; genaue Codec-/Bildraten-/HDR-Matrix und technische Obergrenzen noch festzulegen |
| P-08 | Bandbreitenbegrenzte Live-Profile; CBR als Standard, HQ-CBR bei geeigneten Encodern prüfen | Bestätigte Richtung; keine pauschale Encoderfreigabe |
| P-09 | Nur notwendige Umwandlungen; identische Videoausgaben einer Session gemeinsam encodieren | Bestätigt |
| P-10 | Twitch, Kick, YouTube und TikTok vollständig in den Zielumfang aufnehmen | Bestätigt; Plattformzugänge und Einzelrechte bleiben Abhängigkeiten |
| P-11 | Twitch Enhanced Broadcasting, 1440p, Live-/VOD-Audio und Hochkant berücksichtigen | Bestätigt |
| P-12 | Vergleichbare native Plattformfunktionen auf anderen Zielen berücksichtigen | Bestätigt; keine Annahme identischer Protokolle |
| P-13 | Serverbasierte Hochkant-Aufbereitung aus dem fertigen Stream mit Gameplay-/Kamera-Ausschnitten wie im Screenshot | Standardweg bestätigt; keine zusätzliche Kameraübertragung oder KI-Verfolgung daraus ableiten |
| P-14 | Monitoring pro Nutzer, Verarbeitung und Server sowie belastbare Skalierungsdaten | Bestätigt; keine feste Nutzerzahl vom alten System übernehmen |
| P-15 | Verständliche Fehler, Pufferung, Wartebild und Wiederverbindung | Bestätigt; konkrete Grenzen und Abbruchregeln noch festzulegen |
| P-16 | Normaler Betrieb möglichst ohne zusätzliche OBS-Erweiterung | Klare Nutzerpräferenz, kein absolutes technisches Verbot; Ziel ist kein Pflicht-Plugin |
| P-17 | Streamer können zusätzliche Uplink-Verzögerung im Dashboard variabel einstellen | Bestätigt; Wertebereich, aktive Änderungen und Verhältnis zur Pufferreserve werden definiert |
| P-18 | YouTube-Kanal in Uplink verbinden und VOD-Upload als echte Produktfunktion vorsehen | Bestätigt; Quelle, Auslöser und Speicherung noch offen |
| P-19 | VOD-/Exportfunktionen ohne vorzeitige Festlegung auf ein Repository oder Speicherprodukt planen | Bestätigte offene Organisationsentscheidung; Schnittstelle jetzt entwerfen |
| P-20 | TikTok-Testaccount vorhanden | Vom Nutzer bestätigt; konkrete RTMP-/Chat-/App-Rechte noch nicht technisch nachgewiesen |

## 3. Protokoll, Codec und Qualität getrennt modellieren

Enhanced RTMP erweitert das RTMP-/FLV-Mediensignalisierungsformat unter anderem um AV1, HEVC und Mehrspur-Audio/-Video. RTMPS ergänzt den TLS-geschützten Transport. H.265 ist daher nicht auf SRT beschränkt. [S1]

AV1 ist der Codec, 1440p die Auflösung und HQ-CBR ein herstellerspezifischer Rate-Control-Modus. Diese Begriffe sind keine alternativen Gesamtformate. Ein Profil setzt sie zusammen, beispielsweise AV1 / 2560×1440 / 60 fps / geprüfter CBR-Modus / definiertes Bitratenbudget / explizite Audioauswahl.

HQ-CBR ist unter anderem in AMD AMF für AV1 dokumentiert und benötigt dort PreAnalysis. AMD beschreibt Abwägungen bei Puffern, subjektiver Qualität und Bitratengenauigkeit. Das rechtfertigt Tests, aber nicht die Annahme „immer höchste Qualität ohne zusätzliche Kosten“. [S2, S3]

Vorschlag: plattform- und hardwarebezogene, versionierte Presets statt globaler freier FFmpeg-Parameter. CQP und unbeschränktes VBR sind nicht das Standardprofil für eine knappe Uploadleitung. Begrenzt variable Verfahren sind nicht grundsätzlich sinnlos, werden hier aber nicht anstelle der gewünschten CBR-Profile zum Standard gemacht.

Eine gegebenenfalls gewählte adaptive Absenkung des Zielbitratenbudgets wäre eine eigene Produktfunktion; sie ist nicht dasselbe wie ein unbeschränktes VBR-Profil und darf nicht stillschweigend aktiviert werden.

TLS-Mehraufwand wird im Test gemessen, einschließlich neuer Verbindungen. Es gibt keine unbelegte „kostenlos“-Zusage. Ver- und Entschlüsselung ersetzen keine Authentifizierung, Schlüsselrotation oder Ressourcenbegrenzung. Ausgänge sollen TLS verwenden, soweit der konkrete Plattformendpunkt dies unterstützt; unverschlüsselte Ausnahmen wären explizit zu dokumentieren.

## 4. Medienverarbeitung: gemeinsam nach Ausgabeprofil, nicht nach Plattform

### 4.1 Vorgeschlagene Struktur

- **Eingangs- und Sessionverwaltung:** Authentifizierung, Trackregistrierung, Validierung, Zeitbasis, Ressourcenlimits.
- **Medienverarbeitung:** Passthrough, Decoding, Skalierung, Komposition, Encoding und Audiobearbeitung.
- **Plattformausgänge:** eigene Sessions, Verpackung, Trackzuordnung, Status, Puffer und Wiederverbindung je Ziel.
- **Steuerung und Produktintegration:** Konten, Berechtigungen, Ziele, Layouts, Chat, Aktivitäten, Dashboard und OBS-Docks.

Diese Verantwortlichkeiten sollen unabhängig testbar sein. Daraus folgt nicht, dass jede Verantwortung ein eigener Netzwerk-Microservice werden muss. Codec- und TLS-Implementierungen werden nicht aus Prinzip neu erfunden. Die Medienbibliothek oder Worker-Lösung wird erst nach Nachweis der benötigten Kombination gewählt, insbesondere E-RTMP-Mehrspur-Audio, AV1, Twitch-Ausgänge und Failover.

### 4.2 Gemeinsame Verarbeitung

Bei nötigem Transcoding wird eine Video-Eingangsspur möglichst einmal dekodiert. Mehrere Filterpfade können darauf aufbauen. Pro eindeutigem Video-Ausgabeprofil wird ein Encode erzeugt und dessen komprimiertes Ergebnis an passende Ziele verteilt. FFmpeg beschreibt den Unterschied zwischen Streamcopy, Filtern und verlustbehaftetem Transcoding; die Architekturregel ist eine daraus abgeleitete Designentscheidung. [S4]

Der Identitätsschlüssel eines Video-Ausgabeprofils muss mindestens Session/Quellspur, Layoutrevision, Auflösung, Bildrate, Codec und Profil/Level, Farbraum/Transferfunktion/Bit-Tiefe, Rate-Control-/Bitratenparameter sowie GOP-/Keyframe-Anforderungen berücksichtigen. Die konkrete technische Darstellung bleibt offen.

Audio-Routing und Plattformverpackung gehören nicht automatisch zum Video-Encode-Schlüssel. Derselbe Videobitstrom darf mit unterschiedlichen kompatiblen Audioauswahlen verschickt werden. Unterschiedliches Audio allein ist kein Grund, das Video neu zu encodieren.

Beispiel, nicht zugesagte feste Leiter:

| Medienergebnis | Mögliche Verbraucher | Bedingung |
| --- | --- | --- |
| Originaler AV1-Bitstrom | YouTube | Gesamtes Profil wird akzeptiert; keine Bildänderung nötig |
| H.264 1920×1080 Querformat | Kick, passende Twitch-Ausgabe, optional YouTube/TikTok Querformat | Alle Anforderungen und Qualitätsziele tatsächlich vereinbar |
| HEVC 2560×1440 | Twitch-1440p-Ausgang | Konkrete Enhanced-Broadcasting-Konfiguration und Kanalberechtigung |
| H.264 1080×1920 Hochkant | TikTok, passende Twitch-/YouTube-Hochkant-Ausgaben | Identisches Layout und kompatible Ausgabeprofile |

Ein Querformat- und ein Hochkantbild sind nicht derselbe Encode. Ein gemeinsamer Decode ist möglich; die geänderten Bildinhalte brauchen eine passende eigene codierte Ausgabe. Auch verschiedene 1080p-Bitraten oder Keyframe-Anforderungen können getrennte Encodes erfordern. Die Twitch-Leiter darf nicht als universelle feste Leiter festgeschrieben werden. [S4, S5]

Keine erzwungene Qualitätsverschlechterung aller Plattformen auf den kleinsten gemeinsamen Nenner. Wiederverwendung findet innerhalb kompatibler Qualitätsprofile statt. Vier Ausgänge bedeuten trotz Encoderwiederverwendung weiterhin mehrfachen ausgehenden Netzwerktraffic.

### 4.3 Passthrough-Regel

Passthrough wird für Video und Audio getrennt entschieden. H.264-Eingang bedeutet nicht automatisch Passthrough auf jeder Plattform: Skalierung, anderer Bildausschnitt, anderes Farbsignal oder eine nicht passende Bitrate können trotzdem Umwandlungen verlangen. Kopieren gilt nur bei passendem Gesamtprofil. AV1 für YouTube ist ein Kandidat, keine pauschale Garantie für jeden beliebig niedrig codierten Eingang. [S4, S6]

Keine Cross-Tenant-Wiederverwendung privater Medienpuffer. Innerhalb einer Session gemeinsam verwendete Worker müssen bei Messung und Abrechnung eindeutig zugeordnet werden.

### 4.4 Variable Ausgabeprofile statt fest verdrahteter Auflösungsstufen

Bestätigte Produktregel: Im Dashboard wählen Streamer die gewünschten Ausgaben anhand dessen, was sie tatsächlich einsenden und was das jeweilige Ziel verarbeiten kann. 1440p wird eindeutig als 2560×1440 bezeichnet; „2K“ ist hier keine andere zusätzliche feste Profilklasse. Die Architektur wird nicht auf zwei Auflösungen begrenzt.

Vorschlag für die Profilprüfung: Eingangseigenschaften × Ziel-/Kontofähigkeiten × verfügbare Medienverarbeitung × Ressourcenfreigabe. Das Ergebnis enthält nicht nur erlaubt/verboten, sondern auch den Grund sowie den Unterschied zwischen gewünschtem und tatsächlich aktivem Profil. Eine passende Zielauflösung allein garantiert keine ausreichende Bildqualität bei zu wenig Eingangsbitrate.

Beispiele, jeweils abhängig von bestätigten Zielmöglichkeiten: Ein 1440p60-Eingang kann unverändert, auf 1080p60 oder auf 720p30 ausgegeben werden. Ein 1080p30-Eingang wird nicht ohne Kennzeichnung als natives 1440p60 angeboten. Hochskalierung oder Frame-Wiederholung erzeugen keine zusätzlichen ursprünglichen Details bzw. Bewegungszeitpunkte; ob eine ausdrückliche Upscale-Option angeboten wird, ist offen. Beim Hochkantbild zählt der tatsächliche Ausschnitt, nicht allein die volle Quellauflösung.

Vor dem ersten Ingest können Nutzer ein Wunschprofil speichern. Die verbindliche Prüfung erfolgt mit Teststream oder echtem Eingang; ein nicht gemessener Eingang wird nicht als bestätigt dargestellt. Ändert sich das Quellprofil bei einem Reconnect, werden Ausgaben erneut geprüft. Kein stilles Überschreiben der gespeicherten Wünsche und keine unbemerkte Änderung laufender Ziele.

„Unterschiedliche Eingänge unterstützen“ bedeutet weder beliebige ungeprüfte Codecs noch unbeschränkte Auflösung, Trackzahl oder Bitrate. Technische Grenzen müssen begründet, konfigurierbar und verständlich angezeigt werden. HDR-Unterstützung, besonders hohe Bildraten und die konkrete höchste freigegebene Auflösung sind mit dieser Antwort nicht automatisch zugesagt.

## 5. Plattformen und derzeit belegte Grenzen

### Twitch

Twitch kündigt am 17. Juni 2026 Dual Format für alle Streamer sowie 1440p über Enhanced Broadcasting für Affiliates und Partner an. Der 1440p-Weg nutzt HEVC. Die Meldung nennt außerdem serverseitige Transcodes für Partner und viele Affiliates. [S5]

Für Uplink folgt daraus: serverseitiger Twitch-Ausgang als eigenständige Integration, mit korrekter Sitzungskonfiguration, Trackzuordnung, Audio-Rollen und tatsächlicher Prüfung von Querformat, Hochkant und VOD. AV1-Eingang zu Uplink darf nicht mit einem generell verfügbaren AV1-Ausgang zu Twitch gleichgesetzt werden.

Der Streamer soll nicht als Voraussetzung mehrere Twitch-Qualitätsvarianten über seine knappe Leitung hochladen. Die erforderlichen Ausgabevarianten gehören auf die Serverseite, soweit das gewählte Twitch-Verfahren sie verlangt. Offizielle OBS-Direktunterstützung beweist noch nicht, dass die eigene Serverimplementierung vollständig interoperabel ist.

### YouTube

YouTube dokumentiert H.264, HEVC und AV1 für RTMP(S). Native Dual-Stream-Funktionen sind ebenfalls dokumentiert: selbst erzeugtes Hochkant kann über einen zweiten Stream-Key eingespeist werden; Zugriff und Oberfläche können sich kanalabhängig unterscheiden. Der native gemeinsame Stream ist nicht mit zwei unabhängigen Live-Events gleichzusetzen. [S6, S7]

Der Produktadapter muss daher Kanal-Fähigkeiten, Broadcast-Lifecycle, Verbindung der Ingest-Streams, passende Audioauswahl sowie Quer-/Hochkant-Synchronisation beherrschen. Ein erfolgreicher RTMP-Verbindungsaufbau ist kein ausreichender Beweis, dass der gewünschte öffentliche Live-Event läuft.

### Kick

Die geprüfte offizielle Anleitung nennt H.264, CBR, maximal 1920×1080, 60 fps und 8.000 kbit/s. Daraus wird kein künstlicher 1440p-Ausgang abgeleitet. 1440p-Unterstützung im Produkt bedeutet hier gegebenenfalls 1440p-Eingang mit sauberem 1080p-Ausgang. [S8]

### TikTok

Eine offizielle TikTok-Shop-Anleitung beschreibt OBS-Streaming mit RTMP-Adresse und Stream-Key. Sie ist kein allgemeiner Nachweis universeller Accountfreischaltung, aktueller Codecgrenzen oder einer öffentlich nutzbaren vollständigen LIVE-Chat-/Moderations-API. [S9]

Bei der Prüfung des öffentlichen TikTok-Entwicklerkatalogs wurde keine vollständige, allgemein freigeschaltete LIVE-Chat-/Moderationsschnittstelle als Grundlage für diese Anwendung belegt. Es handelt sich um eine offene Nachweisfrage, nicht um die Behauptung, dass keinerlei Partner- oder andere Integration existiert. Login Kit allein beweist keine LIVE-Rechte. [S10]

Ein TikTok-Testaccount ist laut aktueller Nutzerantwort vorhanden. Daraus allein folgen noch keine bestätigten RTMP-, Chat-, Moderations- oder Partnerrechte. TikTok bleibt im vollen Zielumfang. Video, Kontoverbindung, Chat-Empfang, Chat-Senden, Moderation, Titel und Aktivitäten sind einzeln am autorisierten Account zu verifizieren. Vor Produktfreigabe ist eine tragfähige Integration festzulegen; nicht öffentlich garantierte Verfahren dürfen nicht unbemerkt zur tragenden Abhängigkeit werden.

## 6. Audio: Live und VOD sind eigenständige Rollen

Das Grundmodell sieht mindestens zwei sauber zugeordnete Audiofeeds vor, sofern getrennte VOD-Audios gewünscht sind: Live-Mix und VOD-Mix. Eine Stereo-Audiospur ist weiterhin eine einzige Spur; linker und rechter Kanal ersetzen keine zwei unabhängigen Mischungen.

OBS dokumentiert die Möglichkeit, Live und Twitch-VOD unterschiedliche Audiofeeds zuzuordnen. Enhanced RTMP kann Mehrspur-Audio transportieren. OBS nennt diese Unterstützung seit Version 30.2. [S1, S11, S12]

Die tatsächliche Weiterleitung an einen eigenen Dienst ist separat zu testen. Im am 8. September erneut geprüften OBS-Entwicklungsquellstand wird die VOD-Spur unter anderem anhand der Dienstkonfiguration und des Flags `EnableCustomServerVodTrack` gesteuert. Das ist ein konkreter Ansatz für einen Weg ohne Zusatz-Plugin, aber kein Nachweis der gesamten AV1-/RTMPS-Kette für jeden veröffentlichten OBS-Stand. Ein Browser-Dock oder eine neue URL allein garantiert den Mehrspurweg nicht. [S13]

Nutzerpräferenz: keine verpflichtende OBS-Erweiterung. Standard-OBS, dokumentierte Einstellungen und eine verständliche Einrichtung sind zuerst zu prüfen. Falls eine relevante Funktion damit nicht zuverlässig erreichbar ist, wird diese konkrete Lücke vorgelegt; weder ein Plugin noch der Verlust der VOD-Spur werden stillschweigend beschlossen. Twitch-Ausgabeleitern entstehen serverseitig und setzen kein klientseitiges Enhanced-Broadcasting-Multi-Encode voraus. Unterstützte OBS-Versionen und Betriebssysteme bleiben Teil des Abnahmenachweises.

Architekturvorschläge:

- Explizite Zuordnung zwischen OBS-Mixerwahl, Eingangs-Track-ID, interner Rolle und Plattform-Track-ID; keine zufällige Gleichsetzung von Spurindizes.
- Kompatible AAC-Pakete weiterreichen statt unnötig neu codieren; resamplen oder neu codieren nur bei Bedarf. [S4]
- Gemeinsame Medienzeitbasis und Driftüberwachung; Neustart, Codec-Header und Stille pro Spur korrekt behandeln.
- Je Ziel festlegen, welcher Mix live verwendet wird; Twitch zusätzlich VOD-Mix zuordnen. Nicht jeder Dienst unterstützt dieselbe VOD-Trennung.
- Fehlende gewünschte VOD-Spur nie stillschweigend durch den Live-Mix ersetzen. Eindeutige Warnung und vorher vereinbartes Verhalten; eine stille Ersatzspur nicht als erfolgreich übertragener VOD-Ton ausgeben.
- Beim Wartebild und beim Rückwechsel gelten dieselben Audioregeln. Alte Audiodaten dürfen nicht in die neue Session gelangen.

Der Server kann aus einer einzigen bereits gemischten Tonspur nicht deterministisch die ursprünglichen Einzelquellen wiederherstellen. Benötigte getrennte Mischungen müssen deshalb an der Quelle vorliegen. Die VOD-Spur allein ist kein Archiv. Neu bestätigt ist die geplante Möglichkeit, VODs über Uplink zu YouTube hochzuladen. Dafür benötigt der Export eine tatsächlich verfügbare Videoquelle mit eindeutigem Audiomix; ob Uplink selbst aufzeichnet oder ein anderes Modul die Quelle liefert, bleibt offen. Siehe Abschnitt 10.

## 7. Hochkant: Layouteditor und Herkunft der Bilddaten

Der Screenshot ist eine Bedienreferenz: Quellbild mit getrennten Crop-Bereichen für Gameplay und Kamera, Hochkant-Vorschau, PiP/Stacked, Kamera an/aus sowie speicherbare Konfiguration. Im ursprünglichen Entwurf wurde keine neue Oberfläche erzeugt oder der Screenshot verändert. Der Screenshot wurde diesem Repository bislang nicht als Bilddatei beigefügt; seine Beschreibung ist kein visueller Abnahmenachweis.

Der erwähnte Ansatz entspricht wahrscheinlich Aitum Vertical, dessen Herstellerbeschreibung einen eigenen Canvas und verknüpfte Szenen beschreibt. [S14]

Bestätigter Standardweg: ein fertiges Videoprogramm, serverseitige Ausschnitte. Der Server schneidet Gameplay und Kamera aus dem eingehenden Bild aus und setzt daraus die Hochkantversion wie im Screenshot zusammen. Das erhält den Ein-Video-Upload als Standard. Verdecktes Gameplay ist im zusammengesetzten Eingang nicht vorhanden; eine klein eingebrannte Kamera wird durch Vergrößerung nicht detailreicher.

Eine zusätzliche unabhängige Kamera oder ein separater Hochkant-Canvas ist in dieser Antwort nicht beauftragt und kein Pflichtbestandteil. Eine spätere Erweiterung ist möglich, muss aber eigene Kosten und Anforderungen offenlegen. Automatische Motiverkennung, KI-Tracking und zuverlässige Erkennung beliebiger OBS-Szenen werden ebenfalls nicht aus „so soll es funktionieren“ abgeleitet.

Für den Layouteditor vorgeschlagen: normierte Koordinaten, Gameplay-/Kamera-Crop, Kamera an/aus, PiP/Stacked, Seitenverhältnisprüfung, Live-Vorschau, gespeicherte Layoutprofile, versionierte Speicherung und ein definierter Übernahmezeitpunkt. Die Konfiguration wird serverseitig auf den Eingang angewendet, ohne dass dafür ein Plugin vorausgesetzt wird. Automatischer Wechsel zwischen OBS-Szenenprofilen braucht eine gesondert nachgewiesene Informationsquelle; andernfalls bleiben gespeicherte Profile im Dashboard bzw. Dock bewusst auswählbar.

Browser-Vorschauen gehören in ein eigenes Ressourcenbudget. Kein unbegrenzter zusätzlicher Encode für jedes offene Dock. Die konkrete Vorschauübertragung, ihre Codec-Kompatibilität und ein gegebenenfalls ebenfalls TCP-basierter Weg bleiben eine Implementierungsentscheidung.

## 8. Puffer, Cache und Ausfallverhalten

TCP liefert einen geordneten zuverlässigen Bytestrom und überträgt verlorene Daten erneut. Daraus folgt, dass eine gestörte Verbindung die Bereitstellung späterer Mediendaten verzögern kann. Der TCP-Wechsel ersetzt deshalb kein Latenz- und Staukonzept. [S15]

Vorgeschlagen sind getrennte Mechanismen:

| Mechanismus | Zweck | Grenzen |
| --- | --- | --- |
| Eingangs-/Jitterpuffer | Kurze Lieferschwankungen ausgleichen | Zeit- und Bytegrenze; gemessene statt unbegrenzt wachsende Verzögerung |
| Codec-/Keyframe-Cache | Gültiger Einstieg nach neuem Ausgang oder Reconnect | Je Track und Sessiongeneration; Initialisierungsdaten und benötigte Referenzen |
| Ausgangspuffer | Einzelne Plattform entkoppeln | Langsames Ziel blockiert keine anderen Ausgänge; definierter Neustart statt Aufstauen |
| Decodierter Frame-Pool | Gemeinsame Filter und Komposition | Kurzlebig und hart begrenzt, kein Langzeit-Rohbildarchiv |
| Metadaten-/Emote-Cache | Wiederholte API-Aufrufe vermeiden | Mandantentrennung, Ablaufzeit, Invalidierung |
| Chat-Replay/Entdoppelung | Dock-Reconnect und Event-Wiederholung | Begrenzte Aufbewahrung; Löschungen/Moderationsereignisse übernehmen |
| Wartebild-Assets | Vorbereitete Fallback-Ausgabe | Format- und Audio-kompatibel, begrenzter Speicher |

Ein GOP-/Keyframe-Cache ist nicht automatisch ein zusätzlicher Wiedergabeverzug; absichtliches zeitversetztes Ausspielen ist eine andere Funktion. Ein Puffer kann Schwankungen überbrücken, aber eine dauerhaft zu schmale Uploadleitung nicht vergrößern.

Die zusätzliche Uplink-Verzögerung wird im Dashboard vom Streamer eingestellt. Keine globale Festlegung auf die zuvor lediglich vorgeschlagenen zwei Sekunden. Ebenfalls wurden keine 60 Sekunden Wartebild verbindlich festgelegt.

Vorgeschlagene Semantik: Ein sichtbares Ziel für den zusätzlichen zeitlichen Versatz innerhalb Uplinks; darin werden verwendete Pufferreserven nachvollziehbar berücksichtigt, nicht als versteckter zweiter Delay addiert. Anzeige: gewähltes Ziel, aktuell gemessener Uplink-Anteil, Pufferfüllstand und Zustand der Verbindung. Plattform-/Playerverzögerung und bereits vor Uplink entstandener Versatz sind getrennt und ohne Messnachweis unbekannt. „Minimal“ bedeutet kein absichtlich hinzugefügter Delay, nicht null Ende-zu-Ende-Latenz.

Kurze Stabilitätspuffer, absichtlicher längerer Stream-Delay und Wartebild-/Reconnect-Frist sind separate Begriffe. Die genaue Bedienaufteilung, Grenzen und sinnvolle Voreinstellungen werden noch festgelegt. Ein langer Delay benötigt ein zeit- und bytebegrenztes Konzept für komprimierte Medien und gegebenenfalls persistenten Speicher statt beliebig wachsender Rohbildpuffer.

Ein laufender Wechsel des Delay kann Auswirkungen auf die ausgespielte Zeitlinie haben. Vergrößerung benötigt zusätzliche Füllzeit; Verkleinerung erfordert einen definierten Abbau bzw. Sprung. Keine unbemerkten Inhaltsverluste, Audio-/Video-Verschiebungen oder Versprechen eines folgenlosen Live-Schiebereglers. Vorschlag: Einstellungen zunächst für den nächsten Stream übernehmen; eine aktive Übernahme ist nur mit expliziter, getesteter Semantik verfügbar. Ob aktive Änderung zwingend erforderlich ist, bleibt offen.

Das angestrebte Ausfallverhalten lautet: kurzen Aussetzer aus vorhandener Reserve abfangen, danach definiertes Wartebild mit korrekt gewähltem Ton, autorisierten Reconnect übernehmen, auf einen gültigen Einstiegspunkt warten und kontrolliert zurückschalten. Nach Ablauf einer klaren Frist kontrolliert beenden. Ein Puffer überbrückt nur tatsächlich vorhandene Medien und ersetzt keine dauerhaft ausreichende Verbindung.

Passthrough plus nahtloses Wartebild ist ein gesonderter Nachweis. Gleiches Bildmaß und Codecname garantieren noch keinen nahtlosen Wechsel zweier unabhängig codierter Bitströme. Zeitstempel, Initialisierungsdaten, Decoderzustand und Audiospuren müssen passen. Wo das konkrete Ziel einen Reconnect verlangt, muss das Produkt dies korrekt anzeigen und darf keine unbewiesene nahtlose Übertragung versprechen.

Ein TCP-Close ist nicht immer eine zuverlässige Aussage über die Absicht des Streamers. Explizites Stream-Ende, OBS-Absturz, Neuverbindung und bewusster Profilwechsel brauchen definierte Semantik. Eine OBS-Hilfe oder ein explizites Ende-Signal könnte diese Unterscheidung verbessern; kein Pflicht-Zusatzklick ohne Produktentscheidung.

## 9. Chat, Konten und OBS-Docks

Der gemeinsame Bedienplatz soll alle vier Plattformen abdecken. Als noch detailliert freizugebende Funktionsmatrix werden vorgeschlagen: Lesen, gezieltes Schreiben als berechtigtes Konto, Emotes/Badges, gelöschte Nachrichten, Timeouts/Bans, Titel-/Kategorieverwaltung sowie plattformspezifische Aktivitäten. Plattformen haben unterschiedliche Funktionen und Berechtigungen; eine nicht existierende Funktion ist nicht durch einen generischen Button zu ersetzen.

Twitch dokumentiert EventSub und API als bevorzugten Chatweg. YouTube dokumentiert Chatempfang, serverseitigen Streamingempfang sowie Schreiben und Löschen. [S16, S17] Die konkrete Kick- und TikTok-Chatintegration benötigt separate Funktionsnachweise; in diesem Entwurf wird keine vollständige Berechtigungsfreigabe behauptet.

Vorschläge: ein zentraler Datenstrom pro autorisiertem Kanal statt unabhängiger Plattformabfragen je Dock; Entdoppelung anhand der passenden Ereignisidentität; Bestätigungen und Fehlermeldungen bei Schreibaktionen; gezielte Nachrichtenzustellung statt automatischer Crossposting-Schleifen. Ereignisse mit derselben ID können Updates darstellen und dürfen nicht pauschal verworfen werden. YouTube dokumentiert beispielsweise wiederverwendete IDs für Gift-Updates. [S17]

Chatbedienung für den Streamer und ein öffentlich eingebranntes Multichat-Overlay sind unterschiedliche Funktionen. Der Screenshot-Auftrag erteilt keine automatische Freigabe, sämtliche Chats oder private Betriebsinformationen in jedes Plattformvideo einzubrennen.

Wiederverwendete Oberfläche und Kontoverbindungen bekommen Vertrags- und Sicherheitstests. Keine zweite unabhängige Tokenverwaltung oder parallele Wahrheitsquelle nur wegen des Neubaus. Übergang und Datenmigration werden geplant; kein ungeprüftes Löschen bestehender Nutzerkonfigurationen.

## 10. VODs, Speicherung und YouTube-Upload

### 10.1 Bestätigter Umfang und offene Zuständigkeit

Uplink soll eine YouTube-Kanalverbindung und die Möglichkeit zum VOD-Upload anbieten. Dies wird jetzt als Produkt- und Schnittstellenanforderung aufgenommen, nicht nur als unverbindlicher UI-Platzhalter. Das Repository, der Speicheranbieter und die konkrete Aufzeichnungsquelle sind noch nicht entschieden. Weder ein dauerhaftes Komplettarchiv noch ein automatischer Upload aller Streams ist damit genehmigt.

Live-Restreaming zu YouTube und das spätere Hochladen eines VODs sind zwei unabhängig aktivierbare Funktionen. Vorschlag: Ein Nutzer kann YouTube ausschließlich als VOD-Ziel verbinden, ohne dort live zu senden. Ein gesonderter VOD-Kanal darf als eigene autorisierte Verbindung modelliert werden; seine Unterstützung als Bedienfunktion ist noch abzustimmen. Die Kanalidentität wird aus der autorisierten Verbindung bestätigt und nicht beliebig aus einer eingegebenen Channel-ID ersetzt.

### 10.2 Vorgeschlagene Komponentengrenzen

| Verantwortung | Vertrag, ohne Festlegung auf ein Repository |
| --- | --- |
| Uplink-Steuerung/Dashboard | Kanalverbindung, geprüfte Berechtigungen, VOD-Auswahl, Profil, Audiomix, Metadaten und Uploadstatus |
| Medien-/Aufzeichnungskomponente | Tatsächlich vorhandenes Video und Audio erfassen bzw. ein autorisiertes Medienobjekt bereitstellen |
| Speicheradapter | Dateien/Objekte schreiben und lesen, Integrität und Zugriff schützen, Aufbewahrung und Löschung kontrollieren |
| VOD-/Exportworker | Quelle prüfen, kompatibel verpacken oder nötigenfalls verarbeiten, YouTube-Upload fortsetzen und Ergebnis prüfen |

Daraus folgt keine Pflicht zu vier Microservices. Die Grenzen können in einem Repository als Module umgesetzt und bei Bedarf später getrennt betrieben werden. Live-Medienworker sollen keine fremden Repository-internen Pfade voraussetzen. Die gemeinsame Konten-/Tokenverwaltung bleibt die autoritative Quelle; keine zweite unabhängige Tokenkopie nur für VODs.

Ein vorgeschlagenes Übergabeobjekt enthält Nutzer-/Session-ID, Medien-ID und Speicherreferenz, bekannte Dauer und Integritätsinformationen, Videoeigenschaften, explizite Audio-Rollen, Layoutversionen bzw. bei Änderungen eine zeitliche Zuordnung, Berechtigungskontext und eine Aufbewahrungsregel. Jobs referenzieren eine überprüfte YouTube-Verbindung sowie das gewünschte Ausgabeprofil, den Audiomix und Metadaten. Das ist ein Vertragssketch, kein fertiges Datenbankschema.

### 10.3 Quelle und Qualität

Vorschlag für den üblichen Weg: bei aktivierter VOD-Funktion den bei Uplink ankommenden komprimierten Eingang mit den benötigten Audiofeeds segmentiert sichern. Dadurch muss nicht grundsätzlich ein zusätzlicher Video-Encode laufen; Streamcopy kann vorhandene komprimierte Spuren ohne Decoding/Encoding in eine geeignete Ausgabe übernehmen. Ob das konkrete Archiv- und YouTube-Dateiprofil diese Spuren akzeptiert, ist gesondert zu prüfen. [S4]

Ein Eingangsmitschnitt ist ein Quellmaster und nicht automatisch identisch mit dem fertigen Plattformbild. Für einen späteren Hochkantexport oder die exakte Live-Komposition sind passende Layout-/Zeitinformationen oder eine tatsächlich aufgezeichnete Ausgabe erforderlich. Eine nachträglich gewünschte, nie gespeicherte Quelle darf nicht als verfügbar erscheinen.

Ein anderes Archiv-Repository kann statt des Uplink-Mitschnitts dieselbe Medienobjekt-Schnittstelle bedienen. Ein Import bereits bestehender Twitch-VODs ist eine andere Quellfunktion; Zugang, Downloadweg und Rechte wären separat zu untersuchen. Dieser Entwurf behauptet dafür keine verifizierte API und legt diesen Weg nicht stillschweigend fest.

VOD-Audio wird ausdrücklich gewählt. Vorschlag: der als VOD-Mix konfigurierte Feed ist der Standard für den Export; nie automatisch zum Live-Mix wechseln. Fehlt der erforderliche Mix, hält der Export mit einer verständlichen Meldung an. Ob und wie dadurch laufende Plattformausgänge beeinflusst werden dürfen, bleibt eine eigene Produktentscheidung. „VOD-Mix“ bedeutet nicht automatisch Musikfreiheit oder eine urheberrechtliche Freigabe.

### 10.4 YouTube-Kanal und Übertragung

Die YouTube Data API stellt Video-Uploads und Metadaten bereit und verlangt passende OAuth-Berechtigungen; `youtube.upload` ist ein für `videos.insert` akzeptierter Scope. Welche zusätzlichen Berechtigungen Statusabfrage, Kanalprüfung und Nachbearbeitung brauchen, wird funktionsbezogen geprüft. Eine bestehende Live-Verbindung wird nicht blind als uploadberechtigt behandelt. [S18]

Für Aufträge nach Ende der Browsersitzung ist die dafür vorgesehene serverseitige OAuth-Verwaltung mit sicher aufbewahrten, soweit erteilten Refresh-Tokens zu planen. Tatsächlich gewährte Scopes werden geprüft; widerrufene/abgelaufene Berechtigungen erzeugen einen wiederaufnehmbaren, sichtbaren Handlungsbedarf. [S20]

Vorgeschlagen sind fortsetzbare Uploads. YouTube dokumentiert Upload-Sessions und das Ermitteln bereits angenommener Bytes nach einem Abbruch. Die Jobdaten speichern Session-Referenz, bestätigten Fortschritt und später die YouTube-Video-ID geschützt und dauerhaft. Nach einem Netzwerkfehler nicht blind einen neuen Upload starten. Bei unklarem Abschluss wird zunächst abgeglichen; keine unbelegte Exactly-once-Zusage über eine fremde API. [S19]

Ein vollständig übertragenes Video ist nicht automatisch erfolgreich verarbeitet oder veröffentlicht. YouTube weist eigene Verarbeitungszustände aus. Dashboardzustände und Aufräumregeln müssen diese Unterschiede abbilden. [S21]

Vorgeschlagener Ablauf: Quelle verfügbar → validiert → Ausgabe vorbereitet → Upload wartet/läuft → YouTube verarbeitet → bereit/entsprechend freigegeben; zusätzlich blockiert, fehlgeschlagen und abgebrochen. Manueller Upload und automatische Regeln nach Streamende sind als Optionen vorgesehen; Aktivierung und Standardverhalten noch offen. Veröffentlichung erfolgt nur nach einer ausdrücklichen Nutzerentscheidung bzw. autorisierten Regel, nicht allein wegen einer Kanalverbindung.

Titel, Beschreibung, Sichtbarkeit und gegebenenfalls Veröffentlichungszeit gehören zum Uploadauftrag. Zusätzliche Nachbearbeitung wie Thumbnail oder Playlist bleibt als Erweiterung modellierbar, aber aus der aktuellen Nutzerantwort nicht automatisch verpflichtend.

### 10.5 Betriebs- und Freigabekriterien

API-Projekte, die nach dem 28. Juli 2020 erstellt und nicht verifiziert wurden, unterliegen laut `videos.insert` einer Beschränkung auf private Uploads; deren Aufhebung erfordert das dort genannte Audit. Projektstatus, Kontobeschränkungen und API-Kontingente gehören deshalb in die Integrationsabnahme. Keine veralteten Quota-Zahlen hart codieren. OAuth-Freigabe und das YouTube-API-Audit nicht gleichsetzen. [S18]

Aufzeichnungsspeicher ist kein wegwerfbarer Cache. Eigene Speicherbudgets, Ablaufregeln, Zugriffsschutz und Wiederanlauf nach Absturz sind notwendig. Fehlerhafte oder unvollständige Aufnahmen dürfen nicht als vollständig exportiert angezeigt werden. Dauerhaftes Archiv, temporärer Exportzwischenspeicher und Live-Delay-Speicher werden getrennt bewertet.

Live-Verarbeitung erhält Ressourcenpriorität. VOD-Verarbeitung und Uploads haben getrennte Parallelitäts-/Netzwerkgrenzen; ein großer Upload darf Live-Ausgänge nicht ausbremsen. Voller Speicher oder ein Uploadfehler wird sichtbar behandelt, ohne die übrigen Live-Funktionen unkontrolliert abzureißen.

Quellen werden nicht allein deshalb gelöscht, weil die letzte Upload-Anfrage erfolgreich war. Aufräumen berücksichtigt bestätigten Abschluss, gegebenenfalls Verarbeitung, weitere offene Jobs und die vereinbarte Aufbewahrung. Bei harten Kapazitätsgrenzen sind Warnung und Abbruchverhalten definiert; keine unbegrenzte Speicherung als stiller Fallback. Die konkreten Fristen legt der Nutzer später fest.

## 11. Monitoring, Ressourcen und Skalierung

Keine Kapazitätszusage aus alten CPU-Punkten oder HEVC-Benchmarks ableiten. Hardwarebeschaffung liegt beim Betreiber. Der Neubau muss Daten liefern, mit denen Kapazität und Erweiterungen entschieden werden können.

Pro Session vorgeschlagene Anzeigen: tatsächlich empfangene Video-/Audiotracks, Codec/Profil, Auflösung/FPS, Bitrate und Gesamttraffic, Eingangsalter, Pufferfüllstand, Audioaktivität/Drift, Passthrough versus Encoding, Layoutversion und Ausgänge.

Pro Worker/Encode: CPU-Zeit, Speicher, gegebenenfalls GPU-Decode-/Encode-Auslastung und VRAM, Frame-Verarbeitungsdauer, ausgefallene Fristziele, verworfene Frames und Warteschlangen. Verbrauch gemeinsam genutzter Worker darf nicht für jedes Ziel vollständig doppelt gezählt werden. Direkte Messwerte, zugeordnete Anteile und Schätzungen müssen unterscheidbar sein.

Pro Plattform: Verbindung, Medienfluss, bestätigter Publikationsstatus soweit verfügbar, Codec-/Trackfehler, ausgehende Bitrate, Rückstau, Reconnects und API-/Authentifizierungsprobleme. Fehlermeldungen nennen betroffene Plattform, Ursache soweit belegt, Auswirkungen, Wiederholungsstatus und konkrete Aktion; keine Stream-Keys oder Tokens.

Serverweit: CPU/GPU/RAM/Netzwerk, aktive Sessions und einzigartige Ausgabeprofile, Engpässe, gemessene Echtzeitreserve, zu viele offene Verbindungen, Neustarts und Sicherheitsevents. „Noch X Streamer möglich“ ist eine profil- und hardwareabhängige Schätzung, kein universeller Messwert.

Zusätzlich werden Aufzeichnungsspeicher und Schreibrate, offene VOD-Jobs, Export-Rechenzeit, Uploadtraffic/-fortschritt, Wiederholungen und YouTube-Verarbeitung bzw. blockierte Berechtigungen getrennt erfasst. Datenmenge im Live-Puffer wird nicht als dauerhaft verfügbares Archiv ausgewiesen.

OBS-interne Encoding-Zeiten und das tatsächliche Zuschauer-Playback lassen sich nicht allein aus einem Servereingang exakt messen. Solche Anzeigen benötigen zusätzliche Instrumentierung oder müssen als unbekannt/geschätzt markiert werden.

Skalierungsvorschlag: neue Sessions anhand tatsächlicher Fähigkeiten und Last verteilen; laufende Sessions stabil zuordnen; Nodes kontrolliert leeren; Start/Stop und Ressourcenreservierung idempotent; keine doppelte Übernahme derselben Session. Zusätzliche benötigte Profile werden vor Aktivierung reserviert. Bei fehlender Kapazität neue Arbeit klar ablehnen oder gezielt platzieren, statt bestehende Streams unbemerkt zu verschlechtern.

Autoscaling, geplante Wartung und nahtloser Notfall-Failover sind getrennte Anforderungen. Monitoring allein erzeugt keine ausfallsichere Mehrserverarchitektur.

## 12. Sicherheit und Entwicklungsqualität

Vorgeschlagene Abnahmeregeln: verschlüsselte öffentliche Steuerung, robuste Ingest-Authentifizierung, kurzlebige/rotierbare Zugänge wo sinnvoll, serverseitiger Schutz von Plattformtokens, klare Mandantenrechte, keine Secrets in Logs, begrenzte Eingabegrößen/Tracks/Bitraten/Speicher, Limits vor teurer Decoderarbeit, Schutz gegen interne Zieladressen bei frei konfigurierbaren Ausgängen und sichere Verarbeitung von Wartebild-Uploads.

Netzwerk- und Medienparser bekommen reproduzierbare Testvektoren, Fehlerfälle und Fuzztests. Medienworker sollen keine unnötigen Rechte oder beliebigen Dateizugriff besitzen. Abhängigkeiten werden versioniert, gebaut und mit der tatsächlichen Produktkombination geprüft; ein „alle Tests grün“ im alten Repository ersetzt diese Prüfung nicht.

## 13. Abnahmen für den vollständigen Neubau

1. **Realer Eingang:** Weg ohne verpflichtendes OBS-Plugin, festgelegte OBS-Versionen/Betriebssysteme und repräsentative NVIDIA-/AMD-/Intel-Konfigurationen; AV1, H.264 und freigegebene HEVC-Profile über RTMPS. Start, lange Session, Reconnect, fehlerhafte und fehlende Tracks.
2. **Audio:** deutlich unterscheidbare Live-/VOD-Testsignale, beide korrekten Rollen in der tatsächlichen Twitch-Liveausgabe und im entstandenen VOD; zusätzlich während Hochkant und Failover. Fehlende VOD-Spur wird sichtbar behandelt.
3. **Vier Plattformen:** alle vier wirklich live, nicht nur Socket-Verbindung. Kontoverbindung, Metadaten und vereinbarte Chat-/Aktivitätsfunktionen mit echten autorisierten Testkonten.
4. **Twitch Enhanced Broadcasting:** 1440p, vereinbarte Quer-/Hochkant-Kombination, passende Leiter und Audio in einem zusammenhängenden Test. Keine Behauptung aus isoliertem AV1- oder OBS-Direkttest ableiten.
5. **Hochkant:** Crop-/Kamera-/Layoutfälle aus dem Screenshot aus nur einem fertigen Videoeingang; Auflösungswechsel, Layoutwechsel, kontrollierte Live-Übernahme und gültiges Bild auf den ausgewählten Zielplattformen. Automatischen OBS-Szenenwechsel nur dann als Abnahme verlangen, wenn dessen Informationsweg gesondert vereinbart ist.
6. **Encoderwiederverwendung:** messbarer Nachweis, dass identische Profile nur einmal encodiert werden; abweichende Profile korrekt getrennt; langsamer Ausgang beeinflusst keine anderen Ziele.
7. **Qualität und Upload:** identisches Quellmaterial; H.264-Referenz bei 6.000 kbit/s Video; AV1-Testpunkte beispielsweise 3.000/3.500/4.000/4.500 kbit/s, jeweils als Versuchswerte, nicht Presets. Vergleich nach dem serverseitigen Encode und ergänzend auf der Plattform. Hochbewegtes Gameplay, Text/HUD, Kamera und Szenenwechsel; visuelle Prüfung plus geeignete Metriken. HQ-CBR darf nicht allein wegen einer einzelnen Metrik freigegeben oder verworfen werden. [S2, S3]
8. **Störungen und Dauerlast:** Paketverlust/Verzögerung, Bandbreitendelle, dauerhaft zu kleiner Upload, einzelner Plattformausfall, Tokenablauf, fehlende VOD-Spur, Decoder-/Encoderneustart, Limits und Speicher über lange Sessions. Zeitstempel und Referenzbilder bleiben gültig.
9. **Betrieb und Migration:** per-Nutzer-Messwerte, kontrollierte Node-Abschaltung, Lastschutz, sichere Rückkehr auf den vorherigen Produktstand und geprüfte Datenmigration.
10. **Variable Profile:** unterschiedliche freigegebene Eingangsauflösungen und Bildraten; gespeichertes Wunschprofil versus gemessene Quelle; zulässige/abgelehnte Ausgaben mit Grund; erneute Prüfung nach Profilwechsel; keine irreführende Kennzeichnung hochskalierter Qualität.
11. **Einstellbarer Delay:** verschiedene gewählte Werte, tatsächliche interne Verzögerung, Zeit-/Speicherlimits, Pufferleerstand, Wiederanlauf und Audio-Synchronität. Bei aktiver Änderung klare Übernahmeregel ohne verdeckten Sprung; keine Ende-zu-Ende-Latenz behaupten, die nicht gemessen wurde.
12. **VOD-Export:** bestätigter Quellweg, richtige VOD-Audiospur, gegebenenfalls Layout, autorisierter YouTube-Kanal, fortsetzbarer Upload nach Worker-/Netzwerkausfall, unklarer Abschluss ohne blinden Doppelupload, Verarbeitungsergebnis und vereinbarte Veröffentlichung. Test mit geschlossenem Dashboard, Tokenwiderruf, Quoten-/Speicherfehlern und erfolgreichem Aufräumen nach Richtlinie. Live-Streams bleiben unter paralleler Exportlast stabil.

Testaufbau, Software-/Treiberstände, verwendete Profile und Messergebnisse werden zusammen versioniert. Solange ein Abnahmepunkt fehlt, ist er als offen auszuweisen; es wird keine Funktion durch einen UI-Schalter als fertig deklariert.

## 14. Noch benötigte Produktentscheidungen

Die OBS-Präferenz, der Hochkant-Standard aus dem fertigen Stream, die variable Delay-Einstellung, der vorhandene TikTok-Testaccount und die nicht auf 1080p/1440p begrenzte Profilwahl werden nicht erneut als unbeantwortet behandelt.

| ID | Offene Entscheidung | Bereits feststehend / Abgrenzung |
| --- | --- | --- |
| F1 – VOD-Quelle | Soll Uplink bei aktivierter Funktion selbst den Eingang aufzeichnen, liefert ein anderes Archivmodul das Video oder ist ein Import vorhandener Plattform-VODs gewünscht? | Empfehlung dieses Entwurfs: direkter Eingangsmitschnitt als unabhängiger Standard; noch nicht freigegeben. |
| F2 – VOD-Auslöser und Ziel | Manueller Upload, automatische Regel nach Streamende oder beide? Derselbe YouTube-Kanal wie beim Livestream oder zusätzlich ein getrennt verbundenes VOD-Ziel? | Keine automatische Veröffentlichung ohne Entscheidung. |
| F3 – Aufbewahrung | Nur bis zum bestätigten Export, zusätzliche Wiederholungs-/Sicherheitsfrist oder dauerhaftes Archiv? | Speicherprodukt und Repository bleiben bewusst offen; eine Frist wird nicht erfunden. |
| F4 – Audio-Sicherheitsregel | Welche betroffenen Ausgaben dürfen bei fehlendem erforderlichem VOD-Mix weiterlaufen und welcher Export wird angehalten? | Fehlende erforderliche VOD-Spur bleibt sichtbar; kein stiller Live-Mix-Ersatz. Je Plattform frei auswählbarer Mix ist vorgeschlagen, nicht im Detail bestätigt. |
| F5 – Delay und Stop | Ist aktive Änderung während eines laufenden Streams erforderlich oder reicht die Wahl vor Streamstart? Gewünschter Wertebereich, Wartebildfrist und Umgang mit bewusstem Stop versus Abbruch? | Einstellbarer Delay bestätigt; keine stillschweigend festgelegten Werte oder Stop-Zusatzklicks. |
| F6 – Freigabematrix | Konkrete OBS-Betriebssysteme/-Versionen, HEVC als garantierter Eingang, höchste Bildraten/Auflösungen und HDR? | Allgemeine Auflösungsantwort ist keine HDR-Freigabe. Konkrete TikTok-Rechte werden mit dem vorhandenen Testaccount geprüft; dessen Existenz wird nicht erneut abgefragt. |

## 15. Primärquellen

S1–S17 stammen aus dem übergebenen v0.2-Arbeitsstand mit Quellenprüfung vom **7. September 2026** und sind hier als dessen Belegstand übernommen, nicht sämtlich neu geprüft. Laut übergebenem Entwurf wurden am **8. September 2026** S4 und S13 erneut eingesehen; S18–S21 wurden neu geprüft. **Diese Datumsangaben dokumentieren die übergebenen Prüfbehauptungen, keine erneute Prüfung bei der Anlage dieses Repositorys.** Plattformdokumentation kann sich ändern; einzelne Zugänge sind pro Konto zu bestätigen. Keine universelle Interoperabilität allein aus einem Standarddokument ableiten.

| ID | Quelle und übergebener Belegstand |
| --- | --- |
| S1 | [Veovera, Enhanced RTMP V2](https://veovera.org/docs/enhanced/enhanced-rtmp-v2), ausgewiesene Fassung `v2-2026-01-31-r2`. |
| S2 | [AMD AMF, Rate Control Methods](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/Rate-Control-Methods). |
| S3 | [AMD AMF, AV1 Encoder API](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_AV1_API.md). |
| S4 | [FFmpeg, Streamcopy / Transcoding / Filtering](https://ffmpeg.org/ffmpeg.html). |
| S5 | [Twitch, Introducing Dual Format and 2k Streaming on Twitch](https://blog.twitch.tv/en/2026/06/17/introducing-dual-format-and-2k-streaming-on-twitch/), 17. Juni 2026. |
| S6 | [YouTube, Live encoder settings, lokalisierte Fassung](https://support.google.com/youtube/answer/2853702?hl=es). Laut Entwurf lesbar abgerufen; englische Abrufe schlugen in jener Runde fehl. Verwendet wurden nur klare Codec-/Transportangaben, keine fragwürdigen übersetzten Empfehlungen. |
| S7 | YouTube, Get started with live streaming / Dual stream: [erste Fassung](https://support.google.com/youtube/answer/2474026?co=GENIE.Platform%3DAndroid&hl=en) und [Computer-Fassung](https://support.google.com/youtube/answer/2474026/get-started-live-streaming-computer?hl=en-GB). |
| S8 | [Kick, How to stream on KICK.com](https://help.kick.com/en/articles/7066931-how-to-stream-on-kick-com), ausgewiesener Stand 22. Mai 2026. |
| S9 | [TikTok Shop Academy UK, Using OBS Software for Livestream](https://seller-uk.tiktok.com/university/essay?knowledge_id=7738055662569218). |
| S10 | TikTok Developer Docs, öffentlicher Produktkatalog: [Get started](https://developers.tiktok.com/docs/en/get-started) und [Welcome](https://developers.tiktok.com/docs/en/welcome). |
| S11 | [OBS, Twitch VOD Track Guide](https://obsproject.com/kb/twitch-vod-track-guide). |
| S12 | [OBS Studio 30.2 Release Notes](https://obsproject.com/uk/blog/obs-studio-30-2-release-notes). |
| S13 | OBS, [AdvancedOutput.cpp](https://raw.githubusercontent.com/obsproject/obs-studio/master/frontend/utility/AdvancedOutput.cpp) und [SimpleOutput.cpp](https://raw.githubusercontent.com/obsproject/obs-studio/master/frontend/utility/SimpleOutput.cpp), gelesener Entwicklungszweig, keine Zusage für alle Releases. |
| S14 | [Aitum, eigener Plugin-Eintrag bei OBS](https://obsproject.com/forum/resources/aitum-vertical.1715/). |
| S15 | [IETF, RFC 9293, TCP](https://www.rfc-editor.org/rfc/rfc9293.html). |
| S16 | [Twitch, Chat & Chatbots](https://dev.twitch.tv/docs/chat/). |
| S17 | [YouTube Live Streaming API, LiveChatMessages](https://developers.google.com/youtube/v3/live/docs/liveChatMessages). |
| S18 | [YouTube Data API, Videos: insert](https://developers.google.com/youtube/v3/docs/videos/insert) – Berechtigungen, Upload/Metadaten, Audit-Hinweis; im v0.3-Belegstand neu. |
| S19 | [YouTube Data API, Resumable Uploads](https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol) – Upload-Sessions und Wiederaufnahme; im v0.3-Belegstand neu. |
| S20 | [YouTube Data API, OAuth for Web Server Applications](https://developers.google.com/youtube/v3/guides/auth/server-side-web-apps) – Offlinezugriff, Tokens und gewährte Rechte; im v0.3-Belegstand neu. |
| S21 | [YouTube Data API, Videos](https://developers.google.com/youtube/v3/docs/videos) – Verarbeitungsstatus und Sichtbarkeit; im v0.3-Belegstand neu. |

## 16. Änderungsstand

Die acht Antworten aus der ersten Gesprächsrunde sind verarbeitet; sie werden nicht erneut pauschal abgefragt. Neu festgehalten sind insbesondere der vollständige Repository-Neubau bei möglicher Wiederverwendung der Produktoberfläche, verpflichtender Vier-Plattform-Umfang, 1440p/Enhanced Broadcasting und VOD-Audio von Anfang an, serverseitige Hochkantkomposition, gemeinsame Encodes pro kompatiblem Profil sowie Monitoring statt einer vorab festgelegten Kundenzahl. Die verbleibenden Fragen betreffen konkrete Nutzung und technisch widersprüchliche Randbedingungen, nicht eine erneute Auswahl eines abgespeckten MVPs.

### v0.3 – Antworten vom 8. September 2026

- Keine verpflichtende OBS-Erweiterung als bevorzugter Zielweg festgehalten; VOD-Audio bleibt eine nachzuweisende Kernfunktion.
- Screenshot-basierte Hochkantkomposition aus dem fertigen Stream als Standard übernommen; zusätzliche Kameraübertragung und KI-Automatik nicht ungefragt ergänzt.
- Variable Verzögerung im Dashboard aufgenommen; frühere Diskussionswerte nicht als Freigabe fortgeschrieben.
- Frei wählbare, anhand von Eingang und Ziel geprüfte Profile statt ausschließlich 1080p/1440p ergänzt.
- Vorhandenen TikTok-Testaccount festgehalten, ohne dessen Einzelrechte zu erfinden.
- YouTube-VOD-Upload mit Kanalverbindung, Medienobjekt-/Speicherschnittstelle, richtiger Audiowahl und fortsetzbaren Jobs geplant. Repository, Quelle und Aufbewahrung bleiben offen.
- Abnahmetests und Monitoring um Profile, Delay, Aufzeichnung und Upload erweitert.

Zum Entstehungsstand des übergebenen Entwurfs wurde kein Code neu gebaut, kein Konto verbunden, kein Stream gestartet und kein Video hochgeladen. Spätere Implementierung und Nachweise werden in der [Abnahmematrix](abnahme.md) und in der Aufgabenakte dokumentiert; sie schreiben den historischen Belegstand dieses Vertrags nicht rückwirkend um.
