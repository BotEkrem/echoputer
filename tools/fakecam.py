#!/usr/bin/env python3
"""
fakecam.py — a fake IP camera to validate Echoputer's Camera Finder without real hardware.

Emulates exactly what camscan.rs keys on, so the on-device sweep/fingerprint/cred-ladder/
snapshot/PTZ paths all light up:

  - Server header per brand        -> classify() brand match (red camera blip)
  - 401 + WWW-Authenticate         -> host flagged auth_basic / auth_digest
  - accepts a default credential   -> cred ladder finds it (green blip, user:pass shown)
  - serves a real JPEG (FF D8 FF)  -> snapshot saved to SD as SNAP.JPG
  - accepts ptz.cgi/decoder_control-> PTZ nudges logged

Run on a machine on the SAME WiFi/subnet as the Cardputer's joined AP, then point the
Cardputer's Camera Finder at that AP.

  python3 fakecam.py                      # Dahua + Digest auth + ptz.cgi  (richest: Digest + snapshot + PTZ)
  python3 fakecam.py --brand goahead      # GoAhead + Basic auth + decoder_control.cgi PTZ
  python3 fakecam.py --brand hikvision    # Hikvision + Digest + ISAPI snapshot (no PTZ on this firmware)
  python3 fakecam.py --port 80            # port 80 needs sudo on macOS; 8080 (default) does not
  python3 fakecam.py --user admin --pass admin

Default credential is admin:admin (first entry in camscan.rs CREDS), so the ladder hits it
on attempt #1. The Cardputer probes ports 80 AND 8080 — 8080 is the no-sudo default here.
"""
import argparse, base64, hashlib, re, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# JPEG served on any snapshot endpoint: a real 160x120 "FAKE CAM" image, embedded as
# base64 so this stays a single self-contained file. Starts with the SOI marker FF D8 FF.
JPEG = base64.b64decode(
    "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"
)
assert JPEG[:3] == b"\xff\xd8\xff", "JPEG must start with the SOI marker FF D8 FF"


def md5(s: str) -> str:
    return hashlib.md5(s.encode()).hexdigest()


def parse_kv(s: str) -> dict:
    return {k: v.strip('"') for k, v in re.findall(r'(\w+)=("[^"]*"|[^,]+)', s)}


class Cam(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    server_version = ""        # suppress the default "BaseHTTP/.." token
    sys_version = ""

    # set per-run in main()
    BRAND = "dahua"
    SERVER = "Dahua Rtsp Server/1.0"
    SCHEME = "digest"          # "digest" | "basic"
    REALM = "Login to IPCamera"
    USER = "admin"
    PASS = "admin"
    NONCE = "deadbeefcafef00d1234567890abcdef"
    OPAQUE = "5ccc069c403ebaf9f0171e9517f40e41"

    def log_message(self, *a):
        pass  # we print our own concise lines

    # --- auth ---
    def _ok_basic(self, h):
        if not h or not h.startswith("Basic "):
            return False
        try:
            u, p = base64.b64decode(h[6:]).decode("utf-8", "replace").split(":", 1)
        except Exception:
            return False
        return u == self.USER and p == self.PASS

    def _ok_digest(self, h):
        if not h or not h.startswith("Digest "):
            return False
        d = parse_kv(h[7:])
        if d.get("username") != self.USER or d.get("realm") != self.REALM or d.get("nonce") != self.NONCE:
            return False
        ha1 = md5(f"{self.USER}:{self.REALM}:{self.PASS}")
        ha2 = md5(f"GET:{d.get('uri', '/')}")          # http_head sends GET
        if d.get("qop"):
            expect = md5(f"{ha1}:{d['nonce']}:{d.get('nc', '')}:{d.get('cnonce', '')}:{d['qop']}:{ha2}")
        else:
            expect = md5(f"{ha1}:{d['nonce']}:{ha2}")
        return d.get("response") == expect

    def _authed(self):
        h = self.headers.get("Authorization")
        return self._ok_basic(h) if self.SCHEME == "basic" else self._ok_digest(h)

    def _challenge(self):
        self.send_response(401)
        self.send_header("Server", self.SERVER)
        if self.SCHEME == "basic":
            self.send_header("WWW-Authenticate", f'Basic realm="{self.REALM}"')
        else:
            self.send_header("WWW-Authenticate",
                             f'Digest realm="{self.REALM}", qop="auth", '
                             f'nonce="{self.NONCE}", opaque="{self.OPAQUE}"')
        self.send_header("Content-Length", "0")
        self.end_headers()

    # --- routing ---
    def _is_ptz(self, path):
        return ("ptz.cgi" in path) or ("decoder_control.cgi" in path)

    def _ptz_dir(self, path):
        m = re.search(r"code=(\w+)", path)             # Dahua: code=Up
        if m:
            return m.group(1)
        m = re.search(r"command=(\d+)", path)          # Foscam-clone: command=0/2/4/6
        return {"0": "Up", "2": "Down", "4": "Left", "6": "Right"}.get(m.group(1), "?") if m else "?"

    def _send(self, code, ctype, body, head_only):
        self.send_response(code)
        self.send_header("Server", self.SERVER)
        if ctype:
            self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if not head_only and body:
            self.wfile.write(body)

    def _serve(self, head_only):
        if not self._authed():
            print(f"  [401] {self.command} {self.path}  -> challenge ({self.SCHEME})")
            self._challenge()
            return
        if self._is_ptz(self.path):
            print(f"  [PTZ] {self._ptz_dir(self.path)}   ({self.path})")
            self._send(200, "text/plain", b"OK", head_only)
            return
        if self.path in ("/", ""):
            print(f"  [200] auth OK ({self.USER}:{self.PASS})  {self.path}")
            self._send(200, "text/html", b"<html><body>fake cam ok</body></html>", head_only)
            return
        # anything else is treated as a snapshot endpoint (covers every brand's candidate paths)
        print(f"  [JPEG] snapshot {self.path}  ({len(JPEG)} B)")
        self._send(200, "image/jpeg", JPEG, head_only)

    def do_GET(self):
        self._serve(head_only=False)

    def do_HEAD(self):
        self._serve(head_only=True)


BRANDS = {
    "dahua":     ("Dahua Rtsp Server/1.0", "digest"),
    "goahead":   ("GoAhead-Webs",          "basic"),
    "hikvision": ("App-webs/",             "digest"),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--brand", choices=BRANDS, default="dahua")
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--user", default="admin")
    ap.add_argument("--pass", dest="pw", default="admin")
    a = ap.parse_args()
    try:
        sys.stdout.reconfigure(line_buffering=True)  # logs appear promptly even when piped
    except Exception:
        pass
    Cam.BRAND = a.brand
    Cam.SERVER, Cam.SCHEME = BRANDS[a.brand]
    Cam.USER, Cam.PASS = a.user, a.pw
    srv = ThreadingHTTPServer(("0.0.0.0", a.port), Cam)
    print(f"fake camera up  brand={a.brand}  server='{Cam.SERVER}'  auth={Cam.SCHEME}  "
          f"cred={a.user}:{a.pw}  port={a.port}  jpeg={len(JPEG)}B")
    print("point the Cardputer's Camera Finder at this subnet. Ctrl+C to stop.\n")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\nbye")


if __name__ == "__main__":
    main()
