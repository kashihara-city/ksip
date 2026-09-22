"""Fail if release/ksip.exe carries a path of the build machine; the binary would then change with its folder."""
import json, os, pathlib, re, sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
EXE = ROOT / "release/ksip.exe"
REPORT = ROOT / "temp/reports/build-paths.json"

# 同じソースを別の場所で組んでも同じexeになるためには、組んだ機械の場所が
# バイナリに残っていてはならない。残りやすいのは __FILE__、CMakeの
# インストール先、cargoのレジストリとリポジトリの絶対パス。
# リポジトリからの相対パス（temp\vendor\... など）は __FILE__ の残りで、
# どこで組んでも同じ値になるので構わない。
PATTERNS = [
    ("Windowsの絶対パス", re.compile(rb"[A-Za-z]:[\\/](?:Users|home|Desktop|src|temp|program)[\\/][\x20-\x7e]{1,240}")),
    ("cargoレジストリの絶対パス", re.compile(rb"[A-Za-z]:[\\/][\x20-\x7e]{0,120}?[\\/]\.cargo[\\/]registry[\\/][\x20-\x7e]{0,240}")),
]
# 場所に依らず同じ値になる既知の文字列。LibreSSLのWindows既定ディレクトリ。
ALLOWED = [re.compile(rb"^[A-Za-z]:/Windows/libressl/ssl")]


def machine_needles():
    """利用者名とリポジトリの場所は、上の型に当てはまらなくても残ってはいけない。"""
    for label, needle in (("リポジトリの場所", str(ROOT)), ("利用者のフォルダ", os.environ.get("USERPROFILE", ""))):
        if needle:
            for form in sorted({needle, needle.replace("\\", "/")}):
                yield label, re.compile(re.escape(form.encode()) + rb"[\x20-\x7e]{0,240}")


def main():
    if not EXE.is_file():
        print(f"not built: {EXE}")
        return 2
    data = EXE.read_bytes()
    found = {}
    taken = []  # 既に数えた範囲。短い型が長い一致の中をもう一度数えないため。
    for label, pattern in list(PATTERNS) + list(machine_needles()):
        for match in pattern.finditer(data):
            if any(start <= match.start() < end for start, end in taken):
                continue
            text = match.group()
            if any(a.search(text) for a in ALLOWED):
                continue
            taken.append(match.span())
            key = text.decode("latin1")
            found.setdefault(key, {"kind": label, "count": 0})["count"] += 1
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps({"exe": str(EXE), "size": len(data), "found": found}, ensure_ascii=False, indent=2), encoding="utf-8")
    for text, info in sorted(found.items(), key=lambda item: -item[1]["count"])[:20]:
        print(f"{info['kind']}: {info['count']}x {text[:120]}")
    if found:
        print(f"FAIL: {sum(i['count'] for i in found.values())} machine paths in {EXE.name}; see {REPORT}")
        return 1
    print(f"PASS: no build machine path in {EXE.name} ({len(data)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
