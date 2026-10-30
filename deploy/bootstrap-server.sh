#!/bin/bash
# Install Benga's isolated maintenance host. This NEVER publishes the website.
set -euo pipefail

if [[ ${1:-} != --apply || $# != 1 ]]; then
    printf 'Usage: sudo bash deploy/bootstrap-server.sh --apply\n' >&2
    exit 2
fi
[[ $(id -u) == 0 ]] || { printf 'Run this on the server as root.\n' >&2; exit 1; }
for executable in nginx certbot systemctl sha256sum python3 openssl useradd install; do
    command -v "$executable" >/dev/null || { printf 'Missing prerequisite: %s\n' "$executable" >&2; exit 1; }
done
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
domain=benga.zezic.ru
available=/etc/nginx/sites-available/$domain
enabled=/etc/nginx/sites-enabled/$domain

# Refuse to overwrite an operator's public host or an unrelated symlink.
if [[ -e $available ]] && ! head -n 1 "$available" | grep -Fq '# Benga bootstrap: maintenance only.'; then
    printf 'Refusing to replace an existing non-maintenance Benga vhost.\n' >&2
    exit 1
fi
if [[ -L $enabled && $(readlink -f "$enabled") != "$available" ]] || [[ -e $enabled && ! -L $enabled ]]; then
    printf 'Unexpected Benga sites-enabled entry; inspect it manually.\n' >&2
    exit 1
fi
if getent passwd benga >/dev/null; then
    [[ $(id -u benga) -lt 1000 && $(getent passwd benga | cut -d: -f7) == /usr/sbin/nologin ]] || {
        printf 'Existing benga account is not a locked system service account.\n' >&2; exit 1;
    }
fi
nginx -t

audit_dir=$(mktemp -d /tmp/benga-bootstrap.XXXXXXXX)
trap 'rm -rf -- "$audit_dir"' EXIT
fingerprint_other_sites() {
    find /etc/nginx -type f ! -path "$available" -exec sha256sum {} + | sort
    find /etc/nginx/sites-enabled -maxdepth 1 -type l ! -name "$domain" -printf '%p -> %l\n' | sort
    find /etc/letsencrypt/renewal -type f ! -name "$domain.conf" -exec sha256sum {} + | sort
}
fingerprint_other_sites > "$audit_dir/before"

if ! getent passwd benga >/dev/null; then
    useradd --system --user-group --home-dir /srv/benga --shell /usr/sbin/nologin benga
fi
install -d -m 0755 -o root -g root /srv/benga /srv/benga/acme /opt/benga /etc/benga
install -d -m 0755 -o benga -g benga /srv/benga/site /srv/benga/downloads
install -m 0644 "$source_dir/mirror.py" /opt/benga/mirror.py
install -m 0644 "$source_dir/benga-mirror.service" /etc/systemd/system/benga-mirror.service
install -m 0644 "$source_dir/benga-mirror.timer" /etc/systemd/system/benga-mirror.timer
install -m 0755 "$source_dir/benga-nginx-reload" /etc/letsencrypt/renewal-hooks/deploy/benga-nginx-reload

activate_maintenance_config() {
    local candidate=$1
    local had_previous=false
    if [[ -e $available ]]; then
        cp -p "$available" "$audit_dir/previous-vhost"
        had_previous=true
    fi
    install -m 0644 "$candidate" "$available.pending"
    mv -f "$available.pending" "$available"
    [[ -L $enabled ]] || ln -s "$available" "$enabled"
    if ! nginx -t; then
        if $had_previous; then
            cp -p "$audit_dir/previous-vhost" "$available"
        else
            rm -f "$enabled" "$available"
        fi
        printf 'Benga configuration failed validation; restored its previous files.\n' >&2
        return 1
    fi
    systemctl reload nginx
}

if [[ ! -s /etc/letsencrypt/live/$domain/fullchain.pem ]]; then
    # Install only HTTP while Certbot proves control of this one hostname.
    cat > "$audit_dir/http.conf" <<'NGINX'
# Benga bootstrap: maintenance only. Website publication needs design approval.
server {
    listen 80;
    listen [::]:80;
    server_name benga.zezic.ru;
    location ^~ /.well-known/acme-challenge/ {
        root /srv/benga/acme;
        default_type text/plain;
        try_files $uri =404;
    }
    location / { return 503; }
}
NGINX
    activate_maintenance_config "$audit_dir/http.conf"
fi

# Reuse the existing production account; never create or modify account data.
account=${BENGA_CERTBOT_ACCOUNT:-}
if [[ -z $account ]]; then
    production_accounts=(/etc/letsencrypt/accounts/acme-v02.api.letsencrypt.org/directory/*)
    [[ ${#production_accounts[@]} == 1 && -d ${production_accounts[0]} ]] || {
        printf 'Set BENGA_CERTBOT_ACCOUNT to one existing production account ID.\n' >&2; exit 1;
    }
    account=$(basename -- "${production_accounts[0]}")
fi
[[ $account =~ ^[a-f0-9]{32}$ ]] || { printf 'Invalid Certbot account ID.\n' >&2; exit 1; }
certbot certonly --non-interactive --webroot --webroot-path /srv/benga/acme \
    --cert-name "$domain" --domains "$domain" --keep-until-expiring \
    --key-type ecdsa --account "$account" \
    --server https://acme-v02.api.letsencrypt.org/directory
activate_maintenance_config "$source_dir/nginx-maintenance.conf"
systemctl daemon-reload

fingerprint_other_sites > "$audit_dir/after"
if ! cmp -s "$audit_dir/before" "$audit_dir/after"; then
    diff -u "$audit_dir/before" "$audit_dir/after" >&2 || true
    printf 'Other configuration changed during setup; inspect before proceeding.\n' >&2
    exit 1
fi
install -d -m 0700 /var/lib/benga-bootstrap
install -m 0600 "$audit_dir/before" /var/lib/benga-bootstrap/other-sites-before.sha256
install -m 0600 "$audit_dir/after" /var/lib/benga-bootstrap/other-sites-after.sha256
printf 'Benga maintenance host is ready; other site configurations are unchanged.\n'
printf 'The website still returns 503. No website content was copied.\n'
printf 'Mirror timer state: '
systemctl is-enabled benga-mirror.timer || true
printf 'No timer was enabled. Install the public verification key and approve the site before activation.\n'
