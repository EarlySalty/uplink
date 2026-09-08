# Dienstwechsel

Diese Vorlagen werden erst nach dem gekoppelten Medien-, Kontroll- und Docknachweis aktiviert. Sie wurden noch nicht installiert; der vorhandene Dienst läuft weiter.

Der bestehende Unitname `rs-relay.service` bleibt beim Wechsel erhalten. Damit verwendet `start-uplink.sh` denselben bereits autorisierten Credentialvertrag. Es öffnet dessen vorhandene RAM-Credential als FD 5; die Rustanwendung liest sie im Prozess und bezieht die bestehenden Dienstzugänge sowie konfigurierte TLS-Secretnamen aus Infisical. Keine neue Credentialdatei, kein ENV-Launcher und keine Zugangsdaten in Argumenten. TLS-Bereitstellung und Erneuerung müssen vor dem Start gesondert belegt sein.

Geprüftes Binary, Startskript und unabhängig installierte FFmpeg-/FFprobe-Binaries liegen versioniert unter `/opt/uplink/releases/<commit>/`; `current` verweist atomar auf den freigegebenen Stand. Die normale Konfiguration liegt unter `/home/nathanael/.config/uplink/uplink.toml`. Kein Laufzeitpfad zeigt in das alte Repository.

Vor Aktivierung werden der bisherige Unit-/Overridezustand und das unveränderte alte Binary als Rückkehrpunkt festgehalten, ohne Secrets zu kopieren. Offene Sessions erneut zählen und neue Zugänge erst nach der kontrollierten Übergabe auf den produktiven APIport zulassen. Anschließend TLS-Vertrauen, vorhandene Nutzer-/Dockzugänge, Kontoverbindungen, Ausgänge und Stop/Neustart tatsächlich prüfen.

Bei fehlgeschlagenem Wechsel den neuen Prozess stoppen, ausschließlich den neu installierten Override zurücknehmen bzw. den zuvor dokumentierten Override wiederherstellen und dieselbe User-Unit mit dem alten Binary starten. Geprüfte Zusatzmigrationen müssen diesen Rückweg offenhalten. Das alte Repository erst nach erfolgreichem Wechsel und eigenständig geprüftem neuen Laufzeitpaket entfernen.
