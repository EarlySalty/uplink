#!/bin/sh
# Erzeugt ein prüfbares Paket; installiert/aktiviert keinen Dienst und keinen Port.
set -eu
if [ "$#" -ne 2 ]; then
  echo 'Aufruf: prepare-release.sh /absolutes/uplink-service /neues/release-verzeichnis' >&2
  exit 2
fi
case "$1:$2" in /*:/*) ;; *) echo 'Beide Pfade müssen absolut sein.' >&2; exit 2 ;; esac
if [ ! -f "$1" ] || [ ! -x "$1" ] || [ -L "$1" ] || [ -e "$2" ] || [ -L "$2" ]; then
  echo 'Binary muss regulär und ausführbar sein; das neue Paket darf noch nicht existieren.' >&2
  exit 2
fi
package_source=$(dirname -- "$(realpath -- "$0")")
if [ ! -f "$package_source/../db/migrations/20260908_destination_fences.sql" ]; then
  echo 'Das gekoppelte Release benötigt die Zielgenerationsmigration; Paket unvollständig.' >&2
  exit 1
fi
"$1" --config "$package_source/uplink.toml" --check-config
# Identischer Build wie in den versionierten echten Mediennachweisen, eigener
# Installationsort ohne Abhängigkeit vom alten Repository.
printf '%s\n' \
  'ad7a8c8e8fe4f50972f32f63705cfcc57f44cd3531f57aa8defe388372242f5e  /opt/uplink/media/ffmpeg8-c733b4b2/ffmpeg' \
  '150bfd75016992a8d495a5f5c16cd93387a21c059f4309ed1e6342659aef48b3  /opt/uplink/media/ffmpeg8-c733b4b2/ffprobe' | sha256sum --check --status
umask 077
mkdir -- "$2"
mkdir -- "$2/bin" "$2/config" "$2/migrations"
install -m 0755 -- "$1" "$2/bin/uplink-service"
install -m 0755 -- "$package_source/start-uplink.sh" "$2/start-uplink.sh"
install -m 0644 -- "$package_source/rs-relay-override.conf" "$2/rs-relay-override.conf"
install -m 0600 -- "$package_source/uplink.toml" "$2/config/uplink.toml"
for migration in "$package_source"/../db/migrations/*.sql; do
  [ -f "$migration" ] || continue
  install -m 0644 -- "$migration" "$2/migrations/"
done
(cd -- "$2" && sha256sum bin/uplink-service start-uplink.sh rs-relay-override.conf config/uplink.toml migrations/*.sql > SHA256SUMS)
echo 'Releasepaket vorbereitet. Keine Migration, Dienständerung oder Umschaltung ausgeführt.'
