# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
# PDMS OCR sidecar: OCRmyPDF (Tesseract) behind a minimal HTTP interface.
#
# Why not Rust: there is no mature OCR engine in Rust. OCRmyPDF is a Python
# program wrapping Tesseract, so this sidecar is a small Python script in the
# official OCRmyPDF image. PDMS talks to it only over HTTP (`POST /ocr`) and
# runs normally without it.
FROM jbarlow83/ocrmypdf:v17.12.1
# The wrapper script is AGPL-3.0-or-later; OCRmyPDF (MPL-2.0), Tesseract (Apache-2.0),
# and Ghostscript (AGPL-3.0) keep their own licenses.
LABEL org.opencontainers.image.title="PDMS OCR sidecar" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY server.py /srv/ocr/server.py
USER 1000:1000
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz')"
ENTRYPOINT ["python3", "/srv/ocr/server.py"]
