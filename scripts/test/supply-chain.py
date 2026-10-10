"""Fail closed on unpinned/untrusted/too-new dependencies; retain audit evidence."""
import pathlib,tarfile,os
import datetime,hashlib,json,pathlib,re,subprocess,tomllib,urllib.error,urllib.request
ROOT=pathlib.Path(__file__).resolve().parents[2]
# Nothing published in the last seven days is accepted, whenever this runs.
CUTOFF=datetime.datetime.now(datetime.timezone.utc)-datetime.timedelta(days=7)
def get(url,headers=None):return json.load(urllib.request.urlopen(urllib.request.Request(url,headers={'User-Agent':'ksip-audit/0.1',**(headers or {})}),timeout=40))
def old(date):return datetime.datetime.fromisoformat(date.replace('Z','+00:00'))<=CUTOFF
# What is built is the unpacked tree, not the archive, so the tree is compared
# with the verified archive file by file. Only what scripts/build/patch-baresip.py
# writes (PATCHED) or adds (ADDED) may differ; anything else is a change nobody
# asked for. The WebRTC checkout is a git tree, and there the same rule reads:
# only what scripts/build/patch-webrtc.py touches may show up in git status.
PATCHED={'re':{'src/sipevent/subscribe.c','src/rtp/rtp.c','src/sip/transp.c'},
         'baresip':{'src/main.c','src/stream.c','src/ua.c','CMakeLists.txt'}}
ADDED={'baresip':('modules/ksip_audio/','modules/ksip_audio_filter/','modules/ksip/','modules/ksip_ctrl/')}
WEBRTC_TOUCHED={'BUILD.gn','modules/audio_device/win/core_audio_utility_win.cc',
                'modules/audio_device/win/core_audio_utility_win.h','modules/audio_device/win/core_audio_base_win.cc',
                'modules/audio_device/win/core_audio_input_win.cc','modules/audio_device/win/core_audio_output_win.cc','ksip_bridge/'}
# Tauri publishes its advisories as GitHub repository advisories and files none with
# RustSec (its 2026 advisories are in neither RustSec nor, for weeks after, GitHub's
# reviewed database), so the repositories behind the crates of these owners are asked
# directly. Their version ranges are typed by the maintainers and unchecked, so a range
# that cannot be read fails the check instead of being skipped.
ADVISORY_REPO_OWNERS={'tauri-apps'}
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
def crate_repository(cargo_home,name,version):
    """The GitHub (owner, repo) a crate names as its repository, read from its unpacked tree or its .crate archive; None when it names none."""
    manifest=None
    for path in cargo_home.glob(f'registry/src/index.crates.io-*/{name}-{version}/Cargo.toml'):
        manifest=path.read_text(encoding='utf-8');break
    if manifest is None:
        for crate in cargo_home.glob(f'registry/cache/index.crates.io-*/{name}-{version}.crate'):
            with tarfile.open(crate) as tar:manifest=tar.extractfile(f'{name}-{version}/Cargo.toml').read().decode('utf-8');break
    assert manifest is not None,f'{name} {version}: neither unpacked nor cached (cargo fetch --locked first)'
    m=re.match(r'https?://github\.com/([^/]+)/([^/#?]+)',tomllib.loads(manifest).get('package',{}).get('repository') or '')
    return (m.group(1),m.group(2).removesuffix('.git')) if m else None
def semver(text):
    """(core, prerelease) of a version as advisories write them ('3', '1.3', 'v2.0.0-beta.19'); None when unreadable."""
    m=re.fullmatch(r'v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?',text.strip())
    if not m:return None
    pre=tuple((0,int(p)) if p.isdigit() else (1,p) for p in m.group(4).split('.')) if m.group(4) else None
    return (int(m.group(1)),int(m.group(2) or 0),int(m.group(3) or 0)),pre
def precedes(a,b):
    """Semver precedence; a release comes after every pre-release of its core."""
    if a[0]!=b[0]:return a[0]<b[0]
    if a[1] is None or b[1] is None:return a[1] is not None and b[1] is None
    return a[1]<b[1]
def affected(version,range_text):
    """Whether the version is inside a range like '>= 2.0.0, <= 2.11.5'; None when the range cannot be read."""
    v=semver(version);assert v,version
    for part in range_text.split(','):
        m=re.fullmatch(r'\s*(<=|>=|<|>|=)?\s*(\S+)\s*',part);b=semver(m.group(2)) if m else None
        if not b:return None
        op=m.group(1) or '='
        if not {'<':precedes(v,b),'<=':not precedes(b,v),'>':precedes(b,v),'>=':not precedes(v,b),'=':not precedes(v,b) and not precedes(b,v)}[op]:return False
    return True
def repository_advisories(lock,cargo_home):
    """Published GitHub repository advisories of ADVISORY_REPO_OWNERS, matched against the lock by crate name: (repositories, advisories, hits, unreadable)."""
    locked={p['name'].lower():p['version'] for p in lock['package'] if 'source' in p}
    repos={}
    for p in lock['package']:
        if 'source' in p and (repo:=crate_repository(cargo_home,p['name'],p['version'])) and repo[0] in ADVISORY_REPO_OWNERS:
            repos.setdefault(repo,set()).add(p['name'])
    token=os.environ.get('GH_TOKEN') or os.environ.get('GITHUB_TOKEN')
    headers={'Accept':'application/vnd.github+json','X-GitHub-Api-Version':'2022-11-28',**({'Authorization':f'Bearer {token}'} if token else {})}
    advisories=[];hits=[];unreadable=[]
    for owner,repo in sorted(repos):
        page=1
        while True:
            try:batch=get(f'https://api.github.com/repos/{owner}/{repo}/security-advisories?state=published&per_page=100&page={page}',headers)
            except urllib.error.HTTPError as e:
                raise SystemExit(f'GitHub API {e.code} for {owner}/{repo}: {e.read()[:200]!r} (unauthenticated callers get 60 requests an hour per address; set GH_TOKEN)')
            for a in batch:
                entries=[]
                for v in a['vulnerabilities']:
                    name=(v['package']['name'] or '').lower();rng=v['vulnerable_version_range'] or ''
                    entry={'ecosystem':v['package']['ecosystem'],'name':v['package']['name'],'range':rng,'patched':v['patched_versions']}
                    if name in locked:
                        result=affected(locked[name],rng)
                        entry.update(locked=locked[name],affected=result)
                        if result is None:unreadable.append((a['ghsa_id'],name,rng))
                        elif result:hits.append((a['ghsa_id'],a['severity'],name,locked[name],rng,v['patched_versions']))
                    entries.append(entry)
                advisories.append({'repository':f'{owner}/{repo}','ghsa_id':a['ghsa_id'],'cve_id':a['cve_id'],'severity':a['severity'],
                                   'published_at':a['published_at'],'summary':a['summary'],'vulnerabilities':entries})
            if len(batch)<100:break
            page+=1
    return repos,advisories,hits,unreadable
def main():
    (ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)
    evidence={'cutoff':CUTOFF.isoformat(),'rust':[],'npm':[],'native':[]}
    # The cargo home is where cargo says it is (CARGO_HOME), not always under the user's folder.
    cargo_home=pathlib.Path(os.environ.get('CARGO_HOME') or pathlib.Path.home()/'.cargo')
    cache=next(cargo_home.glob('registry/index/index.crates.io-*/.cache'))
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
    # The CycloneDX schemas the SBOM is checked against: kept as fetched, from a release old enough.
    schema=json.loads((ROOT/'deps/cyclonedx/schema.lock.json').read_text())
    assert old(schema['source_date']),schema['version']
    for name,f in schema['files'].items():
        assert hashlib.sha256((ROOT/'deps/cyclonedx'/name).read_bytes()).hexdigest()==f['sha256'],name
    evidence['schema']=[{'name':name,**f,'version':schema['version'],'commit':schema['commit']} for name,f in schema['files'].items()]
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
    # Repository advisories of the owners in ADVISORY_REPO_OWNERS, matched against the lock.
    repos,advisories,hits,unreadable=repository_advisories(lock,cargo_home)
    (ROOT/'temp/reports/repo-advisories.json').write_text(json.dumps({'owners':sorted(ADVISORY_REPO_OWNERS),
        'repositories':{f'{o}/{r}':sorted(c) for (o,r),c in sorted(repos.items())},'advisories':advisories,'hits':hits,'unreadable':unreadable},indent=2)+'\n')
    problems=[f'{g}: {n} range {r!r} cannot be read; look at it by hand' for g,n,r in unreadable]
    problems+=[f'{g} ({s}): {n} {v} is in {r!r}, patched {p!r}' for g,s,n,v,r,p in hits]
    assert not problems,'\n'.join(problems)
    print('repository advisories:',len(repos),'repositories,',len(advisories),'published advisories, none hits the lock')
if __name__=='__main__':main()
