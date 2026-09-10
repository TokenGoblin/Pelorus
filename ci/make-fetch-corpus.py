#!/usr/bin/env python3
"""Generate the synthetic response corpus the Phase 3 gate fetches against.

NOT the compat suite. tests/compat/archives/ holds recorded traffic from real
sites and is deliberately uncommitted — ADR 002 — because those archives name
every host they replay, which is the disclosure opaque ids exist to avoid.
This corpus is generated, every host in it is the loopback address, and it
discloses nothing. Conflating the two made the Phase 3 gate require files that
could never be committed, which is how this distinction got noticed.

Two hundred archives, written deterministically from a fixed seed so the
corpus is a *fixed target*: "200 URLs fetched correctly" has to mean the same
thing in a year, and a corpus that drifts turns a gate into a mood.

Why recorded rather than live. A gate that reaches the internet goes red when
somebody else has an outage, and a gate that is red for reasons nobody here
controls is one people learn to ignore. It also makes the awkward cases
reachable on demand: a response that is chunked with a trailer, a 304 with a
Content-Length that must be ignored, a body that ends only at close. Those are
the ones worth testing and the ones you cannot summon from the real web.

The shapes here are deliberately the ones crates/px-net/src/http1.rs makes
decisions about. A corpus of two hundred plain 200s would exercise one branch
two hundred times.

    python ci/make-fetch-corpus.py
"""

import hashlib
import json
import os
import pathlib
import sys

OUT = pathlib.Path("tests/net/corpus")
COUNT = 200

# Response shapes, cycled so every one of them appears many times and the
# corpus stays balanced rather than ending with a tail of exotic cases.
SHAPES = [
    "ok_length",
    "ok_chunked",
    "ok_chunked_trailer",
    "ok_until_close",
    "ok_empty_body",
    "redirect_301",
    "redirect_302",
    "not_modified_304",
    "no_content_204",
    "not_found_404",
    "server_error_500",
    "ok_utf8_body",
    "ok_large_body",
    "ok_many_headers",
]


def body_for(index: int, shape: str) -> bytes:
    if shape == "ok_utf8_body":
        return f"page {index} — kærlighed, 日本語, emoji 🙂".encode("utf-8")
    if shape == "ok_large_body":
        # Big enough to span several reads without being slow.
        return (f"line {index} " + "x" * 120 + "\n").encode() * 400
    if shape in ("ok_empty_body", "not_modified_304", "no_content_204"):
        return b""
    return f"<html><body>resource {index}</body></html>".encode()


def archive(index: int) -> dict:
    shape = SHAPES[index % len(SHAPES)]
    path = f"/r/{index:03d}"
    body = body_for(index, shape)

    status = 200
    headers = [{"name": "Content-Type", "value": "text/html; charset=utf-8"}]
    framing = "length"

    if shape == "ok_chunked":
        framing = "chunked"
    elif shape == "ok_chunked_trailer":
        framing = "chunked_trailer"
    elif shape == "ok_until_close":
        framing = "until_close"
    elif shape == "redirect_301":
        status = 301
        headers.append({"name": "Location", "value": f"/r/{(index + 1) % COUNT:03d}"})
    elif shape == "redirect_302":
        status = 302
        headers.append({"name": "Location", "value": f"/r/{(index + 7) % COUNT:03d}"})
    elif shape == "not_modified_304":
        status = 304
        framing = "none"
    elif shape == "no_content_204":
        status = 204
        framing = "none"
    elif shape == "not_found_404":
        status = 404
    elif shape == "server_error_500":
        status = 500
    elif shape == "ok_many_headers":
        for n in range(24):
            headers.append({"name": f"X-Custom-{n}", "value": f"value-{n}"})

    return {
        # A HAR-shaped subset: the fields this corpus needs, not the whole
        # specification. Named .har because that is what the gate counts and
        # what a reader will expect; the schema is documented by this file.
        "log": {
            "version": "1.2",
            "creator": {"name": "ci/make-fetch-corpus.py", "version": "1"},
            "entries": [
                {
                    "request": {"method": "GET", "url": f"http://localhost{path}"},
                    "response": {
                        "status": status,
                        "headers": headers,
                        "content": {
                            "size": len(body),
                            "text": body.decode("utf-8", errors="replace"),
                            "encoding": "utf-8",
                        },
                    },
                    "_replay": {
                        "path": path,
                        "shape": shape,
                        "framing": framing,
                        "body_sha256": hashlib.sha256(body).hexdigest(),
                    },
                }
            ],
        }
    }


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    for existing in OUT.glob("*.har"):
        existing.unlink()

    for index in range(COUNT):
        data = archive(index)
        # Sorted keys and a fixed separator: the files are committed, and a
        # regeneration that reorders keys would be a diff nobody can read.
        text = json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False)
        path = OUT / f"{index:03d}.har"
        with open(path, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text + "\n")

    print(f"wrote {COUNT} archives to {OUT}")
    return 0


if __name__ == "__main__":
    os.chdir(pathlib.Path(__file__).resolve().parent.parent)
    sys.exit(main())
