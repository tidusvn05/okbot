#!/usr/bin/env python3
"""Rebuild the embedded chart fonts (ADR 0016).

Downloads Noto Sans and Noto Sans JP from google/fonts at a pinned commit, checks their hashes,
cuts static weights, keeps only the characters charts need and stores them zlib-compressed:

- latin-400.ttf.z, latin-700.ttf.z: Latin, Latin Extended-A, Vietnamese, punctuation, currency signs.
- ja-400.ttf.z: kana, CJK punctuation, full-width forms and the 2,965 JIS level 1 kanji.

Needs fonttools 4.67.0 (`pip install fonttools==4.67.0`). Run from anywhere:
`python3 crates/okbot-chart/fonts/build.py`.
"""
import hashlib
import io
import pathlib
import urllib.request
import zlib

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

HERE = pathlib.Path(__file__).resolve().parent
COMMIT = "e7db6ca603fd4862010383ff17ffd6cc4ffac1c0"
SOURCES = {
    "latin": (
        "ofl/notosans/NotoSans%5Bwdth%2Cwght%5D.ttf",
        "bfb7bb691513f12e734dc346c03a03f784912432d7e3fa8e56efcf906fe86b3d",
    ),
    "ja": (
        "ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf",
        "c2f3b4d463500a2ddcd3849cded1fceeb9fd6d1c32e6cbecd568453ba50fc68f",
    ),
}

LATIN = [
    (0x20, 0x7E), (0xA0, 0x17F), (0x1A0, 0x1B0), (0x300, 0x36F), (0x1E00, 0x1EFF),
    (0x2000, 0x206F), (0x20A0, 0x20CF), (0x2190, 0x2193), (0x2212, 0x2212), (0x25B2, 0x25BC),
]
JA = [(0x3000, 0x30FF), (0x31F0, 0x31FF), (0xFF00, 0xFFEF)]


def jis_level1() -> list[int]:
    """The JIS X 0208 level 1 kanji (rows 16-47), through the EUC-JP codec."""
    out = []
    for row in range(16, 48):
        for cell in range(1, 95):
            try:
                ch = bytes([0xA0 + row, 0xA0 + cell]).decode("euc_jp")
            except UnicodeDecodeError:
                continue
            if 0x4E00 <= ord(ch) <= 0x9FFF:
                out.append(ord(ch))
    return out


def fetch(path: str, sha: str) -> bytes:
    url = f"https://github.com/google/fonts/raw/{COMMIT}/{path}"
    data = urllib.request.urlopen(url).read()
    if hashlib.sha256(data).hexdigest() != sha:
        raise SystemExit(f"{url}: unexpected sha256")
    return data


def cut(data: bytes, axes: dict, codepoints: list[int], features: list[str]) -> bytes:
    font = instancer.instantiateVariableFont(TTFont(io.BytesIO(data)), axes)
    options = subset.Options()
    options.layout_features = features
    options.hinting = False
    options.name_IDs = ["*"]
    options.notdef_outline = True
    sub = subset.Subsetter(options)
    sub.populate(unicodes=codepoints)
    sub.subset(font)
    out = io.BytesIO()
    font.save(out)
    return out.getvalue()


def ranges(rs) -> list[int]:
    return [c for a, b in rs for c in range(a, b + 1)]


def main() -> None:
    latin = fetch(*SOURCES["latin"])
    ja = fetch(*SOURCES["ja"])
    outputs = {
        "latin-400": cut(latin, {"wght": 400, "wdth": 100}, ranges(LATIN), ["kern"]),
        "latin-700": cut(latin, {"wght": 700, "wdth": 100}, ranges(LATIN), ["kern"]),
        "ja-400": cut(ja, {"wght": 400}, ranges(JA) + jis_level1(), []),
    }
    for name, data in outputs.items():
        (HERE / f"{name}.ttf.z").write_bytes(zlib.compress(data, 9))
        print(f"{name}: {len(data)} bytes, {len(zlib.compress(data, 9))} compressed")


if __name__ == "__main__":
    main()
