#!/bin/sh
set -eu
# Bestehende Credentialversorgung derselben Unit, keine neue Secretdatei.
# Die Shell öffnet ausschließlich den vorhandenen Zugang und vererbt FD 5.
exec 5< /run/user/1000/credentials/rs-relay.service/infisical-token
exec /opt/uplink/current/bin/uplink-service --config /home/nathanael/.config/uplink/uplink.toml
