"""Static files for a fake Hugging Face Hub, served by nginx in vm-models.

Usage: fake_hub.py OUT REV

Repos (all at commit REV):
- test/tiny-asr-gguf: a GGUF whose general.architecture is `parakeet`
- test/llm-gguf: a GGUF for `llama`, which the engine cannot load
- test/corrupt-gguf: the tree lists a sha256 that does not match the file
"""

import hashlib
import json
import struct
import sys
from pathlib import Path

out = Path(sys.argv[1])
rev = sys.argv[2]


def gguf(arch: str, size: int) -> bytes:
    """GGUF v3 header with one string KV, padded to `size` with a
    deterministic pattern so downloads span many reads."""

    def string(s: str) -> bytes:
        b = s.encode()
        return struct.pack("<Q", len(b)) + b

    header = b"GGUF" + struct.pack("<IQQ", 3, 0, 1)
    header += string("general.architecture") + struct.pack("<I", 8) + string(arch)
    body = bytes(i * 31 % 251 for i in range(size - len(header)))
    return header + body


def write(path: str, data: bytes) -> None:
    p = out / path
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(data)


def repo(name: str, file: str, data: bytes, sha256: str | None = None) -> None:
    sha256 = sha256 or hashlib.sha256(data).hexdigest()
    write(f"{name}/resolve/{rev}/{file}", data)
    write(
        f"api/models/{name}/revision/main",
        json.dumps({"id": name, "sha": rev}).encode(),
    )
    tree = [
        {"type": "file", "oid": "0" * 40, "size": 12, "path": "README.md"},
        {
            "type": "file",
            "oid": "1" * 40,
            "size": len(data),
            "lfs": {"oid": sha256, "size": len(data), "pointerSize": 134},
            "path": file,
        },
    ]
    write(f"api/models/{name}/tree/{rev}", json.dumps(tree).encode())


asr = gguf("parakeet", 2 * 1024 * 1024)
repo("test/tiny-asr-gguf", "tiny-asr-Q8_0.gguf", asr)
repo("test/llm-gguf", "llm-Q8_0.gguf", gguf("llama", 256 * 1024))
repo("test/corrupt-gguf", "corrupt-Q8_0.gguf", asr, sha256="0" * 64)
write("meta/tiny-asr.sha256", hashlib.sha256(asr).hexdigest().encode())

write(
    "api/models.json",
    json.dumps(
        [
            {"id": "test/tiny-asr-gguf", "downloads": 42, "likes": 3},
            {"id": "test/llm-gguf", "downloads": 7, "likes": 0},
            {"id": "test/corrupt-gguf", "downloads": 1, "likes": 0},
        ]
    ).encode(),
)
