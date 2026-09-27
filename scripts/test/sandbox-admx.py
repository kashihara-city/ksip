"""Loads docs/admx into gpedit inside Windows Sandbox and checks what enabling, disabling and un-configuring representative policies writes.

Heavy (about 6 minutes) and local only: it needs Windows Sandbox on this machine,
cannot run in CI, and is not part of run.py's groups; run it by hand when the
templates change or before a release.
"""
import argparse, json, shutil, subprocess, sys, time
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ADMX = ROOT / 'docs/admx'
WORK = ROOT / 'temp/build/sandbox-admx'
REPORT = ROOT / 'temp/reports/sandbox-admx.json'
NS = {'p': 'http://schemas.microsoft.com/GroupPolicy/2006/07/PolicyDefinitions'}
INSIDE_IN, INSIDE_OUT = r'C:\ksip-in', r'C:\ksip-out'


def q(tag):
    return '{%s}%s' % (NS['p'], tag)


def read_templates():
    """Policies by name: category, display names and element labels in both
    languages; categories with their display names; and the string table."""
    admx = ET.parse(ADMX / 'ksip.admx').getroot()
    languages = {}
    for lang in ('ja-JP', 'en-US'):
        res = ET.parse(ADMX / lang / 'ksip.adml').getroot()
        strings = {s.get('id'): s.text or '' for s in res.iter(q('string'))}
        labels = {}
        for pres in res.iter(q('presentation')):
            for control in pres:
                label = control.find(q('label'))
                labels[control.get('refId')] = (label.text if label is not None else control.text) or ''
        languages[lang] = (strings, labels)
    def text(lang, ref):
        return languages[lang][0][ref[len('$(string.'):-1]]
    categories = {c.get('name'): {lang: text(lang, c.get('displayName')) for lang in languages} for c in admx.iter(q('category'))}
    parents = {c.get('name'): (c.find(q('parentCategory')).get('ref') if c.find(q('parentCategory')) is not None else None) for c in admx.iter(q('category'))}
    policies = {}
    for pol in admx.iter(q('policy')):
        elements = {}
        found = pol.find(q('elements'))
        for el in (found if found is not None else []):
            kind = el.tag.split('}')[1]
            entry = {'kind': kind, 'label': {lang: languages[lang][1][el.get('id')] for lang in languages}}
            if kind == 'enum':
                entry['items'] = {item.find(q('value'))[0].text or '': {lang: text(lang, item.get('displayName')) for lang in languages} for item in el.iter(q('item'))}
            elements[el.get('valueName')] = entry
        policies[pol.get('name')] = {
            'category': pol.find(q('parentCategory')).get('ref'),
            'name': {lang: text(lang, pol.get('displayName')) for lang in languages},
            'elements': elements,
        }
    return categories, parents, policies


# (policy, state, values to set) and what the key must hold afterwards:
# name -> (kind, data), or None for "absent". Values the step does not name
# are not checked.
STEPS = [
    ('aec', 'Enabled', {}, {'aec': ('DWord', '1')}),
    ('aec', 'Disabled', {}, {'aec': ('DWord', '0')}),
    ('aec', 'NotConfigured', {}, {'aec': ('DWord', '0')}),
    ('sip_port', 'Enabled', {'sip_port': '5070'}, {'sip_port': ('DWord', '5070')}),
    ('sip_port', 'NotConfigured', {}, {'sip_port': ('DWord', '5070')}),
    ('sip_port', 'Disabled', {}, {'sip_port': None}),
    ('transport', 'Enabled', {'transport': 'tls'}, {'transport': ('String', 'tls')}),
    ('transport', 'Disabled', {}, {'transport': None}),
    ('server', 'Enabled', {'server': 'pbx.example'}, {'server': ('String', 'pbx.example')}),
    ('server', 'Disabled', {}, {'server': None}),
    ('button_1', 'Enabled', {'button_1_title': 'Test', 'button_1_kind': 'dial', 'button_1_number': '1001'},
     {'button_1_title': ('String', 'Test'), 'button_1_kind': ('String', 'dial'), 'button_1_number': ('String', '1001')}),
    ('button_1', 'Disabled', {}, {f'button_1_{f}': None for f in ('title', 'kind', 'number', 'transfer', 'pickup')}),
]


def make_plan(categories, parents, policies):
    steps = []
    for policy, state, values, _ in STEPS:
        p = policies[policy]
        chain = []
        c = p['category']
        while c:
            chain.insert(0, categories[c])
            c = parents[c]
        sets = []
        order = list(p['elements'])
        for value_name, value in values.items():
            el = p['elements'][value_name]
            # The dialog's fields are reached by Tab in presentation order, and a
            # dropdown's item by its position (the lists are not sorted).
            sets.append({'kind': el['kind'], 'label': el['label'], 'order': order.index(value_name),
                         'index': list(el['items']).index(value) if el['kind'] == 'enum' else -1,
                         'value': el['items'][value] if el['kind'] == 'enum' else {'ja-JP': value, 'en-US': value}})
        sets.sort(key=lambda s: s['order'])
        steps.append({'policy': policy, 'state': state, 'path': chain, 'name': p['name'], 'set': sets})
    leaves = {}
    for name, p in policies.items():
        leaves.setdefault(p['category'], []).append(p['name'])
    listing = []
    for cat, names in leaves.items():
        chain = []
        c = cat
        while c:
            chain.insert(0, categories[c])
            c = parents[c]
        listing.append({'path': chain, 'policies': names})
    adml = {lang: {s.get('id'): s.text for s in ET.parse(ADMX / lang / 'ksip.adml').getroot().iter(q('string'))} for lang in ('ja-JP', 'en-US')}
    return {
        # The "supported on" box comes just before the first field in Tab order.
        'supported': {lang: adml[lang]['SUPPORTED_KSIP'] for lang in adml},
        'tree': {'ja-JP': ['ローカル コンピューター ポリシー', 'ユーザーの構成', '管理用テンプレート'],
                 'en-US': ['Local Computer Policy', 'User Configuration', 'Administrative Templates']},
        'states': {'ja-JP': {'NotConfigured': '未構成', 'Enabled': '有効', 'Disabled': '無効'},
                   'en-US': {'NotConfigured': 'Not Configured', 'Enabled': 'Enabled', 'Disabled': 'Disabled'}},
        'listing': listing,
        'steps': steps,
    }


def wsb(*args):
    return subprocess.run(['wsb.exe', *args, '--raw'], capture_output=True, text=True, encoding='utf-8', errors='replace')


def running_sandboxes():
    out = wsb('list').stdout.strip()
    try:
        data = json.loads(out) if out else []
    except json.JSONDecodeError:
        return []
    items = data.get('WindowsSandboxEnvironments', data) if isinstance(data, dict) else data
    return [i.get('Id') or i.get('id') for i in items if isinstance(i, dict)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--keep', action='store_true', help='leave the sandbox open at the end, for looking around')
    parser.add_argument('--timeout', type=int, default=900)
    args = parser.parse_args()
    if running_sandboxes():
        raise SystemExit('A Windows Sandbox is already running; close it first (only one can run).')
    categories, parents, policies = read_templates()
    plan = make_plan(categories, parents, policies)
    shutil.rmtree(WORK, ignore_errors=True)
    inside, outside = WORK / 'in', WORK / 'out'
    (inside / 'admx').mkdir(parents=True)
    outside.mkdir(parents=True)
    shutil.copytree(ADMX, inside / 'admx', dirs_exist_ok=True)
    shutil.copy2(Path(__file__).with_suffix('.ps1'), inside / 'sandbox-admx.ps1')
    plan['keep'] = args.keep
    (inside / 'plan.json').write_text(json.dumps(plan, ensure_ascii=False, indent=1), encoding='utf-8')
    command = f'powershell.exe -NoProfile -ExecutionPolicy Bypass -File {INSIDE_IN}\\sandbox-admx.ps1 -Plan {INSIDE_IN}\\plan.json -Out {INSIDE_OUT}'
    config = f"""<Configuration>
  <Networking>Disable</Networking>
  <VGpu>Disable</VGpu>
  <MappedFolders>
    <MappedFolder><HostFolder>{inside}</HostFolder><SandboxFolder>{INSIDE_IN}</SandboxFolder><ReadOnly>true</ReadOnly></MappedFolder>
    <MappedFolder><HostFolder>{outside}</HostFolder><SandboxFolder>{INSIDE_OUT}</SandboxFolder><ReadOnly>false</ReadOnly></MappedFolder>
  </MappedFolders>
  <LogonCommand><Command>{command}</Command></LogonCommand>
</Configuration>
"""
    wsb_file = WORK / 'sandbox-admx.wsb'
    wsb_file.write_text(config, encoding='utf-8')
    subprocess.Popen(['WindowsSandbox.exe', str(wsb_file)])
    print('Sandbox started; do not type into it while the test runs.', flush=True)
    result_path, done = outside / 'result.json', outside / 'done.txt'
    deadline = time.monotonic() + args.timeout
    # The result is saved as the steps go; the run is over once done.txt is there.
    while not done.exists():
        if time.monotonic() > deadline:
            raise SystemExit(f'No result within {args.timeout} s; see {outside}')
        time.sleep(3)
    time.sleep(1)
    result = json.loads(result_path.read_text(encoding='utf-8-sig'))
    if not args.keep:
        for sid in running_sandboxes():
            wsb('stop', '--id', sid)
    report(result, plan)


def report(result, plan):
    failures = []
    def check(ok, what):
        print(('PASS: ' if ok else 'FAIL: ') + what, flush=True)
        if not ok:
            failures.append(what)
    lang = result.get('language', '')
    check(lang in ('ja-JP', 'en-US'), f'gpedit found its tree ({lang or "none"})')
    check(not result.get('fatal'), 'the script ran to the end' + (f" ({result.get('fatal')})" if result.get('fatal') else ''))
    errors = result.get('load_errors') or []
    check(not errors, 'gpedit loaded the templates without an error dialog' + (f': {errors}' if errors else ''))
    shown = result.get('listing') or []
    for entry, seen in zip(plan['listing'], shown):
        wanted = sorted(n[lang] for n in entry['policies']) if lang else []
        got = sorted(seen.get('items') or [])
        check(wanted == got, f"{' > '.join(p[lang] for p in entry['path']) if lang else entry['path']}: {len(got)}/{len(wanted)} policies listed")
    total = sum(len(e['policies']) for e in plan['listing'])
    check(sum(len(s.get('items') or []) for s in shown) == total == 87, f'all {total} policies are listed')
    for (policy, state, _, expect), step in zip(STEPS, result.get('steps') or []):
        if step.get('error'):
            check(False, f'{policy} {state}: {step["error"]}')
            continue
        snap = step.get('registry') or {}
        for name, want in expect.items():
            have = snap.get(name)
            if want is None:
                ok = have is None
                check(ok, f'{policy} {state}: {name} absent' + ('' if ok else f' (is {have})'))
            else:
                ok = have is not None and have.get('kind') == want[0] and str(have.get('data')) == want[1]
                check(ok, f'{policy} {state}: {name} = {want[0]} {want[1]}' + ('' if ok else f' (is {have})'))
        if policy == 'button_1' and state == 'Enabled':
            for f in ('transfer', 'pickup'):
                have = snap.get(f'button_1_{f}')
                check(have is None or have.get('data') == '', f'button_1 Enabled: button_1_{f} empty or absent')
    if len(result.get('steps') or []) < len(STEPS):
        check(False, f"only {len(result.get('steps') or [])} of {len(STEPS)} steps ran")
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(result, ensure_ascii=False, indent=1), encoding='utf-8')
    print(f"{'FAIL' if failures else 'PASS'}: {len(failures)} failure(s); details in {REPORT.relative_to(ROOT)} and {(WORK / 'out').relative_to(ROOT)}")
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
