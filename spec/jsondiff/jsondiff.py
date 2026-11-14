"""The differential test for fabric-core's JSON reader (decision 6).

  python3 -I jsondiff.py gen [N]   print N inputs (default 30000), one per line, as hex
  python3 -I jsondiff.py check     read hex lines and print Python's canonical reading

The Rust side is `cargo run -p fabric-core --example jsondiff`. Both print, for
each input, ERR or the same token line:

  {  }  [  ]   k<hex of key>   s<hex of string>   n<number text>   t  f  z

so every decoded string, key and number is compared byte for byte, in
document order, duplicates included. The corpus is deterministic (a fixed
seed), and invented: no real data is ever in it.
"""

import json
import os
import random
import sys


class Obj(tuple):
    pass


class Num(str):
    pass


def _reject(name):
    raise ValueError(name)


def canon(data: bytes) -> str:
    try:
        text = data.decode("utf-8")
        v = json.loads(text, object_pairs_hook=Obj, parse_int=Num, parse_float=Num, parse_constant=_reject)
        out = []
        stack = [v]
        while stack:
            x = stack.pop()
            if isinstance(x, tuple) and len(x) == 2 and x[0] == "\x00close":
                out.append(x[1])
            elif isinstance(x, Obj):
                out.append("{")
                stack.append(("\x00close", "}"))
                for k, val in reversed(x):
                    stack.append(val)
                    stack.append(("\x00key", k))
            elif isinstance(x, tuple) and len(x) == 2 and x[0] == "\x00key":
                out.append("k" + x[1].encode("utf-8").hex())
            elif isinstance(x, list):
                out.append("[")
                stack.append(("\x00close", "]"))
                stack.extend(reversed(x))
            elif isinstance(x, Num):
                out.append("n" + str(x))
            elif isinstance(x, str):
                out.append("s" + x.encode("utf-8").hex())
            elif x is True:
                out.append("t")
            elif x is False:
                out.append("f")
            elif x is None:
                out.append("z")
            else:
                return "ERR"
        return " ".join(out)
    except Exception:
        return "ERR"


# ---- the corpus ----

ASCII = [chr(c) for c in range(0x20, 0x7F)]
WIDE = ["é", "ß", "€", "中", " ", " ", "😀", "𝄞", "\U0010fffd"]
ESCAPES = ['\\"', "\\\\", "\\/", "\\b", "\\f", "\\n", "\\r", "\\t", "\\u0041", "\\u00e9", "\\u20AC",
           "\\uD83D\\uDE00", "\\ud834\\udd1e", "\\u0000", "\\u001f"]
BAD_ESCAPES = ["\\x41", "\\uD800", "\\uDC00", "\\uD800\\u0041", "\\u12", "\\U0041", "\\", "\\uD800x"]


def json_string(rng):
    parts = []
    for _ in range(rng.randrange(0, 12)):
        r = rng.random()
        if r < 0.45:
            parts.append(rng.choice(ASCII).replace("\\", "\\\\").replace('"', '\\"'))
        elif r < 0.65:
            parts.append(rng.choice(WIDE))
        elif r < 0.97:
            parts.append(rng.choice(ESCAPES))
        elif r < 0.985:
            parts.append(rng.choice(BAD_ESCAPES))
        else:
            parts.append(chr(rng.randrange(0, 0x20)))
    return '"' + "".join(parts) + '"'


def number(rng):
    good = ["0", "-0", "7", "-12", "1234567890123456789012", "0.5", "-1.25", "1e10", "2E-3", "6.02e+23"]
    bad = ["01", "1.", ".5", "+1", "1e", "-", "1.5e", "0x10"]
    return rng.choice(bad) if rng.random() < 0.05 else rng.choice(good)


def value(rng, depth):
    r = rng.random()
    if depth > 5 or r < 0.35:
        return rng.choice([json_string(rng), json_string(rng), number(rng), "true", "false", "null"])
    if r < 0.7:
        items = [value(rng, depth + 1) for _ in range(rng.randrange(0, 5))]
        sep = rng.choice([",", ", ", " ,", ",\n"])
        return "[" + sep.join(items) + "]"
    keys = [json_string(rng) for _ in range(rng.randrange(0, 5))]
    if keys and rng.random() < 0.2:
        keys.append(keys[0])
    sep = rng.choice([",", ", ", ",\n  "])
    colon = rng.choice([":", ": ", " : "])
    return "{" + sep.join(k + colon + value(rng, depth + 1) for k in keys) + "}"


INTERESTING = [b"{", b"}", b"[", b"]", b'"', b"\\", b",", b":", b" ", b"\n", b"\x00", b"\x1f", b"\x7f",
               b"\xff", b"\xc3", b"\xed\xa0\x80", b"\xef\xbb\xbf", b"0", b"e", b"-", b"u"]


def mutate(rng, data: bytes) -> bytes:
    b = bytearray(data)
    for _ in range(rng.randrange(1, 4)):
        if not b:
            break
        i = rng.randrange(0, len(b))
        op = rng.random()
        if op < 0.33:
            del b[i]
        elif op < 0.66:
            b[i:i] = rng.choice(INTERESTING)
        else:
            b[i:i + 1] = rng.choice(INTERESTING)
    return bytes(b)


EDGES = ["", " ", "null", "true", "false", "0", "-0", "[]", "{}", "[[]]", '{"a":{}}', '""', '"\\u0000"',
         "[1,]", '{"a":1,}', "01", "1.", "NaN", "Infinity", "-Infinity", "﻿{}", "{} {}", "[" * 100 + "]" * 100,
         '{"a":1,"a":2}', '"\\ud83d\\ude00"', '"\\uD83D"', '[" "]', "\t[ 1 , 2 ]\r\n"]


def corpus(n):
    rng = random.Random(20261114)
    here = os.path.dirname(os.path.abspath(__file__))
    payloads = os.path.join(here, "..", "..", "adapters", "claude-code", "payloads")
    out = [e.encode("utf-8") for e in EDGES]
    for name in sorted(os.listdir(payloads)):
        if name.endswith(".json"):
            with open(os.path.join(payloads, name), "rb") as f:
                out.append(f.read())
    while len(out) < n:
        doc = value(rng, 0)
        if rng.random() < 0.5:
            doc = rng.choice(["", " ", "\n"]) + doc + rng.choice(["", " ", "\n"])
        data = doc.encode("utf-8", "surrogatepass")
        out.append(mutate(rng, data) if rng.random() < 0.4 else data)
    return out


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode == "gen":
        n = int(sys.argv[2]) if len(sys.argv) > 2 else 30000
        for data in corpus(n):
            print(data.hex())
    elif mode == "check":
        for line in sys.stdin:
            print(canon(bytes.fromhex(line.strip())))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
