"""Holds docs/admx against the settings ksip.exe reads: every value the templates write is one KSIP has, of the type KSIP writes, with the default KSIP uses."""
import argparse, sys
from admx_model import ACCOUNT, Profile, exe_path, policies, setting

# Settings that differ by machine or person and are left out of the templates
# on purpose. Anything else KSIP has and the templates lack is a gap.
LEFT_OUT = {'microphone', 'speaker', 'network_adapter'}
# Where the template's dialog shows another starting value than KSIP's
# default, and why that is right.
DEFAULT_EXCEPTIONS = {
    # KSIP's default -1 (never) is what disabling the policy gives; the dialog
    # offers ten seconds as a starting point for enabling it.
    'tray_after_call',
}
# Values the template writes that mean the same as KSIP's default.
SAME_MEANING = {('transport', 'udp'): ''}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--folder', help='a staged build folder holding ksip.exe; default release/')
    args = parser.parse_args()
    failures = []
    def check(ok, what):
        print(('PASS: ' if ok else 'FAIL: ') + what, flush=True)
        if not ok:
            failures.append(what)

    with Profile('admx', exe_path(args.folder)) as profile:
        defaults = profile.export()
    stored = defaults['stored']
    templates = policies()
    written = {name: v for p in templates for name, v in p['values'].items()}

    check(all(p['class'] == 'User' and p['key'] == r'Software\KashiharaCity\ksip' for p in templates), 'every policy is per user, in the key KSIP reads')
    check(len(written) == sum(len(p['values']) for p in templates), 'no value is written by two policies')
    unknown = sorted(set(written) - set(stored) - set(ACCOUNT))
    check(not unknown, 'every value the templates write is one KSIP reads' + (f': not {unknown}' if unknown else ''))
    missing = sorted(set(stored) - set(written) - LEFT_OUT)
    check(not missing, 'every setting KSIP has is in the templates, but the ones left out on purpose' + (f': missing {missing}' if missing else ''))

    for name, v in sorted(written.items()):
        # The registry type: numbers and switches as REG_DWORD, text and
        # choices as REG_SZ, as KSIP itself writes them.
        want = 'REG_DWORD' if v['kind'] in ('flag', 'decimal') else 'REG_SZ'
        have = 'REG_DWORD' if name == 'port' else 'REG_SZ' if name == 'server' else stored[name]['type']
        if v['kind'] == 'decimal' and v['text']:
            want = 'REG_SZ'
        check(want == have, f'{name}: the template writes {want}, KSIP writes {have}')
        if v['kind'] == 'flag':
            check((v['on'], v['off']) == (1, 0), f'{name}: enabled writes 1 and disabled 0')
        elif v['kind'] == 'decimal' and name not in DEFAULT_EXCEPTIONS and v['default'] is not None:
            check(v['default'] == setting(defaults, name), f"{name}: the dialog starts at {v['default']}, KSIP's default is {setting(defaults, name)}")
        elif v['kind'] == 'enum':
            shown = v['items'][v['default']]
            have = setting(defaults, name)
            check(SAME_MEANING.get((name, shown), shown) == have, f"{name}: the dialog starts at '{shown}', KSIP's default is '{have}'")
        if v['kind'] == 'decimal':
            check(v['min'] <= v['max'], f"{name}: the range {v['min']}..{v['max']} is a range")
    print(f"{'FAIL' if failures else 'PASS'}: {len(failures)} failure(s)")
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
