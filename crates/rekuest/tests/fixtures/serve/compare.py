"""Diff two captures (python/ vs rust/), ignoring ids, uuids and times.

    python compare.py python rust
"""

import json
import re
import sys
from pathlib import Path

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
TIME = re.compile(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z")
VOLATILE_KEYS = {"id", "ts", "timepoint", "start_time", "end_time"}


def normalize(value, key=None):
    if key in VOLATILE_KEYS and value is not None:
        return f"<{key}>"
    if isinstance(value, dict):
        return {UUID.sub("<uuid>", k): normalize(v, k) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize(v) for v in value]
    if isinstance(value, str):
        return TIME.sub("<time>", UUID.sub("<uuid>", value))
    return value


def diff(a, b, path="$"):
    if type(a) is not type(b) and not (isinstance(a, (int, float)) and isinstance(b, (int, float))):
        yield f"{path}: {json.dumps(a)[:200]} != {json.dumps(b)[:200]}"
    elif isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a:
                yield f"{path}.{k}: missing in first ({json.dumps(b[k])[:120]})"
            elif k not in b:
                yield f"{path}.{k}: missing in second ({json.dumps(a[k])[:120]})"
            else:
                yield from diff(a[k], b[k], f"{path}.{k}")
    elif isinstance(a, list):
        if len(a) != len(b):
            yield f"{path}: length {len(a)} != {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            yield from diff(x, y, f"{path}[{i}]")
    elif a != b:
        yield f"{path}: {json.dumps(a)[:200]} != {json.dumps(b)[:200]}"


def main(first: Path, second: Path) -> int:
    problems = 0
    for file in sorted(first.glob("*.json")):
        other = second / file.name
        if not other.exists():
            print(f"== {file.name}: missing in {second}")
            problems += 1
            continue
        a = normalize(json.loads(file.read_text()))
        b = normalize(json.loads(other.read_text()))
        found = list(diff(a, b))
        if found:
            problems += 1
            print(f"== {file.name}")
            for line in found[:25]:
                print("  ", line)
    print(f"{problems} file(s) differ")
    return 1 if problems else 0


if __name__ == "__main__":
    here = Path(__file__).parent
    sys.exit(main(here / sys.argv[1], here / sys.argv[2]))
