"""Fail if anything Git tracks names the local network, a host, or a secret."""
import pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parents[2]

# 公開してよいのは手順だけ。接続先と秘密は local-asterisk/ に置く。
PATTERNS = [
    ("私有IPアドレス", re.compile(r"\b(?:10\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}|192\.168\.\d{1,3})\.\d{1,3}\b")),
    ("社内ホスト名", re.compile(r"\b[A-Za-z0-9][A-Za-z0-9-]*\.(?:local|lan|internal|intra)\b")),
    ("32桁の秘密らしき文字列", re.compile(r"(?<![0-9a-fA-F])[0-9a-f]{32}(?![0-9a-fA-F])")),
    # 行頭に限る。本物のPEMは必ず行頭から始まり、ソース中の文字列リテラルは通る。
    ("PEMの中身", re.compile(r"^-----BEGIN (?:RSA |EC )?PRIVATE KEY-----|^-----BEGIN CERTIFICATE-----")),
]
# 依存の固定に使うハッシュと、説明のための例示アドレスは秘密ではない。
ALLOWED = [
    re.compile(r'"(?:sha256|checksum|commit|deps_sha256|build_commit)"\s*:\s*"[0-9a-f]+"'),
    re.compile(r"codeload\.github\.com/[\w.-]+/[\w.-]+/tar\.gz/[0-9a-f]{40}"),
    re.compile(r"\b(?:192\.0\.2|198\.51\.100|203\.0\.113)\.\d{1,3}\b"),  # RFC 5737 の例示用
    re.compile(r"\b(?:127\.0\.0\.1|0\.0\.0\.0)\b"),
    re.compile(r"[0-9a-f]{40}"),  # git の SHA-1
]
# 中身が秘密そのものではなく、形式上ハッシュを大量に含むもの。
SKIP = {"deps/native-sources.lock.json", "src-tauri/Cargo.lock", "scripts/test/no-secrets.py"}
BINARY = {".tar", ".gz", ".exe", ".ico", ".png", ".wav", ".pdb"}


def tracked():
    listing = subprocess.check_output(["git", "ls-files"], cwd=ROOT, text=True, encoding="utf-8")
    return [line for line in listing.splitlines() if line]


def main():
    problems = []
    for name in tracked():
        if name in SKIP or pathlib.Path(name).suffix.lower() in BINARY:
            continue
        path = ROOT / name
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for number, line in enumerate(text.splitlines(), 1):
            for reason, pattern in PATTERNS:
                for found in pattern.findall(line):
                    if any(allowed.search(line) for allowed in ALLOWED):
                        continue
                    problems.append("%s:%d %s: %s" % (name, number, reason, found))
    if problems:
        print("\n".join(problems))
        raise SystemExit("公開してはいけない情報が追跡対象にあります: %d件" % len(problems))
    print("tracked files:", len(tracked()), "— 接続先・ホスト名・秘密は見つからない")


if __name__ == "__main__":
    main()
