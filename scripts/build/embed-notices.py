"""Embed readable notices for all locked Windows dependencies; no network access."""
from pathlib import Path
import re, subprocess, zlib
ROOT=Path(__file__).resolve().parents[2]
def collect(base):
    return [(p.relative_to(base).as_posix(),p.read_text(encoding="utf-8",errors="replace")) for p in sorted(base.rglob("*")) if p.is_file() and p.name.upper().startswith(("LICENSE","LICENCE","COPYING","NOTICE","PATENTS","COPYRIGHT","AUTHORS"))]
def main():
    sections=["KSIP — Third-party notices\nWebView2 Runtime is supplied by Microsoft separately.\n"]
    def add(name,files):
        assert files, "Missing notices: "+name
        for path,body in files: sections.append("\n===== "+name+"/"+path+" =====\n"+body)
    for name in ["baresip","re","opus","libg722","libressl"]:add(name,collect(ROOT/"temp/vendor"/name))
    add("google-webrtc", [("LICENSE.md", (ROOT/"temp/build/webrtc-notices/LICENSE.md").read_text(encoding="utf-8"))])
    tree=subprocess.check_output(["cargo","tree","--locked","--offline","--manifest-path",str(ROOT/"src-tauri/Cargo.toml"),"--target","x86_64-pc-windows-msvc","--prefix","none","--format","{p}"],encoding="utf-8")
    cache=next(Path.home().glob(".cargo/registry/src/index.crates.io-*"))
    packages=set()
    for line in tree.splitlines():
        m=re.match(r"([\w-]+) v([\w.+-]+)",line)
        if m and m[1]!="ksip":packages.add(m[1]+"-"+m[2])
    for name in sorted(packages):add(name,collect(cache/name) or collect(ROOT/"licenses/upstream"/name))
    output=ROOT/"temp/build/third-party-notices.txt"
    output.parent.mkdir(parents=True,exist_ok=True)
    content="\n".join(sections)
    if not output.exists() or output.read_text(encoding="utf-8") != content:
        output.write_text(content,encoding="utf-8")
    compressed=ROOT/"temp/build/third-party-notices.zlib"
    data=zlib.compress(output.read_bytes(),9)
    if not compressed.exists() or compressed.read_bytes()!=data: compressed.write_bytes(data)
    print("Prepared embedded notices:",len(packages),"Rust packages, 6 native packages")
if __name__=="__main__": main()
