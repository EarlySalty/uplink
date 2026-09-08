#!/bin/sh
# Erzeugt ein prüfbares Paket; installiert/aktiviert keinen Dienst und keinen Port.
set -eu

release_checksums() (
  cd -- "$1"
  sha256sum bin/uplink-service bin/uplink-infisical-bridge bin/uplink-tls-provider \
    start-uplink.sh rs-relay-override.conf infisical-bridge.service \
    tls-provider.service tls-provider.timer config/uplink.toml \
    config/infisical-bridge.toml config/tls-provider.toml migrations/*.sql
)

check_package() {
  for directory in "$1" "$1/bin" "$1/config" "$1/migrations"; do
    if [ ! -d "$directory" ] || [ -L "$directory" ]; then
      echo 'Releasepaket enthält kein reguläres Paketverzeichnis.' >&2
      return 1
    fi
  done
  for artifact in bin/uplink-service bin/uplink-infisical-bridge bin/uplink-tls-provider \
    start-uplink.sh rs-relay-override.conf infisical-bridge.service \
    tls-provider.service tls-provider.timer config/uplink.toml \
    config/infisical-bridge.toml config/tls-provider.toml \
    migrations/20260908_destination_fences.sql SHA256SUMS; do
    if [ ! -f "$1/$artifact" ] || [ -L "$1/$artifact" ]; then
      echo "Releasepaket unvollständig: $artifact fehlt oder ist nicht regulär." >&2
      return 1
    fi
  done
  for executable in bin/uplink-service bin/uplink-infisical-bridge bin/uplink-tls-provider start-uplink.sh; do
    if [ ! -x "$1/$executable" ]; then
      echo "Releasepaket ungültig: $executable ist nicht ausführbar." >&2
      return 1
    fi
  done
  for migration in "$1"/migrations/*.sql; do
    if [ ! -f "$migration" ] || [ -L "$migration" ]; then
      echo 'Releasepaket enthält keine reguläre Migration.' >&2
      return 1
    fi
  done
  # Der feste Umfang verhindert, dass eine gekürzte Prüfsummenliste fehlende
  # Pflichtdateien oder ungeschützte zusätzliche Migrationen verschweigt.
  if ! actual_checksums=$(release_checksums "$1") || ! expected_checksums=$(cat -- "$1/SHA256SUMS"); then
    echo 'Releasepaket oder Prüfsummenliste konnte nicht vollständig gelesen werden.' >&2
    return 1
  fi
  if [ "$actual_checksums" != "$expected_checksums" ]; then
    echo 'Releasepaket hat abweichende Dateien oder eine unvollständige Prüfsummenliste.' >&2
    return 1
  fi
}

if [ "$#" -eq 2 ] && [ "$1" = '--check-package' ]; then
  check_package "$2"
  echo 'Releasepaket vollständig und Prüfsummen bestätigt.'
  exit 0
fi
if [ "$#" -ne 2 ]; then
  echo 'Aufruf: prepare-release.sh /absolutes/uplink-service /neues/release-verzeichnis' >&2
  echo 'Prüfung: prepare-release.sh --check-package /release-verzeichnis' >&2
  exit 2
fi
case "$1:$2" in /*:/*) ;; *) echo 'Beide Pfade müssen absolut sein.' >&2; exit 2 ;; esac
case "$1" in */uplink-service) ;; *) echo 'Der Binarypfad muss auf uplink-service zeigen.' >&2; exit 2 ;; esac
if [ ! -f "$1" ] || [ ! -x "$1" ] || [ -L "$1" ] || [ -e "$2" ] || [ -L "$2" ]; then
  echo 'Binary muss regulär und ausführbar sein; das neue Paket darf noch nicht existieren.' >&2
  exit 2
fi
package_source=$(dirname -- "$(realpath -- "$0")")
binary_source=$(dirname -- "$1")
for binary in uplink-service uplink-infisical-bridge uplink-tls-provider; do
  if [ ! -f "$binary_source/$binary" ] || [ ! -x "$binary_source/$binary" ] || [ -L "$binary_source/$binary" ]; then
    echo "Gekoppeltes Release benötigt das reguläre ausführbare Binary $binary im selben Buildverzeichnis." >&2
    exit 1
  fi
done
for artifact in start-uplink.sh rs-relay-override.conf infisical-bridge.service \
  tls-provider.service tls-provider.timer uplink.toml \
  infisical-bridge.toml.example tls-provider.toml.example; do
  if [ ! -f "$package_source/$artifact" ] || [ -L "$package_source/$artifact" ]; then
    echo "Gekoppeltes Release benötigt die reguläre Betriebsdatei $artifact." >&2
    exit 1
  fi
done
if [ ! -f "$package_source/../db/migrations/20260908_destination_fences.sql" ]; then
  echo 'Das gekoppelte Release benötigt die Zielgenerationsmigration; Paket unvollständig.' >&2
  exit 1
fi
for migration in "$package_source"/../db/migrations/*.sql; do
  if [ ! -f "$migration" ] || [ -L "$migration" ]; then
    echo 'Das gekoppelte Release benötigt ausschließlich reguläre Migrationsdateien.' >&2
    exit 1
  fi
done
"$1" --config "$package_source/uplink.toml" --check-config
# Identischer Build wie in den versionierten echten Mediennachweisen, eigener
# Installationsort ohne Abhängigkeit vom alten Repository.
printf '%s\n' \
  'ad7a8c8e8fe4f50972f32f63705cfcc57f44cd3531f57aa8defe388372242f5e  /opt/uplink/media/ffmpeg8-c733b4b2/ffmpeg' \
  '150bfd75016992a8d495a5f5c16cd93387a21c059f4309ed1e6342659aef48b3  /opt/uplink/media/ffmpeg8-c733b4b2/ffprobe' | sha256sum --check --status
umask 077
mkdir -m 0755 -- "$2"
mkdir -m 0755 -- "$2/bin" "$2/migrations"
mkdir -m 0700 -- "$2/config"
for binary in uplink-service uplink-infisical-bridge uplink-tls-provider; do
  install -m 0755 -- "$binary_source/$binary" "$2/bin/$binary"
done
install -m 0755 -- "$package_source/start-uplink.sh" "$2/start-uplink.sh"
for unit in rs-relay-override.conf infisical-bridge.service tls-provider.service tls-provider.timer; do
  install -m 0644 -- "$package_source/$unit" "$2/$unit"
done
install -m 0600 -- "$package_source/uplink.toml" "$2/config/uplink.toml"
install -m 0600 -- "$package_source/infisical-bridge.toml.example" "$2/config/infisical-bridge.toml"
install -m 0600 -- "$package_source/tls-provider.toml.example" "$2/config/tls-provider.toml"
for migration in "$package_source"/../db/migrations/*.sql; do
  install -m 0644 -- "$migration" "$2/migrations/"
done
release_checksums "$2" > "$2/SHA256SUMS"
check_package "$2"
echo 'Releasepaket vorbereitet. Keine Migration, Dienständerung oder Umschaltung ausgeführt.'
