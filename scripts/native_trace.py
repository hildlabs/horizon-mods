"""Which native mod a compiler error is in, as the launcher traces it (launcher/src/native_mods/diagnostics.rs).

An error is a mod's when its primary span lies inside what that mod brought into the staged game: one of its
helpers (the function of the helper's name, absent from the pristine source) or one of its support modules (the
inline module of its name, absent from the pristine source). An error that more than one mod's code holds, or none,
names no mod.
"""
import json
import re
from pathlib import Path

PLAN = ".horizon-mixins-stage.json"


def masked(src):
    """`src` (bytes) with comments, strings and char literals blanked, so braces and keywords are code's."""
    out = bytearray(src)
    i, n = 0, len(src)

    def blank(a, b):
        for k in range(a, min(b, n)):
            if out[k] != ord("\n"):
                out[k] = ord(" ")

    while i < n:
        c = src[i]
        if src.startswith(b"//", i):
            end = src.find(b"\n", i)
            end = n if end < 0 else end
            blank(i, end)
            i = end
        elif src.startswith(b"/*", i):
            depth, k = 1, i + 2
            while k < n and depth:
                if src.startswith(b"/*", k):
                    depth, k = depth + 1, k + 2
                elif src.startswith(b"*/", k):
                    depth, k = depth - 1, k + 2
                else:
                    k += 1
            blank(i, k)
            i = k
        elif c in b"rb" and (i == 0 or not (chr(src[i - 1]).isalnum() or src[i - 1] == ord("_"))) and \
                re.match(rb"(?:br|r)(#*)\"", src[i:i + 260]):
            hashes = re.match(rb"(?:br|r)(#*)\"", src[i:i + 260])
            close = b'"' + hashes.group(1)
            end = src.find(close, i + hashes.end())
            end = n if end < 0 else end + len(close)
            blank(i, end)
            i = end
        elif c == ord('"'):
            k = i + 1
            while k < n and src[k] != ord('"'):
                k += 2 if src[k] == ord("\\") else 1
            blank(i, k + 1)
            i = k + 1
        elif c == ord("'"):
            # A char literal ('x', '\n', '\u{1F600}'), else a lifetime ('a).
            if i + 1 < n and src[i + 1] == ord("\\"):
                end = src.find(b"'", i + 2)
                end = n if end < 0 else end + 1
                blank(i, end)
                i = end
            elif i + 2 < n and src[i + 2] == ord("'"):
                blank(i, i + 3)
                i += 3
            else:
                m = re.match(rb"'([^\x00-\x7f]+)'", src[i:i + 8])
                if m:
                    blank(i, i + m.end())
                    i += m.end()
                else:
                    i += 1
        else:
            i += 1
    return bytes(out)


def item_ranges(code, keyword, name):
    """Byte ranges of the `fn NAME` or `mod NAME { … }` items in masked source `code`, through their closing brace."""
    pattern = rb"\b%s\s+%s\b" % (keyword.encode(), re.escape(name.encode()))
    ranges = []
    for m in re.finditer(pattern, code):
        # The body's `{`, unless a `;` ends the item first; either outside the signature's () and [] (`[f64; 3]`).
        open_at, nested, k = -1, 0, m.end()
        while k < len(code):
            c = code[k]
            if c in b"([":
                nested += 1
            elif c in b")]":
                nested -= 1
            elif nested <= 0 and c in b"{;":
                open_at = k if c == ord("{") else -1
                break
            k += 1
        if open_at < 0:
            continue  # a declaration without a body
        if keyword == "mod" and code[m.end():open_at].strip():
            continue
        depth, k = 0, open_at
        while k < len(code):
            if code[k] == ord("{"):
                depth += 1
            elif code[k] == ord("}"):
                depth -= 1
                if depth == 0:
                    ranges.append(_with_call(code, name, m.start(), k + 1) if keyword == "fn" else (m.start(), k + 1))
                    break
            k += 1
    return ranges


def _with_call(code, name, start, end):
    """The block horizon-mixins wraps a helper in, `{ fn NAME(…) { … } NAME(…) }`, when that is all the block holds:
    an error in the generated call (an argument the helper's signature no longer takes) is the helper's too."""
    before = re.search(rb"\{\s*(?:#\[[^\]]*\]\s*)*\Z", code[max(0, start - 400):start])
    after = re.match(rb"\s*%s\s*\([^{}]*\)\s*;?\s*\}" % re.escape(name.encode()), code[end:])
    if before and after:
        return start - (len(before.group(0))), end + after.end()
    return start, end


def helper_name(source):
    m = re.search(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)", source or "")
    return m.group(1) if m else None


class Tracer:
    """Traces errors in the staged workspace `stage` (horizon-mixins' output, holding its plan) whose pristine source
    is `pristine`, to the mods in `mods` ({(id, version)})."""

    def __init__(self, stage, pristine, mods):
        self.stage, self.pristine = Path(stage).resolve(), Path(pristine)
        plan = json.loads((self.stage / PLAN).read_text())
        self.entries = []  # (id, version, file, keyword, name)
        for e in plan.get("injections", []):
            name = helper_name(e.get("helper_source"))
            if name:
                self.entries.append((e.get("mod_id"), e.get("mod_version"), e.get("file"), "fn", name))
        for e in plan.get("modules", []):
            self.entries.append((e.get("mod_id"), e.get("mod_version"), e.get("file"), "mod", e.get("name")))
        self.entries = [e for e in self.entries if (e[0], e[1]) in mods and e[2] and e[4]]
        self.cache, self.code = {}, {}

    def _masked(self, path):
        if path not in self.code:
            self.code[path] = masked(path.read_bytes()) if path.is_file() else b""
        return self.code[path]

    def _ranges(self, rel, keyword, name):
        key = (rel, keyword, name)
        if key not in self.cache:
            found = []
            if (self.stage / rel).is_file() and not item_ranges(self._masked(self.pristine / rel), keyword, name):
                found = item_ranges(self._masked(self.stage / rel), keyword, name)
            self.cache[key] = found
        return self.cache[key]

    def owner(self, span):
        """The mod (id) whose code holds a cargo JSON span, or None."""
        try:
            path = (self.stage / span["file_name"]).resolve()
            rel = path.relative_to(self.stage).as_posix()
        except (KeyError, ValueError):
            return None
        start, end = span.get("byte_start", 0), span.get("byte_end", 0)
        owners = {i for i, _, file, keyword, name in self.entries
                  if file == rel and any(a <= start and end <= b for a, b in self._ranges(rel, keyword, name))}
        return owners.pop() if len(owners) == 1 else None


def errors(cargo_stdout):
    """The errors in `cargo --message-format=json` output: {code, message, rendered, spans (primary)}."""
    found = []
    for line in cargo_stdout.splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if value.get("reason") != "compiler-message":
            continue
        message = value.get("message") or {}
        if message.get("level") != "error":
            continue
        found.append({
            "code": (message.get("code") or {}).get("code") or "E_COMPILE",
            "message": message.get("message", ""),
            "rendered": message.get("rendered") or "",
            "spans": [s for s in message.get("spans", []) if s.get("is_primary")],
        })
    return found
