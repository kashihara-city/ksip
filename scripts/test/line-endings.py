"""Fail if a file under src-web/ is not CRLF in the working tree; it is embedded as it is, so the exe would differ."""
import pathlib, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parents[2]

# 改行がバイナリへ届くのは src-web/ だけ。Rust と C/C++ はコンパイラが正規化する。
# .gitattributes が eol=crlf を決めているので、作業ツリーは crlf、索引は lf のはず。
TARGET = "src-web"


def main():
    # 各行は "i/<索引> w/<作業ツリー> attr/<属性...>\t<パス>"。属性欄は空白を含むので
    # パスはタブで切り出す。
    listing = subprocess.check_output(["git", "ls-files", "--eol", "--", TARGET], cwd=ROOT, text=True, encoding="utf-8")
    problems = []
    for line in listing.splitlines():
        if "\t" not in line:
            continue
        columns, path = line.split("\t", 1)
        index, worktree = columns.split()[:2]
        if index == "i/-text" or worktree == "w/-text":
            continue
        if index != "i/lf" or worktree != "w/crlf":
            problems.append(f"{path}: {index} {worktree}")
    for problem in problems:
        print(problem)
    if problems:
        print(f"FAIL: {len(problems)} files under {TARGET}/ are not CRLF in the working tree")
        return 1
    print(f"PASS: every tracked file under {TARGET}/ is CRLF in the working tree and LF in the index")
    return 0


if __name__ == "__main__":
    sys.exit(main())
