"""Reads docs/admx into plain data, and runs ksip.exe --export-settings against a throwaway test profile, for admx.py and app-settings.py."""
import json, os, subprocess, tempfile, winreg
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ADMX = ROOT / 'docs/admx/ksip.admx'
NS = '{http://schemas.microsoft.com/GroupPolicy/2006/07/PolicyDefinitions}'
# The account's values are the store's, not the settings'.
ACCOUNT = ('server', 'port')


def policies(path=None):
    """Every policy: its name, and each registry value it writes with what the
    template says about it (from `path`, docs/admx/ksip.admx by default). A
    switch is {'kind': 'flag', 'on': 1, 'off': 0}, with None for a value the
    template does not give (never False, which would compare equal to 0);
    the elements carry their kind (decimal, text, enum), limits, the default
    the dialog shows, and an enum's values in order."""
    path = Path(path) if path else ADMX
    admx = ET.parse(path).getroot()
    adml = ET.parse(path.parent / 'ja-JP' / 'ksip.adml').getroot()
    shown = {}
    for pres in adml.iter(NS + 'presentation'):
        for control in pres:
            shown[control.get('refId')] = control
    out = []
    for pol in admx.iter(NS + 'policy'):
        values = {}
        if pol.get('valueName'):
            def number(tag):
                found = pol.find(NS + tag + '/' + NS + 'decimal')
                text = found.get('value', '') if found is not None else ''
                return int(text) if text.isdigit() else None
            values[pol.get('valueName')] = {'kind': 'flag', 'on': number('enabledValue'), 'off': number('disabledValue')}
        elements = pol.find(NS + 'elements')
        for el in (elements if elements is not None else []):
            kind = el.tag[len(NS):]
            v = {'kind': kind, 'required': el.get('required') == 'true'}
            control = shown.get(el.get('id'))
            if kind == 'decimal':
                v.update(min=int(el.get('minValue', 0)), max=int(el.get('maxValue', 4294967295)), text=el.get('storeAsText') == 'true',
                         default=int(control.get('defaultValue')) if control is not None and control.get('defaultValue') else None)
            elif kind == 'text':
                v.update(max_length=int(el.get('maxLength', 1023)))
            elif kind == 'enum':
                v.update(items=[item.find(NS + 'value')[0].text or '' for item in el.iter(NS + 'item')],
                         default=int(control.get('defaultItem', 0)) if control is not None else 0)
            values[el.get('valueName')] = v
        out.append({'name': pol.get('name'), 'key': pol.get('key'), 'class': pol.get('class'), 'values': values})
    return out


def exe_path(folder=None):
    exe = Path(folder) / 'ksip.exe' if folder else ROOT / 'release/ksip.exe'
    if not exe.is_file():
        raise SystemExit(f'Build the app first: {exe}')
    return exe


class Profile:
    """A throwaway test profile: its registry key, emptied on the way in and
    out, and the export of what KSIP reads from it."""
    def __init__(self, name, exe):
        self.name = f'test-{name}-{os.getpid()}'
        self.key = rf'Software\KashiharaCity\ksip\Test\{self.name}'
        self.exe = exe
        self.clear()

    def clear(self):
        try:
            winreg.DeleteKey(winreg.HKEY_CURRENT_USER, self.key)
        except FileNotFoundError:
            pass

    def write(self, values):
        """name -> int (REG_DWORD, as a policy writes a number or a switch) or
        str (REG_SZ) or bytes (REG_BINARY, which no policy writes)."""
        with winreg.CreateKey(winreg.HKEY_CURRENT_USER, self.key) as key:
            for name, value in values.items():
                if isinstance(value, bool) or isinstance(value, int):
                    winreg.SetValueEx(key, name, 0, winreg.REG_DWORD, int(value) & 0xFFFFFFFF)
                elif isinstance(value, bytes):
                    winreg.SetValueEx(key, name, 0, winreg.REG_BINARY, value)
                else:
                    winreg.SetValueEx(key, name, 0, winreg.REG_SZ, value)

    def export(self):
        with tempfile.TemporaryDirectory() as folder:
            out = Path(folder) / 'settings.json'
            env = dict(os.environ, KSIP_TEST_PROFILE=self.name)
            code = subprocess.run([str(self.exe), '--export-settings', str(out)], env=env, timeout=30).returncode
            if code != 0:
                raise RuntimeError(f'ksip.exe --export-settings ended with {code}')
            return json.loads(out.read_text(encoding='utf-8'))

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.clear()


def setting(export, name):
    """What KSIP read for a registry value name."""
    if name in ACCOUNT:
        return export['account'][name]
    if name.startswith('button_'):
        _, n, field = name.split('_', 2)
        return export['settings']['buttons'][int(n) - 1][field]
    return export['settings'][name]
