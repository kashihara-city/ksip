"""Holds docs/admx against the settings ksip.exe reads: every value the templates write is one KSIP has, of the type KSIP writes, with the default KSIP uses; and makes sure these checks catch templates broken in the XML itself."""
import argparse, re, shutil, sys, tempfile
from pathlib import Path
from admx_model import ACCOUNT, ADMX, Profile, exe_path, policies, setting

# Settings that differ by machine or person and are left out of the templates
# on purpose. Anything else KSIP has and the templates lack is a gap.
LEFT_OUT = {'microphone', 'speaker', 'network_adapter',
            # The person's alone: adjusted where the phone is used, never fixed.
            'microphone_gain', 'speaker_gain', 'aec_delay_ms',
            # As the devices in use allow, which are the person's.
            'raw_microphone', 'raw_speaker'}
# Where the template's dialog shows another starting value than KSIP's
# default, and why that is right.
DEFAULT_EXCEPTIONS = {
    # KSIP's default -1 (never) is what disabling the policy gives; the dialog
    # offers ten seconds as a starting point for enabling it.
    'tray_after_call',
    # KSIP's default 0 is ports Windows picks, which a policy does not fix: it
    # fixes a port of KSIP's own, and the dialog offers the usual one to start.
    'sip_port', 'rtp_port',
}
# Values the template writes that mean the same as KSIP's default.
SAME_MEANING = {('transport', 'udp'): ''}


def problems(templates, defaults):
    """What is wrong with the templates against KSIP's own export, as
    (passed, what) pairs, every check listed."""
    out = []
    def check(ok, what):
        out.append((bool(ok), what))
    stored = defaults['stored']
    written = {name: v for p in templates for name, v in p['values'].items()}
    check(all(p['class'] == 'User' and p['key'] == r'Software\Policies\KashiharaCity\ksip' for p in templates), 'every policy is per user, in the policy key KSIP reads')
    check(len(written) == sum(len(p['values']) for p in templates), 'no value is written by two policies')
    unknown = sorted(set(written) - set(stored) - set(ACCOUNT))
    check(not unknown, 'every value the templates write is one KSIP reads' + (f': not {unknown}' if unknown else ''))
    missing = sorted(set(stored) - set(written) - LEFT_OUT)
    check(not missing, 'every setting KSIP has is in the templates, but the ones left out on purpose' + (f': missing {missing}' if missing else ''))
    for name, v in sorted(written.items()):
        known = name in stored or name in ACCOUNT
        # The registry type: numbers and switches as REG_DWORD, text and
        # choices as REG_SZ, as KSIP itself writes them.
        want = 'REG_DWORD' if v['kind'] in ('flag', 'decimal') else 'REG_SZ'
        have = 'REG_DWORD' if name == 'port' else 'REG_SZ' if name == 'server' else stored.get(name, {}).get('type')
        if v['kind'] == 'decimal' and v['text']:
            want = 'REG_SZ'
        check(want == have, f'{name}: the template writes {want}, KSIP writes {have}')
        if v['kind'] == 'flag':
            # Each of the two values has to be there, as a number: a missing
            # one is not a 0.
            check(v['on'] == 1 and v['off'] == 0 and type(v['on']) is int and type(v['off']) is int,
                  f"{name}: enabled writes 1 and disabled 0 (the template gives {v['on']} and {v['off']})")
        elif v['kind'] == 'decimal' and known and name not in DEFAULT_EXCEPTIONS and v['default'] is not None:
            check(v['default'] == setting(defaults, name), f"{name}: the dialog starts at {v['default']}, KSIP's default is {setting(defaults, name)}")
        elif v['kind'] == 'enum' and known:
            shown = v['items'][v['default']]
            here = setting(defaults, name)
            check(SAME_MEANING.get((name, shown), shown) == here, f"{name}: the dialog starts at '{shown}', KSIP's default is '{here}'")
        if v['kind'] == 'decimal':
            check(v['min'] <= v['max'], f"{name}: the range {v['min']}..{v['max']} is a range")
    return out


# Templates broken in the XML, each of which the checks must fail on.
BREAKAGES = [
    ('a switch without its disabled value', lambda x: re.sub(r'(<policy name="agc".*?)<disabledValue>.*?</disabledValue>', r'\1', x, count=1, flags=re.S)),
    ('a switch whose disabled value is text', lambda x: re.sub(r'(<policy name="agc".*?<disabledValue>\s*)<decimal value="0" />', r'\1<string>0</string>', x, count=1, flags=re.S)),
    ('a number written as text', lambda x: x.replace('<decimal id="sip_port" valueName="sip_port"', '<decimal id="sip_port" valueName="sip_port" storeAsText="true"', 1)),
    ('a setting with no policy', lambda x: re.sub(r'<policy name="high_pass".*?</policy>', '', x, count=1, flags=re.S)),
    ('a value KSIP does not have', lambda x: x.replace('valueName="register_interval"', 'valueName="register_every"', 2)),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--folder', help='a staged build folder holding ksip.exe; default release/')
    args = parser.parse_args()
    failures = []
    def report(ok, what):
        print(('PASS: ' if ok else 'FAIL: ') + what, flush=True)
        if not ok:
            failures.append(what)

    with Profile('admx', exe_path(args.folder)) as profile:
        defaults = profile.export()
    for ok, what in problems(policies(), defaults):
        report(ok, what)
    # The checks themselves: each broken copy of the XML fails somewhere.
    text = ADMX.read_text(encoding='utf-8')
    for label, breaking in BREAKAGES:
        broken = breaking(text)
        with tempfile.TemporaryDirectory() as folder:
            copy = Path(folder) / 'ksip.admx'
            shutil.copytree(ADMX.parent / 'ja-JP', Path(folder) / 'ja-JP')
            copy.write_text(broken, encoding='utf-8')
            failed = [what for ok, what in problems(policies(copy), defaults) if not ok]
        report(broken != text and failed, f'the checks catch {label}' + (f' ({failed[0]})' if failed else ''))
    print(f"{'FAIL' if failures else 'PASS'}: {len(failures)} failure(s)")
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
