#!/bin/sh
set -eu
# Ausschließlich vorhandene Laufzeitidentität; keine neue Credentialdatei/ENV.
exec 5< /run/user/1000/credentials/rs-relay.service/infisical-token
exec /opt/uplink/ingest-test/uplink-service --config /opt/uplink/ingest-test/config.toml --ingest-test
