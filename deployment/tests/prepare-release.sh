#!/bin/sh
# Isolierte Paketprüfung mit künstlichen Binaries; keine Dienste oder Secrets.
set -eu
test_source=$(dirname -- "$(dirname -- "$(realpath -- "$0")")")
test_root=$(mktemp -d)
trap 'rm -rf -- "$test_root"' EXIT HUP INT TERM
mkdir -- "$test_root/deployment" "$test_root/db" "$test_root/db/migrations" "$test_root/build"
for artifact in prepare-release.sh start-uplink.sh rs-relay-override.conf \
  infisical-bridge.service tls-provider.service tls-provider.timer uplink.toml \
  infisical-bridge.toml.example tls-provider.toml.example; do
  cp -- "$test_source/$artifact" "$test_root/deployment/$artifact"
done
for migration in "$test_source"/../db/migrations/*.sql; do
  cp -- "$migration" "$test_root/db/migrations/"
done
cat > "$test_root/build/uplink-service" <<'FIXTURE'
#!/bin/sh
set -eu
[ "$#" -eq 3 ] && [ "$1" = '--config' ] && [ -f "$2" ] && [ "$3" = '--check-config' ]
FIXTURE
chmod 0755 "$test_root/build/uplink-service"

expect_failure() {
  if "$@" > "$test_root/expected-error.log" 2>&1; then
    echo 'Fehler: Ein ungültiges Releasepaket wurde akzeptiert.' >&2
    exit 1
  fi
}

prepare="$test_root/deployment/prepare-release.sh"
package="$test_root/package"
# Beide neuen Binaries sind Pflicht, bevor überhaupt ein Paket angelegt wird.
expect_failure "$prepare" "$test_root/build/uplink-service" "$package"
[ ! -e "$package" ]
cp -- "$test_root/build/uplink-service" "$test_root/build/uplink-infisical-bridge"
expect_failure "$prepare" "$test_root/build/uplink-service" "$package"
[ ! -e "$package" ]
cp -- "$test_root/build/uplink-service" "$test_root/build/uplink-tls-provider"

mv -- "$test_root/deployment/infisical-bridge.service" "$test_root/missing-unit"
expect_failure "$prepare" "$test_root/build/uplink-service" "$package"
[ ! -e "$package" ]
mv -- "$test_root/missing-unit" "$test_root/deployment/infisical-bridge.service"
"$prepare" "$test_root/build/uplink-service" "$package"
"$prepare" --check-package "$package"

for artifact in bin/uplink-service bin/uplink-infisical-bridge bin/uplink-tls-provider \
  start-uplink.sh rs-relay-override.conf infisical-bridge.service \
  tls-provider.service tls-provider.timer config/uplink.toml \
  config/infisical-bridge.toml config/tls-provider.toml \
  migrations/20260908_destination_fences.sql SHA256SUMS; do
  mv -- "$package/$artifact" "$test_root/missing-artifact"
  expect_failure "$prepare" --check-package "$package"
  mv -- "$test_root/missing-artifact" "$package/$artifact"
done

chmod 0644 "$package/bin/uplink-infisical-bridge"
expect_failure "$prepare" --check-package "$package"
chmod 0755 "$package/bin/uplink-infisical-bridge"
mv -- "$package/bin/uplink-infisical-bridge" "$test_root/bridge-fixture"
ln -s "$test_root/bridge-fixture" "$package/bin/uplink-infisical-bridge"
expect_failure "$prepare" --check-package "$package"
rm -- "$package/bin/uplink-infisical-bridge"
mv -- "$test_root/bridge-fixture" "$package/bin/uplink-infisical-bridge"

cp -- "$package/SHA256SUMS" "$test_root/full-manifest"
sed '/bin\/uplink-infisical-bridge$/d' "$test_root/full-manifest" > "$package/SHA256SUMS"
expect_failure "$prepare" --check-package "$package"
cp -- "$test_root/full-manifest" "$package/SHA256SUMS"
printf '\n# veränderte normale Konfiguration\n' >> "$package/config/infisical-bridge.toml"
expect_failure "$prepare" --check-package "$package"
cp -- "$test_root/deployment/infisical-bridge.toml.example" "$package/config/infisical-bridge.toml"
printf 'SELECT 1;\n' > "$package/migrations/unlisted.sql"
expect_failure "$prepare" --check-package "$package"
rm -- "$package/migrations/unlisted.sql"
"$prepare" --check-package "$package"
echo 'Paketregressionen bestanden: Pflichtartefakte, Ausführbarkeit, Symlinks, Manifestumfang und Dateiintegrität.'
