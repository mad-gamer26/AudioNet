#!/bin/sh
# Certbot deploy hook: gives coturn its own copy of the TLS certificate
# after every renewal, without changing permissions on /etc/letsencrypt.
# Install as /etc/letsencrypt/renewal-hooks/deploy/audionet-coturn.sh (chmod 755).
# CERT_NAME is the certbot certificate name (directory under live/).
set -eu
CERT_NAME="${AUDIONET_COTURN_CERT_NAME:-audionet.example.com}"
SRC="/etc/letsencrypt/live/$CERT_NAME"
# Certbot sets RENEWED_LINEAGE; ignore renewals of other certificates.
if [ -n "${RENEWED_LINEAGE:-}" ] && [ "$RENEWED_LINEAGE" != "$SRC" ]; then
    exit 0
fi
DST=/etc/coturn/certs
install -d -m 0750 -o root -g turnserver "$DST"
install -m 0640 -o root -g turnserver "$SRC/fullchain.pem" "$DST/fullchain.pem"
install -m 0640 -o root -g turnserver "$SRC/privkey.pem" "$DST/privkey.pem"
systemctl restart coturn || true
