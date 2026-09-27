"""Puts in a test profile what the policy templates would write, in the types they write, and checks through ksip.exe --export-settings that KSIP reads each value as meant, that every choice and limit the templates allow passes KSIP's checks, and that values no policy could write are named as unreadable."""
import argparse, sys
from admx_model import Profile, exe_path, policies, setting

# Text that passes KSIP's checks, for the text values of the templates.
SAMPLES = {
    'server': 'pbx.example',
    'ca_file': r'C:\ca\pbx.pem',
    'codecs': 'PCMU,opus',
    'shortcut_window': 'CONTROL+ALT+F9',
    'shortcut_call': 'CONTROL+ALT+F10',
}
# A number that goes with each button function, as KSIP checks it.
NUMBER_FOR_KIND = {'': '', 'transfer': '9001', 'dial': '1001', 'speed': '06-1234-5678', 'park': '701',
                   'open': 'https://pbx.example/extensions', 'dnd': '', 'mwi': '*97'}
# Choices that KSIP takes only together with another value: SDES and OSRTP
# carry their keys in the signalling, which has to be TLS then.
NEEDS = {('media_encryption', 'sdes'): {'transport': 'tls'}, ('media_encryption', 'osrtp'): {'transport': 'tls'}}


def sample(name, n=None):
    if name in SAMPLES:
        return SAMPLES[name]
    if name.startswith('sound_'):
        return rf'C:\sounds\{name}.wav'
    if name.startswith('button_'):
        field = name.split('_', 2)[2]
        return {'title': f'B{n}', 'number': f'{1000 + n}', 'transfer': '', 'pickup': ''}[field]
    raise KeyError(name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--folder', help='a staged build folder holding ksip.exe; default release/')
    args = parser.parse_args()
    exe = exe_path(args.folder)
    failures = []
    def check(ok, what):
        print(('PASS: ' if ok else 'FAIL: ') + what, flush=True)
        if not ok:
            failures.append(what)
    def expect(export, values, what):
        """Every value read back as written: a switch as on or off, a number as
        that number, text and choices as written."""
        wrong = []
        for name, value in values.items():
            have = setting(export, name)
            want = bool(value) if isinstance(have, bool) else value
            if have != want:
                wrong.append(f'{name}={have!r} (wrote {value!r})')
        # Nobody signs in to the test profile, so what is asked of it is that
        # the settings and the account's address pass their checks, not that
        # a connect would go ahead.
        ok = export['settings_valid'] and export['account_valid']
        refused = '; '.join(e for e in (export['settings_error'], export['account_error']) if e)
        check(not wrong and not export['unreadable'] and ok,
              what + ('' if not wrong else f': {wrong[:4]}') + ('' if ok else f' (refused: {refused})'))

    templates = policies()
    with Profile('settings', exe) as profile:
        defaults = profile.export()
        check(defaults['settings_valid'] and defaults['account_valid'] and not defaults['unreadable'], 'with nothing stored KSIP has its defaults, and they pass')
        check(not defaults['valid'] and not defaults['account']['signed_in'], 'nobody signed in: the export says a connect would not go ahead')

        # Every policy enabled at once, each with a value other than KSIP's
        # default: the largest number, the last choice, a sample text; a switch
        # set to what the default is not.
        values = {}
        for p in templates:
            for name, v in p['values'].items():
                if v['kind'] == 'flag':
                    values[name] = v['off'] if setting(defaults, name) else v['on']
                elif v['kind'] == 'decimal':
                    values[name] = v['max']
                elif v['kind'] == 'enum' and not name.startswith('button_'):
                    values[name] = v['items'][-1]
                elif name.startswith('button_'):
                    n = int(name.split('_')[1])
                    field = name.split('_', 2)[2]
                    values[name] = 'dial' if field == 'kind' else sample(name, n)
                else:
                    values[name] = sample(name)
        profile.clear()
        profile.write(values)
        expect(profile.export(), values, f'all {len(templates)} policies enabled at once are read as written, and pass')

        # Every number at the smallest the template allows, alone.
        for p in templates:
            for name, v in p['values'].items():
                if v['kind'] == 'decimal':
                    profile.clear()
                    profile.write({name: v['min']})
                    expect(profile.export(), {name: v['min']}, f"{name} at its minimum {v['min']} is read and passes")

        # Every choice of every list, alone (with what a choice needs beside it).
        for p in templates:
            for name, v in p['values'].items():
                if v['kind'] != 'enum':
                    continue
                for item in v['items']:
                    written = {name: item}
                    written.update(NEEDS.get((name, item), {}))
                    if name.startswith('button_'):
                        n = name.split('_')[1]
                        written[f'button_{n}_number'] = NUMBER_FOR_KIND[item]
                    profile.clear()
                    profile.write(written)
                    expect(profile.export(), written, f"{name} = '{item}' is read and passes")

        # One below each number's minimum is refused, the account's port included:
        # a template whose range reached below what KSIP takes would let a
        # policy write values a connect refuses.
        for p in templates:
            for name, v in p['values'].items():
                if v['kind'] == 'decimal' and v['min'] > 0:
                    profile.clear()
                    profile.write({name: v['min'] - 1})
                    below = profile.export()
                    check(not (below['settings_valid'] and below['account_valid']), f"{name} below its minimum ({v['min'] - 1}) is refused")

        # Values no policy writes are named, and the settings are refused,
        # the account's values as well.
        profile.clear()
        profile.write({'aec': b'\x01\x02', 'agc': 'maybe', 'rtp_port': 70000, 'sip_port': 'often', 'server': b'\x01', 'port': 'x'})
        bad = profile.export()
        check(sorted(bad['unreadable']) == ['aec', 'agc', 'port', 'rtp_port', 'server', 'sip_port'] and not bad['settings_valid'] and not bad['account_valid'],
              f"values of the wrong type or out of reach are named and refused, the account's too ({bad['unreadable']})")
        # Written by hand as text, a switch and a number still read.
        profile.clear()
        profile.write({'aec': 'off', 'sip_port': ' 5070 ', 'tray_after_call': '-1'})
        by_hand = profile.export()
        check(by_hand['settings_valid'] and setting(by_hand, 'aec') is False and setting(by_hand, 'sip_port') == 5070 and setting(by_hand, 'tray_after_call') == -1,
              'a switch and numbers written as text by hand are read')
    print(f"{'FAIL' if failures else 'PASS'}: {len(failures)} failure(s)")
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
