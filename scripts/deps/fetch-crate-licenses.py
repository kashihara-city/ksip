"""Read missing license notices from each crate's exact upstream VCS commit.
Only text is fetched; nothing is executed. Record URLs and content hashes.
"""
from pathlib import Path
import hashlib,json,re,subprocess,tomllib,urllib.request
ROOT=Path(__file__).resolve().parents[2]
cache=next(Path.home().glob('.cargo/registry/src/index.crates.io-*'))
def read(url):
    return urllib.request.urlopen(urllib.request.Request(url,headers={'User-Agent':'ksip-notices/0.1'}),timeout=40).read()
def is_notice(p):return p.upper().startswith(('LICENSE','LICENCE','COPYING','COPYRIGHT','NOTICE'))
def main():
    (ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)
    tree=subprocess.check_output(['cargo','tree','--locked','--offline','--manifest-path',str(ROOT/'src-tauri/Cargo.toml'),'--target','x86_64-pc-windows-msvc','--prefix','none','--format','{p}'],encoding='utf-8')
    seen=set();memo={};evidence=[]
    for line in tree.splitlines():
        m=re.match(r'([\w-]+) v([\w.+-]+)',line)
        if not m or m[1]=='ksip':continue
        key=f'{m[1]}-{m[2]}'
        if key in seen:continue
        seen.add(key);src=cache/key
        if any(p.is_file() and is_notice(p.name) for p in src.rglob('*')):continue
        meta=tomllib.loads((src/'Cargo.toml').read_text(encoding='utf-8'))['package']
        sha=json.loads((src/'.cargo_vcs_info.json').read_text())['git']['sha1']
        repo=meta['repository'].rstrip('/').removesuffix('.git').removeprefix('https://github.com/')
        assert re.fullmatch(r'[\w.-]+/[\w.-]+',repo) and re.fullmatch('[a-f0-9]{40}',sha)
        ref=(repo,sha)
        if key=='selectors-0.36.1':
            # selectors contains MPL notices in source headers, but no copy of
            # the standard license text. Use the identical standard MPL-2.0
            # text from our locked cssparser dependency and ship full source.
            data=(cache/'cssparser-0.36.0/LICENSE').read_bytes()
            memo[ref]=[('LICENSE-MPL-2.0','crate://cssparser/0.36.0/LICENSE',data)]
        if ref not in memo:
            entries=json.loads(read(f'https://api.github.com/repos/{repo}/contents/?ref={sha}'))
            memo[ref]=[(e['name'],e['download_url'],read(e['download_url'])) for e in entries if e['type']=='file' and is_notice(e['name'])]
        assert memo[ref],(key,'no upstream root notices')
        for name,url,data in memo[ref]:
            dest=ROOT/'licenses/upstream'/key/name;dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(data)
            evidence.append({'crate':key,'commit':sha,'url':url,'path':dest.relative_to(ROOT).as_posix(),'sha256':hashlib.sha256(data).hexdigest()})
    (ROOT/'temp/reports/license-sources.json').write_text(json.dumps(evidence,indent=2),encoding='utf-8')
    print('Pinned upstream license texts:',len(evidence))
if __name__=='__main__':main()
