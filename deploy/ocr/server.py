# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
"""PDMS OCR sidecar.

POST /ocr   body: the file (PDF, PNG, JPEG, or TIFF); Content-Type: its media type
            200 text/plain: the recognized text
            413 too large, 415 unsupported type, 422 unreadable file, 503 busy
GET /healthz

Settings (environment): OCR_LANGUAGES (default "eng"), OCR_MAX_BYTES
(default 52428800), OCR_TIMEOUT_SECONDS (default 280), OCR_CONCURRENCY
(default 2), OCR_BIND (default 0.0.0.0:8080).

Only the standard library is used besides OCRmyPDF itself. Files are written
to a private temporary directory and removed after each request.
"""

import os
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LANGUAGES = os.environ.get("OCR_LANGUAGES", "eng")
MAX_BYTES = int(os.environ.get("OCR_MAX_BYTES", str(50 * 1024 * 1024)))
TIMEOUT = int(os.environ.get("OCR_TIMEOUT_SECONDS", "280"))
SLOTS = threading.BoundedSemaphore(int(os.environ.get("OCR_CONCURRENCY", "2")))
TYPES = {
    "application/pdf": ".pdf",
    "image/png": ".png",
    "image/jpeg": ".jpg",
    "image/tiff": ".tif",
}


class Handler(BaseHTTPRequestHandler):
    server_version = "pdms-ocr/1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

    def reply(self, status, text):
        body = text.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/healthz":
            self.reply(200, "ok")
        else:
            self.reply(404, "not found")

    def do_POST(self):
        if self.path != "/ocr":
            return self.reply(404, "not found")
        media = self.headers.get("Content-Type", "").split(";")[0].strip().lower()
        if media not in TYPES:
            return self.reply(415, "unsupported media type: %s" % media)
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            return self.reply(400, "bad Content-Length")
        if length <= 0 or length > MAX_BYTES:
            return self.reply(413, "file too large or empty")
        data = self.rfile.read(length)
        if not SLOTS.acquire(timeout=5):
            return self.reply(503, "busy")
        try:
            with tempfile.TemporaryDirectory() as tmp:
                src = os.path.join(tmp, "input" + TYPES[media])
                out = os.path.join(tmp, "output.pdf")
                txt = os.path.join(tmp, "output.txt")
                with open(src, "wb") as f:
                    f.write(data)
                cmd = ["ocrmypdf", "--force-ocr", "--sidecar", txt, "--output-type", "pdf",
                       "-l", LANGUAGES, "--jobs", "1", "--quiet"]
                if media != "application/pdf":
                    cmd += ["--image-dpi", "300"]
                cmd += [src, out]
                try:
                    r = subprocess.run(cmd, capture_output=True, timeout=TIMEOUT, check=False)
                except subprocess.TimeoutExpired:
                    return self.reply(503, "timed out")
                if r.returncode != 0:
                    detail = r.stderr.decode("utf-8", "replace")[-500:]
                    status = 422 if r.returncode in (2, 4, 7, 8) else 500
                    return self.reply(status, "ocrmypdf exit %d: %s" % (r.returncode, detail))
                with open(txt, "r", encoding="utf-8", errors="replace") as f:
                    return self.reply(200, f.read())
        finally:
            SLOTS.release()


def main():
    host, _, port = os.environ.get("OCR_BIND", "0.0.0.0:8080").rpartition(":")
    ThreadingHTTPServer((host or "0.0.0.0", int(port)), Handler).serve_forever()


if __name__ == "__main__":
    main()
