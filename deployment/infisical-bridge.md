# Geschützter Infisical-Zugang

Provider und Uplink lesen bzw. schreiben Infisical über `/run/uplink-infisical/api.sock`. Der bisherige Hostport `127.0.0.1:8080` ist für diese Clients kein Transport und kein Fallback. Der gemeinsame Rust-Connector prüft Socketbesitzer und Vorfahren, lehnt Symlinks und fremd beschreibbare Pfade ab und erzwingt Unixsocket, keine Proxys und keine Redirects. Produktion erwartet UID 0. Isolierte Tests verwenden ihren eigenen UID ausschließlich in privaten Verzeichnissen.

Die Rust-Bridge ist eine interne Root-Systemunit ohne TCP-Listener. Ihr Socket liegt im rootkontrollierten Runtimeverzeichnis. ACL und tatsächliche `SO_PEERCRED`-Prüfung erlauben ausschließlich UID 0 (TLS-Provider), 1000 (Uplink als nathanael) und 991 (neuer Uplink-FD9-Pfad des Dashboards als twitchdash). Der Botnutzer 995 erhält keinen Zugriff, solange kein entsprechender Caller vorhanden ist. Diese IDs sind Hostkonfiguration, keine allgemeine Produktvorgabe.

Die Bridge fragt alle zwei Sekunden genau vier öffentliche Docker-Metadaten ab: Container-ID, Running, PID und SandboxKey. Der Aufruf verwendet `/usr/bin/docker`, ausdrücklich `/run/docker.sock`, ein leeres Runtime-Konfigurationsverzeichnis, keine geerbte Prozessumgebung, drei Sekunden Frist und höchstens 512 Bytes Ergebnis. Die Formatabfrage gibt keine Container-ENV oder Geheimnisse aus. Clients bekommen niemals Docker-Zugriff.

Der Namespacepfad muss unter dem rootkontrollierten Docker-Netzverzeichnis liegen. Sein offener FD muss dieselbe Namespace wie der gemeldete Containerprozess bezeichnen; vor Freigabe werden die Docker-Metadaten erneut verglichen. Ausschließlich ein neuer Workerthread wechselt per `setns` in diesen festgehaltenen Namespace-FD. Dort verbindet er `127.0.0.1:8080`. Ein fremder Listener auf dem Hostport kann diese Verbindung nicht übernehmen. Root, der Docker-Daemon und der tatsächliche Infisical-Container bleiben die Vertrauensgrenze.

Bei gestopptem Container, Dockerfehler, fehlendem Namespace oder uneindeutiger Identität werden neue Verbindungen geschlossen. Es gibt keinen Rückgriff auf alte Hostadressen. Nach einem Container- oder Docker-Neustart werden die neue Identität und Namespace automatisch bestätigt; die nächste Verbindung nutzt den neuen FD. Alte Verbindungen bestehen ab Annahme höchstens etwa 18 Sekunden: bis zu drei Sekunden Aufbau, danach maximal 15 Sekunden Kopie. Ihr FD kann nicht auf einen anderen Namespace umgebogen werden. Maximal 16 Verbindungen laufen gleichzeitig; Kopierpuffer bleiben begrenzt. Gleichbleibende Zustandsfehler werden nur beim Übergang gemeldet.

## Bereitstellung

Vor Aktivierung sind Code und gekoppelte Service-/Dashboard-Reader unabhängig zu prüfen. Root installiert Binary, normale Config und Unit. `infisical-bridge.service` benötigt die vorhandenen Programme Docker und setfacl sowie ausschließlich die Fähigkeiten `CAP_SYS_ADMIN` für `setns` und `CAP_SYS_PTRACE` für den Namespacevergleich mit dem Containerprozess. Die Bridge verändert weder Infisical-Container noch Secrets. Ihr beschreibbarer Pfad ist allein das systemd-Runtimeverzeichnis.

Reihenfolge: Bridge starten; geschützten Socket und erlaubte Client-UIDs bestätigen; Providerconfig von `infisical_port` auf `infisical_socket` umstellen; normale Uplink- und Dashboard-FD9-Konfiguration auf denselben Socket umstellen; anschließend Provider und Dienste starten. Der erste Start erfolgt mit einem begrenzten Readinesscheck. Bei späterem vorübergehend fehlendem Socket schlagen Secretzugriffe sichtbar fehl; Service-Restart und TLS-Timer wiederholen den Zugriff. Eine Rückkehr auf ungeschütztes Host-HTTP ist kein erlaubter Rollback. Die übrigen Legacy-Bot-Infisicalwege sind nicht Bestandteil dieses Pakets.

### User-Unit und Host-UID 0

Die bestehende `rs-relay`-User-Unit erbte beim Umschaltversuch vom 9. September 2026 `PrivateTmp=yes`, `ProtectSystem=strict` und `ProtectProc=invisible`. systemd 255 erzeugt für die Mount-Sandbox einer User-Unit implizit eine Usernamespace, auch wenn `systemctl show` weiterhin `PrivateUsers=no` meldet. Deren `uid_map` und `gid_map` enthalten hier ausschließlich `1000 1000 1`. Host-root erscheint darin als UID/GID 65534. Deshalb scheitert bereits der erste Pfadbestandteil `/` an der unveränderten Besitzerprüfung. Bridge-UID, Socketbesitzer und ACL waren korrekt.

`rs-relay-override.conf` setzt diese drei geerbten Namespace-Regeln sowie `PrivateUsers` explizit zurück. Es entfällt der Mount-Schreibschutz und das private `/tmp`; `ProtectProc` wird auf die normale Prozesssicht zurückgesetzt. Der Dienst bleibt UID 1000 ohne neue Privilegien, mit UMask 0077 und den bestehenden Host-Dateirechten. Rootgeschützter Socketpfad, Owner-UID 0, Symlinkverbot und Bridge-ACL einschließlich `SO_PEERCRED` bleiben unverändert. Niemals `socket_owner_uid = 65534` einstellen: Diese Sammel-UID kann verschiedene nicht abgebildete Hostbenutzer bezeichnen. Eine vollständig erhaltene Mount-Sandbox würde eine privilegiert eingerichtete Namespace oder eine System-Unit mit `User=nathanael` benötigen und ist ein gesonderter Betriebsumbau.

Die isolierte Diagnose nutzt denselben `client_builder` wie der Dienst und führt weder eine Verbindung noch eine Secretabfrage aus:

```sh
cargo run -p uplink-infisical-transport --example pruefe_socket -- /run/uplink-infisical/api.sock 0
```

Ein erfolgreicher Aufruf im normalen Terminal belegt noch nicht die Sicht des Dienstes. Bei erneutem Fehler dieselbe Probe ausschließlich lesend in dessen bestehender User- und Mountnamespace ausführen (`nsenter --target <PID> --user --mount --setuid 1000 --setgid 1000`). Vor dem nächsten Umschalten die korrigierte Override-Datei installieren und den User-Manager durch den zuständigen Deployer neu einlesen lassen. Bridge-Unit, Bridge-Config und Socketverzeichnis benötigen keine Änderung.

## Nachweise

Normale Tests prüfen geschützte/fremde/symlinkartige Socketpfade sowie den Provider gegen einen echten synthetischen Unixsocket. Der zusätzliche Test `real_namespaces_switch_without_host_port_or_unauthorized_peer` läuft als root in einer eigenen äußeren Netznamespace. Er belegt zwei verschiedene echte Backend-Netznamespaces mit identischem Port, verweigert eine nicht erlaubte Peer-UID, schließt bei fehlender Gegenstelle und erreicht nach FD-Wechsel ausschließlich das zweite Backend. Der synthetische Angreiferlistener im äußeren Hostnetz erhält keine Verbindung. Produktions-Infisical wird dafür nicht neu gestartet. Die Docker-Metadatenvalidierung wird separat geprüft; der Test behauptet keinen tatsächlich ausgeführten Produktions-Docker-Neustart.
