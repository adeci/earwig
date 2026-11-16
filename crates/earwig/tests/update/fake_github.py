"""A stand-in for GitHub's API and release downloads, for earwig's update
tests: one release, v<VERSION>, with one file. Test builds of earwig ask it
instead of GitHub (EARWIG_UPDATE_TEST_FEED=http://127.0.0.1:<PORT> when they
are compiled).

Usage: fake_github.py --port PORT --version VERSION --file FILE [--name NAME]
                      [--wrong-digest] [--seconds SECONDS]
  --name           the file's name in the release (FILE's own by default)
  --wrong-digest   list a SHA-256 that does not match the file
  --seconds        spread the download over this long
"""

import argparse
import hashlib
import http.server
import json
import sys
import time
from pathlib import Path

REPO = "adeci/earwig"

parser = argparse.ArgumentParser()
parser.add_argument("--port", type=int, required=True)
parser.add_argument("--version", required=True)
parser.add_argument("--file", type=Path, required=True)
parser.add_argument("--name")
parser.add_argument("--wrong-digest", action="store_true")
parser.add_argument("--seconds", type=float, default=0)
args = parser.parse_args()

data = args.file.read_bytes()
name = args.name or args.file.name
digest = hashlib.sha256(b"not " + data if args.wrong_digest else data).hexdigest()
tag = f"v{args.version}"
download = f"/{REPO}/releases/download/{tag}/{name}"
release = {
    "tag_name": tag,
    "name": f"earwig {tag}",
    "draft": False,
    "prerelease": "-" in args.version,
    "html_url": f"https://github.com/{REPO}/releases/tag/{tag}",
    "assets": [
        {
            "name": name,
            "size": len(data),
            "digest": f"sha256:{digest}",
            "state": "uploaded",
            "content_type": "application/octet-stream",
            "browser_download_url": f"http://127.0.0.1:{args.port}{download}",
        }
    ],
}


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split("?")[0]
        if path == f"/repos/{REPO}/releases/latest":
            self.reply(json.dumps(release).encode(), "application/json")
        elif path == f"/repos/{REPO}/releases":
            self.reply(json.dumps([release]).encode(), "application/json")
        elif path == download:
            self.reply(data, "application/octet-stream")
        else:
            self.send_error(404)

    def reply(self, body, kind):
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        chunks = [body[i : i + 65536] for i in range(0, len(body), 65536)] or [b""]
        for chunk in chunks:
            self.wfile.write(chunk)
            if args.seconds and kind != "application/json":
                time.sleep(args.seconds / len(chunks))

    def log_message(self, format, *rest):
        print(format % rest, file=sys.stderr, flush=True)


http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()
