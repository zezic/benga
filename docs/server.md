# Benga host and download mirror

The intended host is `benga.zezic.ru`. Website publication is gated on design
approval. Infrastructure setup serves HTTP **503** on both HTTP and HTTPS,
except for the HTTP ACME challenge path. It never copies or serves `web/`.

Provisioned on 2026-10-31 (Indian/Antananarivo). HTTPS verifies successfully,
and both schemes return 503. The certificate expires at 2027-01-29 00:09 local
time (2027-01-28 21:09 UTC). The other four hosts responded with HTTP 200 after
setup, and their configuration inventories matched exactly. The mirror timer
is installed, disabled and inactive; its public key has not been installed.

## Isolated infrastructure

`deploy/bootstrap-server.sh --apply`, run as root **on the server**, creates only:

- A locked system account `benga`, with `/usr/sbin/nologin` and `/srv/benga` home.
- `/srv/benga/site` and `/srv/benga/downloads`, owned by that account.
- `/srv/benga/acme`, a dedicated Certbot webroot.
- `/etc/nginx/sites-available/benga.zezic.ru` and its `sites-enabled` link.
- A separate `benga.zezic.ru` certificate lineage and a conditional reload hook.
- Root-owned `/opt/benga/mirror.py` and `benga-mirror.service` / `.timer` units.

It requires the existing Nginx, Certbot, Python 3, OpenSSL, and systemd setup.
It does not install packages, change the firewall, upgrade the host, restart
services, enable the mirror, or edit existing site configurations. Nginx changes
pass `nginx -t` before a graceful reload. A failed validation restores Benga's
previous vhost. Re-running refuses to replace a vhost that has already moved
beyond the marked maintenance configuration.

SHA256 inventories of the other Nginx files, enabled-site symlinks, and existing
certificate renewal configurations are compared before and after setup. The
server retains both inventories in `/var/lib/benga-bootstrap/` with root-only
permissions. Nginx logs for this host are `/var/log/nginx/benga.*.log`.

Certificates use `certbot certonly --webroot`, with a dedicated `--cert-name`,
so Certbot does not rewrite the other vhosts. The existing production account
is reused. The normal Certbot timer handles renewals, and the new deploy hook
reloads Nginx only when `RENEWED_LINEAGE` is Benga's lineage. See the
[Certbot webroot and renewal documentation](https://eff-certbot.readthedocs.io/en/stable/using.html).

## Authenticated mirror

The mirror runs as `benga`. It needs outbound HTTPS and a **public** verification
key in `/etc/benga/update-public-key`, encoded as base64 Ed25519 raw 32-byte key.
Use exactly the public key derived by the release workflow and compiled into
the app. The private signing seed belongs only in the repository's
`BENGA_UPDATE_SIGNING_KEY` Actions secret. Do not copy it to this host.

The Python script uses only the standard library plus the existing OpenSSL
executable. It fetches the latest public stable release from `zezic/benga`,
authenticates the exact `latest.json` bytes with `latest.json.sig`, and verifies
every signed archive's size and SHA256 before activation. It rejects foreign
URLs, insecure redirects, incomplete platform sets, oversized files, modified
version directories, and version rollbacks. Downloads follow HTTPS redirects
only within GitHub's release infrastructure. No GitHub access token or SSH
credential is stored in the service or required from Actions.

The signed manifest covers portable ZIP/tar.gz archives for the five build
targets. DMG, Setup EXE and DEB installers remain on GitHub Releases unless
they are added to authenticated release metadata later.

Each version is staged privately inside the download filesystem, then renamed
to `/srv/benga/downloads/vMAJOR.MINOR.PATCH`. Only after every file verifies does
an atomic `current` symlink switch activate that complete version. Existing
versions are retained for stable links and can be pruned manually after checking
which version `current` references. The service does not delete older releases.

After approved publication, the public Nginx template exposes:

| Public URL | Server file |
| --- | --- |
| `/downloads/latest.json` | `downloads/current/latest.json` |
| `/downloads/latest.json.sig` | `downloads/current/latest.json.sig` |
| `/downloads/vX.Y.Z/Benga-…` | `downloads/vX.Y.Z/Benga-…` |

The manifest retains its original GitHub URLs so its signature stays valid.
The website can map an archive filename to the versioned local mirror URL.
Requests for the latest manifest and signature are uncached. Clients fetching
the pair across a release switch should retry verification once; immutable
versioned URLs always refer to a matching pair.

## Activation after design approval

The following steps are intentionally **not** part of bootstrap:

1. Copy the reviewed static website files into `/srv/benga/site`, owned by
   `benga`. Inspect the staged files before switching the maintenance vhost.
2. Install the public verification key as `/etc/benga/update-public-key` with
   root ownership and mode `0644`.
3. Run `systemctl start benga-mirror.service` after a signed public release
   exists. Check `journalctl -u benga-mirror.service` and the version directory.
4. Only with design approval, install `deploy/nginx-public.conf` as the Benga
   vhost, run `nginx -t`, and gracefully reload Nginx. The maintenance file is
   the rollback configuration.
5. Enable polling with `systemctl enable --now benga-mirror.timer`.

The timer checks every half hour with a small randomized delay. It stays
disabled until this explicit activation. The service cannot write outside the
download directory and has no root privileges. The mirror's Ed25519 signature
is separate from Apple notarization or Windows code signing.

## Verification and maintenance

```sh
nginx -t
systemctl status certbot.timer --no-pager
certbot certificates --cert-name benga.zezic.ru
certbot renew --cert-name benga.zezic.ru --dry-run
systemctl is-enabled benga-mirror.timer
journalctl -u benga-mirror.service --since today
```

Local mirror boundary tests use throwaway signing keys and no network:

```sh
python3 -m unittest discover -s deploy -p 'test_*.py' -v
bash -n deploy/bootstrap-server.sh
```
