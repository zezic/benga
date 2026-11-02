# Benga host and mirror

Domain: `benga.zezic.ru`. The website belongs in **`/home/benga/web`**,
owned by `benga` and readable by Nginx. The current website design was approved
on **2026-11-02**.

## Current state

Provisioned on 2026-10-31: locked `benga` account, dedicated Nginx vhost,
Let's Encrypt certificate and renewal hook. At provisioning, HTTP/HTTPS returned
**503**, the mirror was disabled and its public key was unset. Other sites were checked
unchanged; configuration inventories are in `/var/lib/benga-bootstrap/`.

The hosts are unavailable due to technical downtime as of 2026-11-02; their
current state has not been reverified. Deployment awaits host recovery and
native release/installer checks.

**Host migration is pending.** Local bootstrap/public-vhost templates now use
`/home/benga/web`; the deployed setup still uses `/srv/benga/site`. Apply the
updated paths on the host before activation. No server files have been moved
and Nginx has not been changed during the current downtime.

| Purpose | Path |
| --- | --- |
| Website files (intended) | `/home/benga/web` |
| Release mirror (existing service data) | `/srv/benga/downloads` |
| ACME challenge webroot (existing) | `/srv/benga/acme` |
| Public update key | `/etc/benga/update-public-key` |
| Mirror program | `/opt/benga/mirror.py` |
| Nginx vhost | `/etc/nginx/sites-available/benga.zezic.ru` |

Keep the ACME path aligned with Certbot's renewal configuration. Changing the
website root does not require relocating the challenge webroot or release cache.

## Mirror contract

`benga-mirror.service` runs as `benga`; its timer polls every 30 minutes.
It pulls public stable `zezic/benga` releases, verifies the Ed25519 signature
and every archive's size/SHA-256, then atomically switches `downloads/current`.
It rejects unsafe origins, incomplete releases and downgrades; older versions
remain available. No GitHub token or private signing key belongs on this host.

Install `deploy/update-public-key` (base64 raw 32-byte public key) at `/etc/benga/update-public-key`
(root-owned, mode `0644`). It must match the key compiled into the app; see
[releases.md](releases.md). Preserve signed JSON bytes and GitHub URLs.

The public vhost maps `/downloads/latest.json` and `.sig` to `current/` with
no caching, and `/downloads/vX.Y.Z/…` to immutable versioned archives. Only
portable ZIP/tar.gz archives are mirrored; DMG/EXE/DEB stay on GitHub Releases.

## Activate after host recovery and release checks

1. Apply the website-path migration on the host using the updated local
   bootstrap/public-vhost templates. Allow Nginx to traverse `/home/benga`.
2. Copy only approved website files to `/home/benga/web`; use the allowlist in
   [web/README.md](../web/README.md). Preserve the separate release cache.
3. Install the public key. After the first signed release, start
   `benga-mirror.service` and inspect its journal and downloaded files.
4. Install the updated `deploy/nginx-public.conf`, run `nginx -t`, then
   gracefully reload Nginx. Keep `nginx-maintenance.conf` for rollback.
5. Enable polling with `systemctl enable --now benga-mirror.timer`.

Change only Benga's configuration; preserve the other sites and certificates.
Certbot uses a dedicated webroot/lineage, avoiding edits to unrelated vhosts.

## Checks

```sh
nginx -t
systemctl status certbot.timer benga-mirror.timer --no-pager
certbot renew --cert-name benga.zezic.ru --dry-run
journalctl -u benga-mirror.service --since today
python3 -m unittest discover -s deploy -p 'test_*.py'
```
