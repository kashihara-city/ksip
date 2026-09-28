"""Validate a JSON document against a JSON Schema (draft-07), as much of it as the CycloneDX 1.6 schemas use; standard library only."""
import json, re
from pathlib import Path

# Keywords that only describe (or that draft-07 leaves to the validator to
# assert, as `format`): read past.
ANNOTATIONS = {'$schema', '$id', '$comment', 'title', 'description', 'default', 'examples', 'deprecated', 'meta:enum', 'format', 'definitions', 'readOnly'}
CHECKED = {'$ref', 'type', 'enum', 'const', 'properties', 'required', 'additionalProperties', 'items', 'additionalItems', 'minItems',
           'maxItems', 'uniqueItems', 'minLength', 'maxLength', 'pattern', 'minimum', 'maximum', 'allOf', 'anyOf', 'oneOf', 'if', 'then', 'else'}


class Schemas:
    """The schemas of a folder, found by their $id or file name, so that a
    $ref to another file resolves as it does on the web."""
    def __init__(self, folder):
        self.docs = {}
        for path in Path(folder).glob('*.schema.json'):
            doc = json.loads(path.read_text(encoding='utf-8'))
            self.docs[path.name] = doc
            if doc.get('$id'):
                self.docs[doc['$id']] = doc
        unknown = set()
        for doc in {id(d): d for d in self.docs.values()}.values():
            self._keywords(doc, unknown)
        # A keyword this does not check would pass every document: refuse.
        assert not unknown, f'schema keywords this validator does not know: {sorted(unknown)}'

    def _keywords(self, schema, unknown):
        if isinstance(schema, dict):
            for key, value in schema.items():
                if key in ('properties', 'definitions'):
                    for sub in value.values():
                        self._keywords(sub, unknown)
                elif key in ('enum', 'const', 'examples', 'default', 'meta:enum'):
                    continue
                else:
                    if key not in CHECKED and key not in ANNOTATIONS:
                        unknown.add(key)
                    self._keywords(value, unknown)
        elif isinstance(schema, list):
            for sub in schema:
                self._keywords(sub, unknown)

    def resolve(self, ref, base):
        target, _, pointer = ref.partition('#')
        doc = self.docs[target.rsplit('/', 1)[-1]] if target else base
        node = doc
        for part in [p for p in pointer.split('/') if p]:
            node = node[part.replace('~1', '/').replace('~0', '~')]
        return node, doc

    def errors(self, instance, schema, base=None, path='$'):
        """Every way the instance fails the schema, as 'path: why'."""
        base = base or schema
        if schema is True or schema == {}:
            return []
        if schema is False:
            return [f'{path}: not allowed']
        out = []
        if '$ref' in schema:
            # draft-07: $ref stands alone; its siblings are ignored.
            target, doc = self.resolve(schema['$ref'], base)
            return self.errors(instance, target, doc, path)
        t = schema.get('type')
        if t is not None:
            types = t if isinstance(t, list) else [t]
            if not any(_is(instance, x) for x in types):
                return [f'{path}: is not {"/".join(types)}']
        if 'enum' in schema and instance not in schema['enum']:
            out.append(f'{path}: {instance!r} is not one of the allowed values')
        if 'const' in schema and instance != schema['const']:
            out.append(f'{path}: is not {schema["const"]!r}')
        if isinstance(instance, str):
            if len(instance) < schema.get('minLength', 0) or len(instance) > schema.get('maxLength', 1 << 60):
                out.append(f'{path}: length out of range')
            if 'pattern' in schema and not re.search(schema['pattern'], instance):
                out.append(f'{path}: {instance!r} does not match {schema["pattern"]}')
        if isinstance(instance, (int, float)) and not isinstance(instance, bool):
            if instance < schema.get('minimum', float('-inf')) or instance > schema.get('maximum', float('inf')):
                out.append(f'{path}: {instance} out of range')
        if isinstance(instance, dict):
            for name in schema.get('required', []):
                if name not in instance:
                    out.append(f'{path}: {name} is required')
            props = schema.get('properties', {})
            for name, value in instance.items():
                if name in props:
                    out += self.errors(value, props[name], base, f'{path}.{name}')
                elif 'additionalProperties' in schema:
                    extra = schema['additionalProperties']
                    if extra is False:
                        out.append(f'{path}: {name} is not a known property')
                    elif isinstance(extra, dict):
                        out += self.errors(value, extra, base, f'{path}.{name}')
        if isinstance(instance, list):
            if len(instance) < schema.get('minItems', 0) or len(instance) > schema.get('maxItems', 1 << 60):
                out.append(f'{path}: number of items out of range')
            if schema.get('uniqueItems'):
                seen = [json.dumps(x, sort_keys=True) for x in instance]
                if len(seen) != len(set(seen)):
                    out.append(f'{path}: items are not unique')
            items = schema.get('items')
            if isinstance(items, dict):
                for i, value in enumerate(instance):
                    out += self.errors(value, items, base, f'{path}[{i}]')
            elif isinstance(items, list):
                for i, value in enumerate(instance):
                    sub = items[i] if i < len(items) else schema.get('additionalItems', True)
                    out += self.errors(value, sub, base, f'{path}[{i}]')
        for sub in schema.get('allOf', []):
            out += self.errors(instance, sub, base, path)
        if 'anyOf' in schema and not any(not self.errors(instance, sub, base, path) for sub in schema['anyOf']):
            out.append(f'{path}: matches none of anyOf')
        if 'oneOf' in schema:
            matched = sum(1 for sub in schema['oneOf'] if not self.errors(instance, sub, base, path))
            if matched != 1:
                out.append(f'{path}: matches {matched} of oneOf, not exactly one')
        if 'if' in schema:
            branch = schema.get('then') if not self.errors(instance, schema['if'], base, path) else schema.get('else')
            if branch is not None:
                out += self.errors(instance, branch, base, path)
        return out


def _is(value, kind):
    return {
        'object': isinstance(value, dict),
        'array': isinstance(value, list),
        'string': isinstance(value, str),
        'boolean': isinstance(value, bool),
        'null': value is None,
        'integer': isinstance(value, int) and not isinstance(value, bool),
        'number': isinstance(value, (int, float)) and not isinstance(value, bool),
    }[kind]
