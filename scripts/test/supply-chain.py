"""Fail closed on unpinned/untrusted/too-new dependencies; retain audit evidence."""
import pathlib,tarfile
import datetime,hashlib,json,pathlib,re,subprocess,tomllib,urllib.request
ROOT=pathlib.Path(__file__).resolve().parents[2]
# Nothing published in the last seven days is accepted, whenever this runs.
CUTOFF=datetime.datetime.now(datetime.timezone.utc)-datetime.timedelta(days=7)
def get(url):return json.load(urllib.request.urlopen(urllib.request.Request(url,headers={'User-Agent':'ksip-audit/0.1'}),timeout=40))
def old(date):return datetime.datetime.fromisoformat(date.replace('Z','+00:00'))<=CUTOFF
# What is built is the unpacked tree, not the archive, so the tree is compared
# with the verified archive file by file. Only what scripts/build/patch-baresip.py
# writes (PATCHED) or adds (ADDED) may differ; anything else is a change nobody
# asked for. The WebRTC checkout is a git tree, and there the same rule reads:
# only what scripts/build/patch-webrtc.py touches may show up in git status.
PATCHED={'re':{'cmake/re-config.cmake','src/sipevent/subscribe.c'},
         'baresip':{'src/main.c','CMakeLists.txt'}}
ADDED={'baresip':('modules/ksip_audio/','modules/postlab/','modules/ksip/')}
WEBRTC_TOUCHED={'BUILD.gn','modules/audio_device/win/core_audio_utility_win.cc',
                'modules/audio_device/win/core_audio_utility_win.h','modules/audio_device/win/core_audio_base_win.cc','ksip_bridge/'}
def tree_differences(name,archive,tree):
    """The files of an unpacked tree that are not as the archive has them, patches aside."""
    if not tree.exists():return []
    expected={}
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            parts=pathlib.PurePosixPath(member.name).parts
            if member.isfile() and len(parts)>1:
                expected['/'.join(parts[1:])]=hashlib.sha256(tar.extractfile(member).read()).hexdigest()
    patched=PATCHED.get(name,set());added=ADDED.get(name,())
    problems=[]
    for path in sorted(p for p in tree.rglob('*') if p.is_file()):
        rel=path.relative_to(tree).as_posix()
        if rel in patched or rel.startswith(added):continue
        if rel not in expected:problems.append(f'{name}: {rel} is not in the pinned archive')
        elif expected[rel]!=hashlib.sha256(path.read_bytes()).hexdigest():problems.append(f'{name}: {rel} differs from the pinned archive')
    for rel in expected:
        if rel not in patched and not (tree/rel).is_file():problems.append(f'{name}: {rel} is missing from the unpacked tree')
    return problems
def checkout_differences(source):
    """Changes in the WebRTC checkout beyond what the bridge patch makes."""
    if not (source/'.git').exists():return []
    status=subprocess.check_output(['git','status','--porcelain'],cwd=source,text=True,encoding='utf-8')
    problems=[]
    for line in status.splitlines():
        path=line[3:].strip().replace('\\','/')
        if path in WEBRTC_TOUCHED or any(path.startswith(t) for t in WEBRTC_TOUCHED if t.endswith('/')):continue
        problems.append(f'google-webrtc: {line.strip()} is a change the bridge patch does not make')
    return problems
def main():
    (ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)
    evidence={'cutoff':CUTOFF.isoformat(),'rust':[],'npm':[],'native':[]}
    cache=next(pathlib.Path.home().glob('.cargo/registry/index/index.crates.io-*/.cache'))
    lock=tomllib.loads((ROOT/'src-tauri/Cargo.lock').read_text())
    for p in lock['package']:
        if 'source' not in p:continue
        assert p['source']=='registry+https://github.com/rust-lang/crates.io-index',p
        n=p['name'];key=n if len(n)==1 else '2/'+n if len(n)==2 else '3/'+n[0]+'/'+n if len(n)==3 else n[:2]+'/'+n[2:4]+'/'+n
        records=[json.loads(e) for e in (cache/key).read_bytes().split(b'\0') if e.startswith(b'{')]
        d=next(d for d in records if d['vers']==p['version'])
        assert d['cksum']==p['checksum'],p
        assert d.get('pubtime') and old(d['pubtime']),(p,d.get('pubtime'))
        evidence['rust'].append({'name':n,'version':p['version'],'published':d['pubtime'],'checksum':p['checksum']})
    # v0.1.1 embeds plain HTML/CSS/JS directly: no npm resolution or build.
    assert not (ROOT/'package.json').exists(), 'Review any new npm dependencies before packaging'
    for name,p in json.loads((ROOT/'deps/native-sources.lock.json').read_text()).items():
        assert old(p['source_date'])
        if 'published_at' in p:assert old(p['published_at'])
        if p.get('source_type') == 'git':
            if name == 'depot-tools': checkout=ROOT/'temp/d'
            else: checkout=ROOT/'temp/w/src'
            if checkout.exists():
                head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=checkout,text=True).strip()
                assert head==p['commit'],(name,head)
            if name == 'google-webrtc' and checkout.exists():
                digest=hashlib.sha256((checkout/'DEPS').read_bytes()).hexdigest()
                assert digest==p['deps_sha256'],name
                build_head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=checkout/'build',text=True).strip()
                assert build_head==p['build_commit'],('webrtc-build',build_head)
                assert old(p['build_source_date'])
        else:
            digest=hashlib.sha256((ROOT/'deps'/f'{name}.tar.gz').read_bytes()).hexdigest()
            assert digest==p['sha256'],name
            problems=tree_differences(name,ROOT/'deps'/f'{name}.tar.gz',ROOT/'temp/vendor'/name)
            assert not problems,'\n'.join(problems)
        evidence['native'].append({'name':name,**p})
    problems=checkout_differences(ROOT/'temp/w/src')
    assert not problems,'\n'.join(problems)
    (ROOT/'temp/reports/dependency-evidence.json').write_text(json.dumps(evidence,indent=2)+'\n')
    print('Age, registry, lock/hash checks passed:',{k:len(v) for k,v in evidence.items() if isinstance(v,list)})
    # cargo-audit reads the same lock and reports RustSec advisories. It is a
    # developer tool like cargo itself, so it is not pinned here, but a missing
    # tool fails the check instead of silently skipping the audit.
    try:
        report=subprocess.run(['cargo','audit','--file',str(ROOT/'src-tauri/Cargo.lock'),'--json'],
                              capture_output=True,text=True,timeout=600)
    except FileNotFoundError:
        raise SystemExit('cargo-audit is required: cargo install cargo-audit --locked')
    audit=json.loads(report.stdout)
    (ROOT/'temp/reports/cargo-audit.json').write_text(json.dumps(audit,indent=2)+'\n')
    assert not audit['vulnerabilities']['found'],audit['vulnerabilities']['list']
    warnings={k:len(v) for k,v in audit.get('warnings',{}).items() if v}
    print('cargo audit:',audit['lockfile']['dependency-count'],'crates, no vulnerabilities, warnings',warnings or 'none')

    # OSV commit queries complement ecosystem package audits; a clean response
    # does not establish complete C/C++ advisory coverage.
    queries=[{'commit':p['commit']} for p in evidence['native'] if 'commit' in p]
    queries += [{'commit':p['build_commit']} for p in evidence['native'] if 'build_commit' in p]
    data=json.dumps({'queries':queries}).encode()
    req=urllib.request.Request('https://api.osv.dev/v1/querybatch',data=data,headers={'Content-Type':'application/json'})
    result=json.load(urllib.request.urlopen(req,timeout=60))
    (ROOT/'temp/reports/native-osv.json').write_text(json.dumps({'queries':queries,'response':result,'limitation':'Commit matching only; no complete C/C++ vulnerability coverage claim.'},indent=2)+'\n')
    assert not any(r.get('vulns') for r in result['results']),result
    print('OSV pinned native commit queries: no matches')
if __name__=='__main__':main()
