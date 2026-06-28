#!/usr/bin/env bash
#
# Echoputer WPA crack-offload server — one-line server bootstrap.
#
#   curl -fsSL https://github.com/BotEkrem/echoputer/releases/latest/download/server-install.sh | sudo bash
#
# Downloads the static `crack-server` binary from the latest GitHub release (verifying
# its SHA256), installs hashcat + a wordlist, generates a shared secret (PSK), and runs
# it as a hardened systemd service (survives logout + reboot, auto-restarts). The
# Cardputer POSTs a captured `.22000` handshake WITH the PSK header; this box cracks it.
#
# Security: the server REQUIRES the PSK header on every crack request (set here), caps
# concurrent cracks, and the secret is printed once below. It is still PLAIN HTTP, so
# keep it on a trusted LAN / behind a firewall — the PSK stops randoms, not a sniffer.
#
# Override via env: REPO=owner/name PORT=8080 BIND=0.0.0.0 WORDLIST_URL=... bash server-install.sh
#
set -euo pipefail

REPO="${REPO:-BotEkrem/echoputer}"
PORT="${PORT:-8080}"
BIND="${BIND:-0.0.0.0}"
WORDLIST_URL="${WORDLIST_URL:-https://github.com/brannondorsey/naive-hashcat/releases/download/data/rockyou.txt}"
BASE="https://github.com/${REPO}/releases/latest/download"
ASSET="crack-server-x86_64-linux"
BIN=/usr/local/bin/crack-server
WORKDIR=/opt/echoputer-offload
UNIT=/etc/systemd/system/echoputer-offload.service

red()  { printf '\033[31m%s\033[0m\n' "$*"; }
say()  { printf '\033[36m==>\033[0m %s\n' "$*"; }

[ "$(id -u)" -eq 0 ] || { red "run as root:  curl -fsSL <url> | sudo bash"; exit 1; }
# the published binary is x86_64-linux-musl; refuse to install a binary that can't run
[ "$(uname -m)" = "x86_64" ] || { red "this release binary is x86_64 only; your arch is $(uname -m). Build from source (offload/crack-server) instead."; exit 1; }

# 1. dependencies
say "installing hashcat + curl ..."
if command -v apt-get >/dev/null 2>&1; then
  apt-get update -y && apt-get install -y hashcat curl ca-certificates coreutils
elif command -v dnf >/dev/null 2>&1; then
  dnf install -y hashcat curl coreutils
else
  red "no apt/dnf — install hashcat + curl manually, then re-run."; exit 1
fi

# 2. service user + workdir
id offload >/dev/null 2>&1 || useradd --system --home-dir "$WORKDIR" --shell /usr/sbin/nologin offload
mkdir -p "$WORKDIR"

# 3. the static binary, from the latest release — VERIFY SHA256 before installing as root
say "downloading $ASSET (+ .sha256) ..."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fSL "$BASE/$ASSET"        -o "$tmp/$ASSET"
curl -fSL "$BASE/$ASSET.sha256" -o "$tmp/$ASSET.sha256"
say "verifying checksum ..."
( cd "$tmp" && sha256sum -c "$ASSET.sha256" ) || { red "CHECKSUM MISMATCH — refusing to install. Aborting."; exit 1; }
install -m 0755 "$tmp/$ASSET" "$BIN"

# 4. a wordlist (skip if a system one exists or it's already here)
WL="$WORKDIR/rockyou.txt"
if [ -f /usr/share/wordlists/rockyou.txt ]; then
  WL=/usr/share/wordlists/rockyou.txt; say "using system wordlist: $WL"
elif [ ! -f "$WL" ]; then
  say "fetching a wordlist (rockyou) ..."
  curl -fSL "$WORDLIST_URL" -o "$WL" || red "WARN: wordlist download failed — put your own list at $WL"
fi
chown -R offload:offload "$WORKDIR"

# 5. shared secret (PSK); the device signs each request with HMAC-SHA256(PSK, body)
PSK="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"

# 6. hardened systemd service (PSK-gated; binds $BIND)
say "writing $UNIT ..."
cat > "$UNIT" <<EOF
[Unit]
Description=Echoputer WPA crack-offload server
After=network-online.target
Wants=network-online.target

[Service]
Environment=OFFLOAD_KEY=${PSK}
Environment=BIND=${BIND}
Environment=HOME=${WORKDIR}
ExecStart=${BIN} ${WL} ${PORT}
User=offload
Restart=always
RestartSec=2
StartLimitIntervalSec=60
StartLimitBurst=5
# hardening: a compromise can't escape — read-only FS, private /tmp, no new privs.
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=${WORKDIR}

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now echoputer-offload.service

# 7. status + the secret + the security reminder
sleep 1
say "service status:"
systemctl --no-pager --lines=4 status echoputer-offload.service || true
echo
say "listening on ${BIND}:${PORT}  (POST a .22000 line to /crack, PSK-gated)"
echo
red "=================== SHARED SECRET / PSK (save this) ==================="
red "  ${PSK}"
red " Put it in the device's offload config (Settings or Web UI) as the PSK."
red " The device signs each request with HMAC-SHA256(PSK, body); the PSK itself"
red " is never sent on the wire."
red "======================================================================"
echo
red "Still PLAIN HTTP — the HMAC stops forgery/replay-on-other-bodies + hides the"
red "PSK, but the .22000 + cracked pass travel in clear. Keep this on a TRUSTED LAN /"
red "behind a firewall; don't expose ${PORT} to the public internet."
red "e.g.:  ufw allow from <lab-subnet> to any port ${PORT}"
echo
say "test:  BODY=\$(tr -d '\\r\\n' < HS22000.TXT); \\"
say "       SIG=\$(printf '%s' \"\$BODY\" | openssl dgst -sha256 -hmac '${PSK}' | sed 's/^.*= //'); \\"
say "       curl -s -H \"X-Offload-Sig: \$SIG\" --data-binary \"\$BODY\" http://<this-ip>:${PORT}/crack; echo"
say "logs:    journalctl -u echoputer-offload -f"
say "remove:  systemctl disable --now echoputer-offload && rm -f ${BIN} ${UNIT}"
