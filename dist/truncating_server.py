#!/usr/bin/env python3
"""Truncating archive server for RELEASE-VALIDATION.md §6b.

Serves a single file but lies about its length on the first N requests:
it sends the real `Content-Length` header, then writes only a fraction
of the body and slams the connection shut. That reproduces exactly what
a TLS-inspecting proxy / antivirus middlebox does to a large HTTPS
download — a *clean* early EOF — which the installer must now catch as
"Download incomplete" instead of a baffling "Could not find EOCD".

After the first N truncated responses it serves the file in full, so a
single run exercises BOTH the honest-failure path and the
self-healing retry (the installer retries `DOWNLOAD_ATTEMPTS` times).

Usage:
    python dist/truncating_server.py dist/CodeScope-v99.0.0-windows.zip \
        --port 8000 --truncate-first 2 --fraction 0.5

Point the dev build at it:
    $env:CODESCOPE_DEV_UPDATE_URL =
        "http://127.0.0.1:8000/CodeScope-v99.0.0-windows.zip"

- truncate-first 2 (default), DOWNLOAD_ATTEMPTS=3  -> attempts 1-2 are
  cut short, attempt 3 succeeds: toast shows the retry self-healing
  into "Installing" -> "Update installed".
- truncate-first 999  -> every attempt is cut: the flow ends on the
  honest "Download incomplete: received N of M bytes" failure (NOT an
  EOCD extract error).
"""
import argparse
import http.server
import os
import sys


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file", help="path to the archive to serve")
    ap.add_argument("--port", type=int, default=8000)
    ap.add_argument(
        "--truncate-first",
        type=int,
        default=2,
        help="cut the body short on the first N requests, then serve full",
    )
    ap.add_argument(
        "--fraction",
        type=float,
        default=0.5,
        help="fraction of the body to send before cutting (0..1)",
    )
    args = ap.parse_args()

    body = open(args.file, "rb").read()
    total = len(body)
    name = os.path.basename(args.file)
    served = {"n": 0}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):  # noqa: N802 (stdlib naming)
            served["n"] += 1
            request_no = served["n"]
            truncate = request_no <= args.truncate_first
            # Always advertise the *full* length — the lie that makes a
            # short body look like a truncated transfer to the client.
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(total))
            self.end_headers()
            if truncate:
                cut = max(1, int(total * args.fraction))
                self.wfile.write(body[:cut])
                self.wfile.flush()
                # Force a hard close mid-body: the client sees a clean
                # EOF after `cut` bytes despite Content-Length == total.
                self.connection.close()
                sys.stderr.write(
                    f"[req {request_no}] TRUNCATED {cut}/{total} bytes\n"
                )
            else:
                self.wfile.write(body)
                sys.stderr.write(f"[req {request_no}] full {total} bytes\n")
            sys.stderr.flush()

        def log_message(self, *_):  # silence the default access log
            pass

    httpd = http.server.HTTPServer(("127.0.0.1", args.port), Handler)
    sys.stderr.write(
        f"serving {name} ({total} bytes) on http://127.0.0.1:{args.port}/ "
        f"— truncating first {args.truncate_first} request(s) at "
        f"{args.fraction:.0%}\n"
    )
    sys.stderr.flush()
    httpd.serve_forever()


if __name__ == "__main__":
    main()
